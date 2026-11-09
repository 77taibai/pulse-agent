//! Windows service integration: SCM installation plus a hand-written service
//! dispatcher (no `windows-sys` crate).
//!
//! `run` first tries to run as a Windows service; if the process was not started
//! by the Service Control Manager (`StartServiceCtrlDispatcherW` fails with
//! error 1063) it falls back to running in the console.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use crate::agent::Agent;
use crate::cli::{Config, BIN_NAME};
use crate::log;

use super::{ServiceState, StatusReport};

type Handle = *mut c_void;
type Hkey = isize;

const HKEY_LOCAL_MACHINE: Hkey = 0x8000_0002u32 as i32 as isize;
const KEY_QUERY_VALUE: u32 = 0x0001;
const KEY_SET_VALUE: u32 = 0x0002;
const REG_SZ: u32 = 1;
const REG_EXPAND_SZ: u32 = 2;
const REG_MULTI_SZ: u32 = 7;

const HWND_BROADCAST: Handle = 0xFFFF_usize as Handle;
const WM_SETTINGCHANGE: u32 = 0x001A;
const SMTO_ABORTIFHUNG: u32 = 0x0002;

const SYSTEM_ENV_KEY: &str = "SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment";

const SC_MANAGER_CONNECT: u32 = 0x0001;
const SC_MANAGER_ALL_ACCESS: u32 = 0xF003F;
const SERVICE_ALL_ACCESS: u32 = 0xF01FF;
const SERVICE_START: u32 = 0x0010;
const SERVICE_STOP: u32 = 0x0020;
const SERVICE_QUERY_STATUS: u32 = 0x0004;
const DELETE_ACCESS: u32 = 0x0001_0000;

const SERVICE_WIN32_OWN_PROCESS: u32 = 0x0000_0010;
const SERVICE_AUTO_START: u32 = 0x0000_0002;
const SERVICE_ERROR_NORMAL: u32 = 0x0000_0001;

const SERVICE_CONTROL_STOP: u32 = 0x0000_0001;
const SERVICE_CONTROL_INTERROGATE: u32 = 0x0000_0004;
const SERVICE_CONTROL_SHUTDOWN: u32 = 0x0000_0005;

const SERVICE_STOPPED: u32 = 0x0000_0001;
const SERVICE_START_PENDING: u32 = 0x0000_0002;
const SERVICE_STOP_PENDING: u32 = 0x0000_0003;
const SERVICE_RUNNING: u32 = 0x0000_0004;
const SERVICE_CONTINUE_PENDING: u32 = 0x0000_0005;
const SERVICE_PAUSE_PENDING: u32 = 0x0000_0006;
const SERVICE_PAUSED: u32 = 0x0000_0007;

const SERVICE_ACCEPT_STOP: u32 = 0x0000_0001;
const SERVICE_ACCEPT_SHUTDOWN: u32 = 0x0000_0004;

