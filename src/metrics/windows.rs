//! Windows metric collection through Win32 FFI.
//!
//! All bindings are declared locally; no `windows-sys`/`winapi` crate is used.

use std::collections::HashMap;
use std::os::raw::{c_int, c_void};
use std::time::{Duration, Instant};

use super::Sample;

type Bool = i32;
type Hkey = isize;

const HKEY_LOCAL_MACHINE: Hkey = 0x8000_0002u32 as i32 as isize;
const RRF_RT_REG_SZ: u32 = 0x0000_0002;

const DRIVE_FIXED: u32 = 3;
const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;

// Byte offsets inside MIB_IFROW (Windows SDK netioapi.h / ipmib.h layout).
const IF_ROW_SIZE: usize = 860;
const OFF_ROW_INDEX: usize = 512;
const OFF_ROW_TYPE: usize = 516;
const OFF_ROW_IN_OCTETS: usize = 552;
const OFF_ROW_OUT_OCTETS: usize = 576;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FileTime {
    low: u32,
    high: u32,
}

impl FileTime {
    fn as_u64(&self) -> u64 {
        ((self.high as u64) << 32) | self.low as u64
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct MemoryStatusEx {
    length: u32,
    memory_load: u32,
    total_phys: u64,
    avail_phys: u64,
    total_page_file: u64,
    avail_page_file: u64,
    total_virtual: u64,
    avail_virtual: u64,
    avail_extended_virtual: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SystemInfo {
    processor_architecture: u16,
    reserved: u16,
    page_size: u32,
    minimum_application_address: *mut c_void,
    maximum_application_address: *mut c_void,
    active_processor_mask: usize,
    number_of_processors: u32,
    processor_type: u32,
    allocation_granularity: u32,
    processor_level: u16,
    processor_revision: u16,
}

impl Default for SystemInfo {
    fn default() -> Self {
        SystemInfo {
            processor_architecture: 0,
            reserved: 0,
            page_size: 0,
            minimum_application_address: std::ptr::null_mut(),
            maximum_application_address: std::ptr::null_mut(),
            active_processor_mask: 0,
            number_of_processors: 0,
            processor_type: 0,
            allocation_granularity: 0,
            processor_level: 0,
            processor_revision: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OsVersionInfoW {
    size: u32,
    major: u32,
    minor: u32,
    build: u32,
    platform_id: u32,
    csd_version: [u16; 128],
}

impl Default for OsVersionInfoW {
    fn default() -> Self {
        OsVersionInfoW {
            size: 0,
            major: 0,
            minor: 0,
            build: 0,
            platform_id: 0,
            csd_version: [0; 128],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SystemTime {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    milliseconds: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TimeZoneInformation {
    bias: i32,
    standard_name: [u16; 32],
    standard_date: SystemTime,
    standard_bias: i32,
    daylight_name: [u16; 32],
    daylight_date: SystemTime,
    daylight_bias: i32,
}

impl Default for TimeZoneInformation {
    fn default() -> Self {
        TimeZoneInformation {
            bias: 0,
            standard_name: [0; 32],
            standard_date: SystemTime::default(),
            standard_bias: 0,
            daylight_name: [0; 32],
            daylight_date: SystemTime::default(),
            daylight_bias: 0,
        }
    }
}

#[link(name = "kernel32")]
extern "system" {
    fn GetSystemTimes(idle: *mut FileTime, kernel: *mut FileTime, user: *mut FileTime) -> Bool;
    fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> Bool;
    fn GetSystemInfo(info: *mut SystemInfo);
    fn GetTickCount64() -> u64;
    fn GetComputerNameW(buffer: *mut u16, size: *mut u32) -> Bool;
    fn GetLogicalDrives() -> u32;
    fn GetDriveTypeW(root: *const u16) -> u32;
    fn GetDiskFreeSpaceExW(
        directory: *const u16,
        free_to_caller: *mut u64,
        total: *mut u64,
        total_free: *mut u64,
    ) -> Bool;
    fn GetTimeZoneInformation(info: *mut TimeZoneInformation) -> u32;
}

#[link(name = "advapi32")]
extern "system" {
    fn RegGetValueW(
        hkey: Hkey,
        sub_key: *const u16,
        value: *const u16,
        flags: u32,
        out_type: *mut u32,
        out_data: *mut c_void,
        out_size: *mut u32,
    ) -> i32;
}

#[link(name = "ntdll")]
extern "system" {
    fn RtlGetVersion(info: *mut OsVersionInfoW) -> i32;
}

#[link(name = "iphlpapi")]
extern "system" {
    fn GetIfTable(table: *mut c_void, size: *mut u32, order: c_int) -> u32;
}

pub fn sample() -> Sample {
    let window_start = Instant::now();

    let cpu_before = cpu_times();
    let net_before = net_counters();

    std::thread::sleep(Duration::from_millis(1000));

    let cpu_after = cpu_times();
    let net_after = net_counters();

    let elapsed = window_start.elapsed().as_secs_f64().max(0.001);

    let cpu_usage = match (cpu_before, cpu_after) {
        (Some(before), Some(after)) => {
            let kernel = after.1.saturating_sub(before.1);
            let user = after.2.saturating_sub(before.2);
            let total = kernel.saturating_add(user);
            let idle = after.0.saturating_sub(before.0);
            if total > 0 {
                let busy = total.saturating_sub(idle.min(total)) as f64;
                Some(((busy / total as f64) * 100.0).clamp(0.0, 100.0))
            } else {
                None
            }
        }
        _ => None,
    };

    let mut rx: u64 = 0;
    let mut tx: u64 = 0;
    for (index, (in_now, out_now)) in net_after.iter() {
        if let Some((in_before, out_before)) = net_before.get(index) {
            rx = rx.saturating_add(in_now.wrapping_sub(*in_before) as u64);
            tx = tx.saturating_add(out_now.wrapping_sub(*out_before) as u64);
        }
    }

    let net_down = Some((rx as f64 / elapsed).round() as u64);
    let net_up = Some((tx as f64 / elapsed).round() as u64);

    let (mem_total, mem_used, swap_total, swap_used) = memory_and_swap();
    let (disk_total, disk_used) = disk_totals();
    let (os_name, kernel) = os_version();

    Sample {
        timezone: timezone_name(),
        hostname: hostname(),
        os: os_name,
        kernel,
        arch: Some(std::env::consts::ARCH.to_string()),
        uptime: Some(unsafe { GetTickCount64() } / 1000),
        cpu_model: cpu_model(),
        cpu_cores: cpu_cores(),
        cpu_usage,
        mem_total,
        mem_used,
        swap_total,
        swap_used,
        disk_total,
        disk_used,
        net_up,
        net_down,
    }
}

fn cpu_times() -> Option<(u64, u64, u64)> {
    let mut idle = FileTime::default();
    let mut kernel = FileTime::default();
    let mut user = FileTime::default();
    let ok = unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) };
    if ok == 0 {
        return None;
    }
    Some((idle.as_u64(), kernel.as_u64(), user.as_u64()))
}

fn net_counters() -> HashMap<u32, (u32, u32)> {
    let mut map = HashMap::new();

    let mut size: u32 = 0;
    unsafe {
        GetIfTable(std::ptr::null_mut(), &mut size, 0);
    }
    if size == 0 || size as usize > 64 * 1024 * 1024 {
        return map;
    }

    // Over-allocate on a u64 boundary so the buffer is at least 8-byte aligned.
    let words = (size as usize + 7) / 8;
    let mut buf: Vec<u64> = vec![0u64; words];
    let byte_len = words * 8;
    let mut actual = byte_len as u32;
    let rc = unsafe { GetIfTable(buf.as_mut_ptr() as *mut c_void, &mut actual, 0) };
    if rc != 0 {
        return map;
    }

    let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, byte_len) };

    let count = match bytes.get(0..4) {
        Some(b) => u32::from_ne_bytes([b[0], b[1], b[2], b[3]]) as usize,
        None => return map,
    };

    for i in 0..count {
        let base = 4 + i * IF_ROW_SIZE;
        if base + IF_ROW_SIZE > bytes.len() {
            break;
        }
        let if_type = read_u32(bytes, base + OFF_ROW_TYPE);
        if if_type == IF_TYPE_SOFTWARE_LOOPBACK {
            continue;
        }
        let index = read_u32(bytes, base + OFF_ROW_INDEX);
        let in_octets = read_u32(bytes, base + OFF_ROW_IN_OCTETS);
        let out_octets = read_u32(bytes, base + OFF_ROW_OUT_OCTETS);
        map.insert(index, (in_octets, out_octets));
    }

    map
}

fn read_u32(buf: &[u8], offset: usize) -> u32 {
    u32::from_ne_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ])
}

fn memory_and_swap() -> (Option<u64>, Option<u64>, Option<u64>, Option<u64>) {
    let mut status = MemoryStatusEx {
        length: std::mem::size_of::<MemoryStatusEx>() as u32,
        ..Default::default()
    };
    let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
    if ok == 0 {
        return (None, None, None, None);
    }

    let mem_total = Some(status.total_phys);
    let mem_used = Some(status.total_phys.saturating_sub(status.avail_phys));

    // The page file counters include physical memory; subtract it to obtain
    // the size of the swap/page file itself.
    let swap_total = Some(status.total_page_file.saturating_sub(status.total_phys));
    let page_used = status
        .total_page_file
        .saturating_sub(status.avail_page_file);
    let phys_used = status.total_phys.saturating_sub(status.avail_phys);
    let swap_used = Some(page_used.saturating_sub(phys_used));

    (mem_total, mem_used, swap_total, swap_used)
}

fn cpu_cores() -> Option<u64> {
    let mut info = SystemInfo::default();
    unsafe { GetSystemInfo(&mut info) };
    if info.number_of_processors > 0 {
        Some(info.number_of_processors as u64)
    } else {
        None
    }
}

fn disk_totals() -> (Option<u64>, Option<u64>) {
    let mask = unsafe { GetLogicalDrives() };
    if mask == 0 {
        return (None, None);
    }

    let mut total: u64 = 0;
    let mut used: u64 = 0;
    let mut found_any = false;

    for letter in b'A'..=b'Z' {
        let bit = 1u32 << (letter - b'A');
        if mask & bit == 0 {
            continue;
        }

        let root: Vec<u16> = vec![letter as u16, b':' as u16, b'\\' as u16, 0];
        let drive_type = unsafe { GetDriveTypeW(root.as_ptr()) };
        if drive_type != DRIVE_FIXED {
            continue;
        }

        let mut free_to_caller: u64 = 0;
        let mut drive_total: u64 = 0;
        let mut total_free: u64 = 0;
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                root.as_ptr(),
                &mut free_to_caller,
                &mut drive_total,
                &mut total_free,
            )
        };
        if ok == 0 {
            continue;
        }

        total = total.saturating_add(drive_total);
        used = used.saturating_add(drive_total.saturating_sub(free_to_caller));
        found_any = true;
    }

    if found_any {
        (Some(total), Some(used))
    } else {
        (None, None)
    }
}

fn hostname() -> Option<String> {
    let mut buffer = [0u16; 256];
    let mut size: u32 = buffer.len() as u32;
    let ok = unsafe { GetComputerNameW(buffer.as_mut_ptr(), &mut size) };
    if ok == 0 {
        return std::env::var("COMPUTERNAME").ok();
    }
    Some(wide_to_string(&buffer[..size as usize]))
}

fn os_version() -> (Option<String>, Option<String>) {
    let mut info = OsVersionInfoW {
        size: std::mem::size_of::<OsVersionInfoW>() as u32,
        ..Default::default()
    };
    let status = unsafe { RtlGetVersion(&mut info) };
    if status != 0 {
        return (None, None);
    }

    let product = match (info.major, info.minor) {
        (10, _) if info.build >= 22000 => "Windows 11",
        (10, _) => "Windows 10",
        (6, 3) => "Windows 8.1",
        (6, 2) => "Windows 8",
        (6, 1) => "Windows 7",
        (6, 0) => "Windows Vista",
        (5, 2) => "Windows Server 2003",
        (5, 1) => "Windows XP",
        _ => "Windows",
    };
    let os = format!("{} (build {})", product, info.build);
    let kernel = format!("{}.{}.{}", info.major, info.minor, info.build);
    (Some(os), Some(kernel))
}

fn timezone_name() -> Option<String> {
    let mut info = TimeZoneInformation::default();
    let rc = unsafe { GetTimeZoneInformation(&mut info) };
    // TIME_ZONE_ID_INVALID is 0xFFFFFFFF.
    if rc == 0xFFFF_FFFF {
        return None;
    }

    let daylight = rc == 2; // TIME_ZONE_ID_DAYLIGHT
    let name = if daylight {
        &info.daylight_name[..]
    } else {
        &info.standard_name[..]
    };
    let text = wide_to_string(name);
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn cpu_model() -> Option<String> {
    let sub_key = to_wide("HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0");
    let value = to_wide("ProcessorNameString");

    let mut buffer = [0u16; 256];
    let mut size: u32 = (buffer.len() * 2) as u32;

    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            sub_key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buffer.as_mut_ptr() as *mut c_void,
            &mut size,
        )
    };
    if rc != 0 {
        return None;
    }

    let text = wide_to_string(&buffer);
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn to_wide(s: &str) -> Vec<u16> {
    let mut out: Vec<u16> = s.encode_utf16().collect();
    out.push(0);
    out
}

fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end]).trim().to_string()
}
