//! systemd integration.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::agent::{self, Agent};
use crate::cli::{Config, BIN_NAME};
use crate::util::{run_command, run_command_checked};

const DEFAULT_BIN_DIR: &str = "/usr/local/bin";
const SYSTEMD_DIR: &str = "/etc/systemd/system";

extern "C" {
    fn geteuid() -> u32;
}

pub fn run(cfg: &Config) -> Result<(), String> {
    agent::install_signal_handlers();
    let agent = Agent::new(cfg)?;
    agent.run(Some(&agent::SIGNAL_STOP));
    Ok(())
}

pub fn install(cfg: &Config) -> Result<(), String> {
    require_root()?;
    let _ = cfg.api_base()?;
    let _ = cfg.api_key()?;

    let bin_dir = cfg
        .install_dir
        .clone()
        .unwrap_or_else(|| DEFAULT_BIN_DIR.to_string());
    fs::create_dir_all(&bin_dir).map_err(|e| format!("创建目录 {} 失败: {}", bin_dir, e))?;

    let dest = Path::new(&bin_dir).join(BIN_NAME);
    let current = std::env::current_exe().map_err(|e| format!("获取当前程序路径失败: {}", e))?;
    if current != dest {
        fs::copy(&current, &dest)
            .map_err(|e| format!("复制程序到 {} 失败: {}", dest.display(), e))?;
    }
    set_mode(&dest, 0o755)?;

    let unit_path = unit_path(&cfg.service_name);
    let unit = build_unit(cfg, &dest);
    fs::write(&unit_path, unit)
        .map_err(|e| format!("写入 {} 失败: {}", unit_path.display(), e))?;
    set_mode(&unit_path, 0o600)?;

    let service = format!("{}.service", cfg.service_name);
    run_command_checked("systemctl", &["daemon-reload"])?;
    run_command_checked("systemctl", &["enable", service.as_str()])?;
    run_command_checked("systemctl", &["restart", service.as_str()])?;

    println!("已安装并启动服务: {}", service);
    println!("程序位置: {}", dest.display());
    println!("unit 文件: {}", unit_path.display());
    println!("查看日志: journalctl -u {} -f", service);
    Ok(())
}

pub fn uninstall(cfg: &Config) -> Result<(), String> {
    require_root()?;
    let service = format!("{}.service", cfg.service_name);

    let _ = run_command("systemctl", &["stop", service.as_str()]);
    let _ = run_command("systemctl", &["disable", service.as_str()]);

    let unit_path = unit_path(&cfg.service_name);
    if unit_path.exists() {
        fs::remove_file(&unit_path)
            .map_err(|e| format!("删除 {} 失败: {}", unit_path.display(), e))?;
    }
    let _ = run_command_checked("systemctl", &["daemon-reload"]);

    let bin_dir = cfg
        .install_dir
        .clone()
        .unwrap_or_else(|| DEFAULT_BIN_DIR.to_string());
    let dest = Path::new(&bin_dir).join(BIN_NAME);
    if dest.exists() {
        match fs::remove_file(&dest) {
            Ok(()) => println!("已删除程序文件: {}", dest.display()),
            Err(e) => println!("警告: 删除 {} 失败: {}", dest.display(), e),
        }
    }
    // Only remove the directory when the user explicitly chose a dedicated one,
    // and only if it is now empty (never delete a shared bin directory).
    if cfg.install_dir.is_some() {
        let _ = fs::remove_dir(&bin_dir);
    }

    println!("已停止并移除服务: {}", service);
    Ok(())
}

pub fn start(cfg: &Config) -> Result<(), String> {
    require_root()?;
    let service = format!("{}.service", cfg.service_name);
    run_command_checked("systemctl", &["start", service.as_str()])?;
    println!("已启动服务: {}", service);
    Ok(())
}

pub fn stop(cfg: &Config) -> Result<(), String> {
    require_root()?;
    let service = format!("{}.service", cfg.service_name);
    run_command_checked("systemctl", &["stop", service.as_str()])?;
    println!("已停止服务: {}", service);
    Ok(())
}

pub fn restart(cfg: &Config) -> Result<(), String> {
    require_root()?;
    let service = format!("{}.service", cfg.service_name);
    run_command_checked("systemctl", &["restart", service.as_str()])?;
    println!("已重启服务: {}", service);
    Ok(())
}

pub fn status(cfg: &Config) -> Result<(), String> {
    let service = format!("{}.service", cfg.service_name);
    let out = run_command(
        "systemctl",
        &["status", service.as_str(), "--no-pager", "-l"],
    )?;
    if !out.stdout.is_empty() {
        println!("{}", out.stdout);
    }
    if !out.stderr.is_empty() {
        println!("{}", out.stderr);
    }
    Ok(())
}

fn unit_path(service_name: &str) -> PathBuf {
    Path::new(SYSTEMD_DIR).join(format!("{}.service", service_name))
}

fn build_unit(cfg: &Config, binary: &Path) -> String {
    let mut unit = String::new();
    unit.push_str("[Unit]\n");
    unit.push_str("Description=Pulse Agent - server metrics reporter\n");
    unit.push_str("After=network-online.target\n");
    unit.push_str("Wants=network-online.target\n\n");

    unit.push_str("[Service]\n");
    unit.push_str("Type=simple\n");

    let base = cfg.api_base().unwrap_or_default();
    let key = cfg.api_key().unwrap_or_default();
    unit.push_str(&format!(
        "Environment=\"PULSE_URL={}\"\n",
        systemd_escape(&base)
    ));
    unit.push_str(&format!(
        "Environment=\"PULSE_API_KEY={}\"\n",
        systemd_escape(&key)
    ));
    if let Some(interval) = cfg.interval {
        unit.push_str(&format!("Environment=\"PULSE_INTERVAL={}\"\n", interval));
    }
    if cfg.insecure {
        unit.push_str("Environment=\"PULSE_INSECURE=1\"\n");
    }
    unit.push_str(&format!(
        "Environment=\"PULSE_LOG_LEVEL={}\"\n",
        cfg.log_level.as_str()
    ));
    if let Some(log_file) = &cfg.log_file {
        unit.push_str(&format!(
            "Environment=\"PULSE_LOG_FILE={}\"\n",
            systemd_escape(log_file)
        ));
    }

    unit.push_str(&format!("ExecStart={} run\n", binary.display()));
    unit.push_str("Restart=always\n");
    unit.push_str("RestartSec=10\n");
    unit.push_str("NoNewPrivileges=true\n");
    unit.push_str("ProtectSystem=full\n");
    unit.push_str("ProtectHome=read-only\n");
    unit.push_str("PrivateTmp=true\n\n");

    unit.push_str("[Install]\n");
    unit.push_str("WantedBy=multi-user.target\n");
    unit
}

/// Escape a value for use inside a systemd double-quoted `Environment=` entry.
fn systemd_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '%' => out.push_str("%%"),
            '\n' | '\r' => out.push(' '),
            _ => out.push(c),
        }
    }
    out
}

fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    let permissions = fs::Permissions::from_mode(mode);
    fs::set_permissions(path, permissions)
        .map_err(|e| format!("设置 {} 权限失败: {}", path.display(), e))
}

fn require_root() -> Result<(), String> {
    let euid = unsafe { geteuid() };
    if euid != 0 {
        return Err("该操作需要 root 权限，请使用 sudo 运行".to_string());
    }
    Ok(())
}