const ERROR_FAILED_SERVICE_CONTROLLER_CONNECT: u32 = 1063;
const ERROR_SERVICE_ALREADY_RUNNING: u32 = 1056;
const ERROR_SERVICE_NOT_ACTIVE: u32 = 1062;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ServiceStatus {
    service_type: u32,
    current_state: u32,
    controls_accepted: u32,
    win32_exit_code: u32,
    service_specific_exit_code: u32,
    checkpoint: u32,
    wait_hint: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ServiceStatusProcess {
    service_type: u32,
    current_state: u32,
    controls_accepted: u32,
    win32_exit_code: u32,
    service_specific_exit_code: u32,
    checkpoint: u32,
    wait_hint: u32,
    process_id: u32,
    service_flags: u32,
}

type ServiceMainFn = unsafe extern "system" fn(u32, *mut *mut u16);
type ServiceControlFn = unsafe extern "system" fn(u32, u32, *mut c_void, *mut c_void) -> u32;

#[repr(C)]
struct ServiceTableEntry {
    name: *const u16,
    proc: Option<ServiceMainFn>,
}

#[link(name = "advapi32")]
extern "system" {
    fn OpenSCManagerW(machine: *const u16, database: *const u16, access: u32) -> Handle;
    #[allow(clippy::too_many_arguments)]
    fn CreateServiceW(
        scm: Handle,
        name: *const u16,
        display: *const u16,
        desired_access: u32,
        service_type: u32,
        start_type: u32,
        error_control: u32,
        bin_path: *const u16,
        load_order_group: *const u16,
        tag_id: *mut u32,
        dependencies: *const u16,
        account: *const u16,
        password: *const u16,
    ) -> Handle;
    fn OpenServiceW(scm: Handle, name: *const u16, access: u32) -> Handle;
    fn StartServiceW(service: Handle, argc: u32, argv: *const *const u16) -> i32;
    fn ControlService(service: Handle, control: u32, status: *mut ServiceStatus) -> i32;
    fn QueryServiceStatusEx(
        service: Handle,
        level: u32,
        buffer: *mut u8,
        size: u32,
        needed: *mut u32,
    ) -> i32;
    fn DeleteService(service: Handle) -> i32;
    fn CloseServiceHandle(handle: Handle) -> i32;

    fn StartServiceCtrlDispatcherW(table: *const ServiceTableEntry) -> i32;
    fn RegisterServiceCtrlHandlerExW(
        name: *const u16,
        handler: Option<ServiceControlFn>,
        context: *mut c_void,
    ) -> Handle;
    fn SetServiceStatus(handle: Handle, status: *mut ServiceStatus) -> i32;

    fn RegOpenKeyExW(
        hkey: Hkey,
        sub_key: *const u16,
        options: u32,
        sam: u32,
        out: *mut Hkey,
    ) -> i32;
    fn RegQueryValueExW(
        hkey: Hkey,
        value_name: *const u16,
        reserved: *mut u32,
        value_type: *mut u32,
        data: *mut u8,
        data_len: *mut u32,
    ) -> i32;
    fn RegSetValueExW(
        hkey: Hkey,
        value_name: *const u16,
        reserved: u32,
        value_type: u32,
        data: *const u8,
        data_len: u32,
    ) -> i32;
    fn RegCloseKey(hkey: Hkey) -> i32;
}

#[link(name = "user32")]
extern "system" {
    fn SendMessageTimeoutW(
        hwnd: Handle,
        msg: u32,
        wparam: usize,
        lparam: isize,
        flags: u32,
        timeout: u32,
        result: *mut usize,
    ) -> isize;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetLastError() -> u32;
    fn SetConsoleCtrlHandler(
        handler: Option<unsafe extern "system" fn(u32) -> i32>,
        add: i32,
    ) -> i32;
}

static STOP: AtomicBool = AtomicBool::new(false);
static CONSOLE_STOP: AtomicBool = AtomicBool::new(false);
static STATUS_HANDLE: AtomicUsize = AtomicUsize::new(0);
static CONFIG: OnceLock<Config> = OnceLock::new();
static SERVICE_NAME_W: OnceLock<Vec<u16>> = OnceLock::new();

// ---------------------------------------------------------------------------
// Foreground / service entry point
// ---------------------------------------------------------------------------

pub fn run(cfg: &Config) -> Result<(), String> {
    let _ = CONFIG.set(cfg.clone());
    let _ = SERVICE_NAME_W.set(to_wide(&cfg.service_name));

    match try_service_dispatch()? {
        true => Ok(()),
        false => {
            log::info("未作为 Windows 服务启动，进入前台运行模式（Ctrl+C 结束）");
            unsafe {
                SetConsoleCtrlHandler(Some(console_ctrl_handler), 1);
            }
            let agent = Agent::new(cfg)?;
            agent.run(Some(&CONSOLE_STOP));
            Ok(())
        }
    }
}

unsafe extern "system" fn console_ctrl_handler(_ctrl_type: u32) -> i32 {
    CONSOLE_STOP.store(true, Ordering::SeqCst);
    1
}

fn try_service_dispatch() -> Result<bool, String> {
    let name = name_wide();
    let table = Box::new([
        ServiceTableEntry {
            name: name.as_ptr(),
            proc: Some(service_main),
        },
        ServiceTableEntry {
            name: std::ptr::null(),
            proc: None,
        },
    ]);

    // The dispatcher blocks for the lifetime of the service, so the table must
    // outlive this stack frame: leak it deliberately.
    let table_ptr = Box::into_raw(table) as *const ServiceTableEntry;
    let ok = unsafe { StartServiceCtrlDispatcherW(table_ptr) };

    if ok == 0 {
        let err = unsafe { GetLastError() };
        if err == ERROR_FAILED_SERVICE_CONTROLLER_CONNECT {
            return Ok(false);
        }
        return Err(format!("启动服务派发器失败，错误码 {}", err));
    }

    Ok(true)
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut *mut u16) {
    let name = name_wide();
    let handle = RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(control_handler), std::ptr::null_mut());
    if handle.is_null() {
        return;
    }
    STATUS_HANDLE.store(handle as usize, Ordering::SeqCst);

    report_status(SERVICE_START_PENDING, 0);
    report_status(SERVICE_RUNNING, SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN);

    match CONFIG.get() {
        Some(cfg) => match Agent::new(cfg) {
            Ok(agent) => agent.run(Some(&STOP)),
            Err(e) => log::error(&format!("服务启动失败: {}", e)),
        },
        None => log::error("服务缺少运行配置"),
    }

    report_status(SERVICE_STOPPED, 0);
}

unsafe extern "system" fn control_handler(
    control: u32,
    _event_type: u32,
    _event_data: *mut c_void,
    _context: *mut c_void,
) -> u32 {
    match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            STOP.store(true, Ordering::SeqCst);
            report_status(SERVICE_STOP_PENDING, 0);
        }
        SERVICE_CONTROL_INTERROGATE => {
            report_status(SERVICE_RUNNING, SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN);
        }
        _ => {}
    }
    0
}

fn report_status(state: u32, accepted: u32) {
    let handle = STATUS_HANDLE.load(Ordering::SeqCst) as Handle;
    if handle.is_null() {
        return;
    }
    let mut status = ServiceStatus {
        service_type: SERVICE_WIN32_OWN_PROCESS,
        current_state: state,
        controls_accepted: accepted,
        ..Default::default()
    };
    unsafe {
        SetServiceStatus(handle, &mut status);
    }
}

// ---------------------------------------------------------------------------
// install / uninstall / start / stop / status
// ---------------------------------------------------------------------------

pub fn install(cfg: &Config) -> Result<(), String> {
    let base = cfg.api_base()?;
    let key = cfg.api_key()?;

    let dest = install_binary(cfg)?;
    let log_path = cfg
        .log_file
        .clone()
        .or_else(|| {
            dest.parent()
                .map(|p| p.join("pulse-agent.log").to_string_lossy().to_string())
        });

    let scm = open_scm(SC_MANAGER_ALL_ACCESS)?;
    let name_w = to_wide(&cfg.service_name);
    let display_w = to_wide("Pulse Agent");

    // Remove an existing instance so `install` is idempotent.
    let existing = unsafe { OpenServiceW(scm, name_w.as_ptr(), SERVICE_ALL_ACCESS) };
    if !existing.is_null() {
        let mut status = ServiceStatus::default();
        unsafe {
            ControlService(existing, SERVICE_CONTROL_STOP, &mut status);
        }
        std::thread::sleep(Duration::from_millis(600));
        unsafe {
            DeleteService(existing);
            CloseServiceHandle(existing);
        }
        std::thread::sleep(Duration::from_millis(600));
    }

    let bin_path = format!("\"{}\" run", dest.display());
    let bin_w = to_wide(&bin_path);

    let service = unsafe {
        CreateServiceW(
            scm,
            name_w.as_ptr(),
            display_w.as_ptr(),
            SERVICE_ALL_ACCESS,
            SERVICE_WIN32_OWN_PROCESS,
            SERVICE_AUTO_START,
            SERVICE_ERROR_NORMAL,
            bin_w.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };

    if service.is_null() {
        let err = unsafe { GetLastError() };
        unsafe {
            CloseServiceHandle(scm);
        }
        return Err(format!(
            "创建服务失败，错误码 {}{}",
            err,
            admin_hint(err)
        ));
    }

    let env_result = set_service_environment(
        &cfg.service_name,
        &base,
        &key,
        cfg.interval,
        cfg.insecure,
        cfg.log_level.as_str(),
        log_path.as_deref(),
    );

    unsafe {
        CloseServiceHandle(service);
        CloseServiceHandle(scm);
    }

    env_result?;

    println!("已创建服务: {}", cfg.service_name);
    println!("程序位置: {}", dest.display());
    if let Some(log) = &log_path {
        println!("日志文件: {}", log);
    }

    // Make `pulse-agent` callable from any new shell, mirroring Linux where the
    // binary lands in /usr/local/bin (already on PATH).
    if let Some(parent) = dest.parent() {
        match install_path_entry(parent) {
            Ok(true) => println!("已加入系统 PATH: {}", parent.display()),
            Ok(false) => println!("系统 PATH 已包含: {}", parent.display()),
            Err(err) => println!(
                "警告: 写入系统 PATH 失败，请手动添加 {}: {}",
                parent.display(),
                err
            ),
        }
    }

    start(cfg)?;
    Ok(())
}

pub fn uninstall(cfg: &Config) -> Result<(), String> {
    let _ = stop(cfg);
    std::thread::sleep(Duration::from_millis(600));

    with_service(cfg, DELETE_ACCESS, |service| {
        let ok = unsafe { DeleteService(service) };
        if ok == 0 {
            let code = unsafe { GetLastError() };
            return Err(format!("删除服务失败，错误码 {}{}", code, admin_hint(code)));
        }
        Ok(())
    })?;

    remove_path_entry(&install_dir(cfg));
    remove_install_dir(cfg);
    println!("已删除服务: {}", cfg.service_name);
    Ok(())
}

/// Delete the installation directory and everything inside it.
///
/// When the running executable lives in that directory Windows keeps the file
/// locked, so a short-lived helper is asked to finish the job after we exit.
fn remove_install_dir(cfg: &Config) {
    let dir = install_dir(cfg);
    if !safe_to_remove(&dir) {
        println!("警告: 拒绝删除可疑目录，请手动检查: {}", dir.display());
        return;
    }

    let exe = dir.join(format!("{}.exe", BIN_NAME));
    if !exe.exists() {
        return;
    }

    match std::fs::remove_dir_all(&dir) {
        Ok(()) => println!("已删除安装目录: {}", dir.display()),
        Err(_) => {
            if schedule_dir_removal(&dir) {
                println!("安装目录将在程序退出后自动删除: {}", dir.display());
            } else {
                println!("警告: 无法删除安装目录 {}，请手动删除", dir.display());
            }
        }
    }
}

/// Refuse to recursively delete drive roots or well-known system directories.
fn safe_to_remove(dir: &PathBuf) -> bool {
    if dir.components().count() < 3 {
        return false;
    }
    let lower = dir.to_string_lossy().to_ascii_lowercase();
    let trimmed = lower.trim_end_matches(|c| c == '\\' || c == '/');
    const FORBIDDEN: &[&str] = &[
        "\\program files",
        "\\program files (x86)",
        "\\windows",
        "\\system32",
        "\\users",
        "\\programdata",
    ];
    !FORBIDDEN.iter().any(|suffix| trimmed.ends_with(suffix))
}

fn schedule_dir_removal(dir: &PathBuf) -> bool {
    let path = dir.to_string_lossy().to_string();
    std::process::Command::new("cmd.exe")
        .arg("/C")
        .args([
            "ping", "-n", "3", "127.0.0.1", ">NUL", "&", "rmdir", "/S", "/Q",
        ])
        .arg(&path)
        .spawn()
        .is_ok()
}

pub fn start(cfg: &Config) -> Result<(), String> {
    with_service(cfg, SERVICE_START, |service| {
        let ok = unsafe { StartServiceW(service, 0, std::ptr::null()) };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            if err == ERROR_SERVICE_ALREADY_RUNNING {
                return Ok(());
            }
            return Err(format!("启动服务失败，错误码 {}", err));
        }
        Ok(())
    })?;
    println!("已启动服务: {}", cfg.service_name);
    Ok(())
}

pub fn stop(cfg: &Config) -> Result<(), String> {
    with_service(cfg, SERVICE_STOP | SERVICE_QUERY_STATUS, |service| {
        let mut status = ServiceStatus::default();
        let ok = unsafe { ControlService(service, SERVICE_CONTROL_STOP, &mut status) };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            if err == ERROR_SERVICE_NOT_ACTIVE {
                return Ok(());
            }
            return Err(format!("停止服务失败，错误码 {}", err));
        }
        Ok(())
    })?;
    println!("已停止服务: {}", cfg.service_name);
    Ok(())
}

pub fn restart(cfg: &Config) -> Result<(), String> {
    let _ = stop(cfg);
    std::thread::sleep(Duration::from_millis(800));
    start(cfg)
}

pub fn status(cfg: &Config) -> Result<(), String> {
    let (state, pid) = with_service(cfg, SERVICE_QUERY_STATUS, |service| {
        let mut buffer = ServiceStatusProcess::default();
        let mut needed: u32 = 0;
        let ok = unsafe {
            QueryServiceStatusEx(
                service,
                0, // SC_STATUS_PROCESS_INFO
                &mut buffer as *mut ServiceStatusProcess as *mut u8,
                std::mem::size_of::<ServiceStatusProcess>() as u32,
                &mut needed,
            )
        };
        if ok == 0 {
            return Err(format!(
                "查询服务状态失败，错误码 {}",
                unsafe { GetLastError() }
            ));
        }
        Ok((map_state(buffer.current_state), buffer.process_id))
    })?;

    StatusReport {
        service: &cfg.service_name,
        state,
        pid: Some(pid),
        binary: read_service_image(&cfg.service_name),
    }
    .print();
    Ok(())
}

fn map_state(state: u32) -> ServiceState {
    match state {
        SERVICE_STOPPED => ServiceState::Stopped,
        SERVICE_START_PENDING | SERVICE_CONTINUE_PENDING => ServiceState::StartPending,
        SERVICE_STOP_PENDING | SERVICE_PAUSE_PENDING => ServiceState::StopPending,
        SERVICE_RUNNING => ServiceState::Running,
        SERVICE_PAUSED => ServiceState::Paused,
        _ => ServiceState::Unknown,
    }
}

/// Read `ImagePath` from the service's registry key so `status` can show the
/// installed executable, matching the Linux output.
fn read_service_image(service_name: &str) -> Option<String> {
    let sub_key = to_wide(&format!(
        "SYSTEM\\CurrentControlSet\\Services\\{}",
        service_name
    ));
    let mut hkey: Hkey = 0;
    let rc = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            sub_key.as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut hkey,
        )
    };
    if rc != 0 {
        return None;
    }

    let value_name = to_wide("ImagePath");
    let mut value_type: u32 = 0;
    let mut len: u32 = 0;
    let rc = unsafe {
        RegQueryValueExW(
            hkey,
            value_name.as_ptr(),
            std::ptr::null_mut(),
            &mut value_type,
            std::ptr::null_mut(),
            &mut len,
        )
    };
    if rc != 0 || len == 0 {
        unsafe {
            RegCloseKey(hkey);
        }
        return None;
    }

    let mut buf = vec![0u8; len as usize];
    let rc = unsafe {
        RegQueryValueExW(
            hkey,
            value_name.as_ptr(),
            std::ptr::null_mut(),
            &mut value_type,
            buf.as_mut_ptr(),
            &mut len,
        )
    };
    unsafe {
        RegCloseKey(hkey);
    }
    if rc != 0 || (value_type != REG_SZ && value_type != REG_EXPAND_SZ) {
        return None;
    }

    let units: Vec<u16> = buf[..len as usize]
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let raw = String::from_utf16_lossy(&units);
    normalize_image_path(raw.trim_matches('\0').trim())
}

/// Turn `"C:\...\pulse-agent.exe" run` into a bare executable path.
fn normalize_image_path(raw: &str) -> Option<String> {
    let mut path = raw.trim();
    if let Some(stripped) = path.strip_suffix(" run") {
        path = stripped.trim_end();
    }
    let path = path.trim_matches('"').trim();
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

fn with_service<T, F>(cfg: &Config, access: u32, action: F) -> Result<T, String>
where
    F: FnOnce(Handle) -> Result<T, String>,
{
    let scm = open_scm(SC_MANAGER_CONNECT)?;
    let name_w = to_wide(&cfg.service_name);
    let service = unsafe { OpenServiceW(scm, name_w.as_ptr(), access) };

    let result = if service.is_null() {
        let code = unsafe { GetLastError() };
        Err(if code == ERROR_SERVICE_DOES_NOT_EXIST {
            format!("服务 {} 不存在（尚未安装）", cfg.service_name)
        } else {
            format!("无法打开服务 {}，错误码 {}{}", cfg.service_name, code, admin_hint(code))
        })
    } else {
        let outcome = action(service);
        unsafe {
            CloseServiceHandle(service);
        }
        outcome
    };

    unsafe {
        CloseServiceHandle(scm);
    }
    result
}

const ERROR_SERVICE_DOES_NOT_EXIST: u32 = 1060;

fn admin_hint(code: u32) -> &'static str {
    if code == ERROR_ACCESS_DENIED {
        "（请以管理员身份运行）"
    } else {
        ""
    }
}

const ERROR_ACCESS_DENIED: u32 = 5;

fn open_scm(access: u32) -> Result<Handle, String> {
    let scm = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), access) };
    if scm.is_null() {
        let code = unsafe { GetLastError() };
        return Err(format!(
            "打开服务管理器失败，错误码 {}{}",
            code,
            admin_hint(code)
        ));
    }
    Ok(scm)
}

fn install_binary(cfg: &Config) -> Result<PathBuf, String> {
    let dir = install_dir(cfg);

    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("创建目录 {} 失败: {}", dir.display(), e))?;

    let dest = dir.join(format!("{}.exe", BIN_NAME));
    let current = std::env::current_exe().map_err(|e| format!("获取当前程序路径失败: {}", e))?;
    if !same_path(&current, &dest) {
        std::fs::copy(&current, &dest)
            .map_err(|e| format!("复制程序到 {} 失败: {}", dest.display(), e))?;
    }

    Ok(dest)
}

fn default_install_dir() -> PathBuf {
    let base = std::env::var("ProgramFiles")
        .or_else(|_| std::env::var("ProgramW6432"))
        .unwrap_or_else(|_| "C:\\Program Files".to_string());
    PathBuf::from(base).join("PulseAgent")
}

fn install_dir(cfg: &Config) -> PathBuf {
    match &cfg.install_dir {
        Some(dir) => PathBuf::from(dir),
        None => default_install_dir(),
    }
}

fn same_path(a: &PathBuf, b: &PathBuf) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

// ---------------------------------------------------------------------------
// System PATH management (so `pulse-agent` works from any shell after install)
// ---------------------------------------------------------------------------

fn normalize_dir(dir: &std::path::Path) -> String {
    dir.to_string_lossy()
        .trim()
        .trim_matches('"')
        .trim_end_matches(|c| c == '\\' || c == '/')
        .to_string()
}

fn same_dir(candidate: &str, target: &str) -> bool {
    let candidate = candidate
        .trim()
        .trim_matches('"')
        .trim_end_matches(|c| c == '\\' || c == '/');
    candidate.eq_ignore_ascii_case(target)
}

fn path_contains(current: &str, dir: &str) -> bool {
    current.split(';').any(|entry| same_dir(entry, dir))
}

/// Append `dir` to the machine-wide `PATH`. Returns `true` when it was added,
/// `false` when it was already present.
fn install_path_entry(dir: &std::path::Path) -> Result<bool, String> {
    let dir = normalize_dir(dir);
    if dir.is_empty() {
        return Ok(false);
    }
    let (mut current, value_type) = read_system_path()?;
    if path_contains(&current, &dir) {
        return Ok(false);
    }
    if !current.is_empty() && !current.ends_with(';') {
        current.push(';');
    }
    current.push_str(&dir);
    write_system_path(&current, value_type)?;
    broadcast_environment_change();
    Ok(true)
}

/// Remove `dir` from the machine-wide `PATH` if present.
fn remove_path_entry(dir: &std::path::Path) {
    let dir = normalize_dir(dir);
    if dir.is_empty() {
        return;
    }
    let (current, value_type) = match read_system_path() {
        Ok(pair) => pair,
        Err(_) => return,
    };
    let entries: Vec<&str> = current.split(';').collect();
    if !entries.iter().any(|entry| same_dir(entry, &dir)) {
        return;
    }
    let updated: Vec<&str> = entries
        .into_iter()
        .filter(|entry| !same_dir(entry, &dir))
        .collect();
    let updated = updated.join(";");
    if write_system_path(&updated, value_type).is_ok() {
        broadcast_environment_change();
        println!("已从系统 PATH 移除: {}", dir);
    }
}

fn read_system_path() -> Result<(String, u32), String> {
    let sub_key = to_wide(SYSTEM_ENV_KEY);
    let mut hkey: Hkey = 0;
    let rc = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            sub_key.as_ptr(),
            0,
            KEY_QUERY_VALUE | KEY_SET_VALUE,
            &mut hkey,
        )
    };
    if rc != 0 {
        return Err(format!(
            "打开系统环境变量注册表项失败，错误码 {}{}",
            rc,
            admin_hint(rc as u32)
        ));
    }

    let value_name = to_wide("Path");
    let mut value_type: u32 = 0;
    let mut len: u32 = 0;
    let rc = unsafe {
        RegQueryValueExW(
            hkey,
            value_name.as_ptr(),
            std::ptr::null_mut(),
            &mut value_type,
            std::ptr::null_mut(),
            &mut len,
        )
    };
    if rc != 0 {
        unsafe {
            RegCloseKey(hkey);
        }
        if rc == ERROR_FILE_NOT_FOUND as i32 {
            return Ok((String::new(), REG_EXPAND_SZ));
        }
        return Err(format!("读取系统 PATH 失败，错误码 {}", rc));
    }

    let mut buf = vec![0u8; len as usize];
    let rc = unsafe {
        RegQueryValueExW(
            hkey,
            value_name.as_ptr(),
            std::ptr::null_mut(),
            &mut value_type,
            buf.as_mut_ptr(),
            &mut len,
        )
    };
    unsafe {
        RegCloseKey(hkey);
    }
    if rc != 0 {
        return Err(format!("读取系统 PATH 失败，错误码 {}", rc));
    }

    let units: Vec<u16> = buf[..len as usize]
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let text = String::from_utf16_lossy(&units);
    Ok((text.trim_end_matches('\0').to_string(), value_type))
}

fn write_system_path(value: &str, value_type: u32) -> Result<(), String> {
    let sub_key = to_wide(SYSTEM_ENV_KEY);
    let mut hkey: Hkey = 0;
    let rc = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            sub_key.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        )
    };
    if rc != 0 {
        return Err(format!(
            "打开系统环境变量注册表项失败，错误码 {}{}",
            rc,
            admin_hint(rc as u32)
        ));
    }

    let units: Vec<u16> = value.encode_utf16().collect();
    let mut bytes: Vec<u8> = Vec::with_capacity(units.len() * 2);
    for unit in &units {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }

    let value_name = to_wide("Path");
    let ty = if value_type == REG_SZ {
        REG_SZ
    } else {
        REG_EXPAND_SZ
    };
    let rc = unsafe {
        RegSetValueExW(
            hkey,
            value_name.as_ptr(),
            0,
            ty,
            bytes.as_ptr(),
            bytes.len() as u32,
        )
    };
    unsafe {
        RegCloseKey(hkey);
    }
    if rc != 0 {
        return Err(format!("写入系统 PATH 失败，错误码 {}", rc));
    }
    Ok(())
}

/// Tell running processes (notably Explorer) that the environment changed.
fn broadcast_environment_change() {
    let target = to_wide("Environment");
    let mut result: usize = 0;
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            target.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            5000,
            &mut result,
        );
    }
}

const ERROR_FILE_NOT_FOUND: u32 = 2;

fn set_service_environment(
    service_name: &str,
    base: &str,
    api_key: &str,
    interval: Option<u64>,
    insecure: bool,
    log_level: &str,
    log_file: Option<&str>,
) -> Result<(), String> {
    let sub_key = format!(
        "SYSTEM\\CurrentControlSet\\Services\\{}",
        service_name
    );
    let sub_key_w = to_wide(&sub_key);

    let mut hkey: Hkey = 0;
    let rc = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            sub_key_w.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        )
    };
    if rc != 0 {
        return Err(format!("打开服务注册表项失败，错误码 {}", rc));
    }

    let mut entries: Vec<String> = Vec::new();
    entries.push(format!("PULSE_URL={}", base));
    entries.push(format!("PULSE_API_KEY={}", api_key));
    entries.push(format!("PULSE_SERVICE_NAME={}", service_name));
    if let Some(interval) = interval {
        entries.push(format!("PULSE_INTERVAL={}", interval));
    }
    if insecure {
        entries.push("PULSE_INSECURE=1".to_string());
    }
    entries.push(format!("PULSE_LOG_LEVEL={}", log_level));
    if let Some(log_file) = log_file {
        entries.push(format!("PULSE_LOG_FILE={}", log_file));
    }

    // REG_MULTI_SZ: a sequence of NUL-terminated UTF-16 strings ending with an
    // extra NUL terminator.
    let mut units: Vec<u16> = Vec::new();
    for entry in &entries {
        units.extend(entry.encode_utf16());
        units.push(0);
    }
    units.push(0);

    let mut bytes: Vec<u8> = Vec::with_capacity(units.len() * 2);
    for unit in &units {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }

    let value_name = to_wide("Environment");
    let rc = unsafe {
        RegSetValueExW(
            hkey,
            value_name.as_ptr(),
            0,
            REG_MULTI_SZ,
            bytes.as_ptr(),
            bytes.len() as u32,
        )
    };

    unsafe {
        RegCloseKey(hkey);
    }

    if rc != 0 {
        return Err(format!("写入服务环境变量失败，错误码 {}", rc));
    }
    Ok(())
}

fn name_wide() -> &'static [u16] {
    SERVICE_NAME_W
        .get_or_init(|| to_wide("PulseAgent"))
        .as_slice()
}

fn to_wide(s: &str) -> Vec<u16> {
    let mut out: Vec<u16> = s.encode_utf16().collect();
    out.push(0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_dangerous_directories() {
        assert!(!safe_to_remove(&PathBuf::from("C:\\")));
        assert!(!safe_to_remove(&PathBuf::from("C:\\Program Files")));
        assert!(!safe_to_remove(&PathBuf::from("C:\\Windows")));
        assert!(!safe_to_remove(&PathBuf::from("C:\\ProgramData")));
        assert!(safe_to_remove(&PathBuf::from("C:\\Program Files\\PulseAgent")));
        assert!(safe_to_remove(&PathBuf::from("C:\\PulseAgent")));
    }

    #[test]
    fn normalizes_service_image_path() {
        assert_eq!(
            normalize_image_path("\"C:\\Program Files\\PulseAgent\\pulse-agent.exe\" run"),
            Some("C:\\Program Files\\PulseAgent\\pulse-agent.exe".to_string())
        );
        assert_eq!(
            normalize_image_path("\"C:\\Program Files\\PulseAgent\\pulse-agent.exe\""),
            Some("C:\\Program Files\\PulseAgent\\pulse-agent.exe".to_string())
        );
        assert_eq!(normalize_image_path("   "), None);
    }

    #[test]
    fn detects_path_entries_case_insensitively() {
        let path = "C:\\Windows;C:\\Program Files\\PulseAgent\\";
        assert!(path_contains(path, "c:\\program files\\pulseagent"));
        assert!(!path_contains(path, "C:\\Program Files\\Other"));
    }
}
