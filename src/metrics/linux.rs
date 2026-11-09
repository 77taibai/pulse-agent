//! Linux metric collection via `/proc` and friends.
//!
//! No external crates: `statvfs` is declared directly, `/proc` files are parsed
//! by hand.

use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs;
use std::os::raw::{c_char, c_int, c_void};
use std::time::{Duration, Instant};

use super::Sample;

extern "C" {
    fn statvfs(path: *const c_char, buf: *mut c_void) -> c_int;
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
        (Some((total0, idle0)), Some((total1, idle1))) => {
            let dt = total1.saturating_sub(total0);
            let di = idle1.saturating_sub(idle0);
            if dt > 0 {
                let busy = dt.saturating_sub(di) as f64;
                Some(((busy / dt as f64) * 100.0).clamp(0.0, 100.0))
            } else {
                None
            }
        }
        _ => None,
    };

    let (rx_delta, tx_delta) = (
        net_after.0.saturating_sub(net_before.0),
        net_after.1.saturating_sub(net_before.1),
    );
    let net_down = Some((rx_delta as f64 / elapsed).round() as u64);
    let net_up = Some((tx_delta as f64 / elapsed).round() as u64);

    let meminfo = read_meminfo();
    let mem_total = meminfo.get("MemTotal").copied();
    let mem_avail = meminfo.get("MemAvailable").copied().or_else(|| {
        let free = meminfo.get("MemFree").copied()?;
        let buffers = meminfo.get("Buffers").copied().unwrap_or(0);
        let cached = meminfo.get("Cached").copied().unwrap_or(0);
        Some(free.saturating_add(buffers).saturating_add(cached))
    });
    let mem_used = match (mem_total, mem_avail) {
        (Some(total), Some(avail)) => Some(total.saturating_sub(avail)),
        _ => None,
    };

    let swap_total = meminfo.get("SwapTotal").copied();
    let swap_free = meminfo.get("SwapFree").copied();
    let swap_used = match (swap_total, swap_free) {
        (Some(total), Some(free)) => Some(total.saturating_sub(free)),
        _ => None,
    };

    let (disk_total, disk_used) = disk_totals();

    Sample {
        timezone: timezone_name(),
        hostname: hostname(),
        os: os_name(),
        kernel: kernel_version(),
        arch: Some(std::env::consts::ARCH.to_string()),
        uptime: uptime_secs(),
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

fn read_file(path: &str) -> Option<String> {
    fs::read_to_string(path).ok()
}

/// Returns `(total_jiffies, idle_jiffies)` for the aggregate CPU line.
fn cpu_times() -> Option<(u64, u64)> {
    let content = read_file("/proc/stat")?;
    let line = content.lines().next()?;
    let mut parts = line.split_whitespace();
    if parts.next()? != "cpu" {
        return None;
    }

    let mut total: u64 = 0;
    let mut idle: u64 = 0;
    for (idx, token) in parts.take(8).enumerate() {
        let value: u64 = token.parse().ok()?;
        total = total.saturating_add(value);
        // Fields 3 and 4 are `idle` and `iowait`.
        if idx == 3 || idx == 4 {
            idle = idle.saturating_add(value);
        }
    }
    Some((total, idle))
}

/// Summed `(rx_bytes, tx_bytes)` across non-loopback interfaces.
fn net_counters() -> (u64, u64) {
    let content = match read_file("/proc/net/dev") {
        Some(c) => c,
        None => return (0, 0),
    };

    let mut rx: u64 = 0;
    let mut tx: u64 = 0;

    for line in content.lines() {
        let colon = match line.find(':') {
            Some(i) => i,
            None => continue,
        };
        let name = line[..colon].trim();
        if name.is_empty() || name == "lo" {
            continue;
        }
        let fields: Vec<&str> = line[colon + 1..].split_whitespace().collect();
        // 1st field is rx_bytes, 9th field is tx_bytes.
        if fields.len() < 9 {
            continue;
        }
        if let Ok(v) = fields[0].parse::<u64>() {
            rx = rx.saturating_add(v);
        }
        if let Ok(v) = fields[8].parse::<u64>() {
            tx = tx.saturating_add(v);
        }
    }

    (rx, tx)
}

fn read_meminfo() -> HashMap<String, u64> {
    let mut map = HashMap::new();
    if let Some(content) = read_file("/proc/meminfo") {
        for line in content.lines() {
            if let Some((key, rest)) = line.split_once(':') {
                let number = rest.split_whitespace().next().unwrap_or("0");
                if let Ok(value_kb) = number.parse::<u64>() {
                    map.insert(key.trim().to_string(), value_kb.saturating_mul(1024));
                }
            }
        }
    }
    map
}

fn disk_totals() -> (Option<u64>, Option<u64>) {
    let content = match read_file("/proc/mounts") {
        Some(c) => c,
        None => return (None, None),
    };

    let mut seen_devices: HashSet<String> = HashSet::new();
    let mut total: u64 = 0;
    let mut used: u64 = 0;
    let mut found_any = false;

    for line in content.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 3 {
            continue;
        }
        let device = fields[0];
        let mountpoint = unescape_mount(fields[1]);
        let fstype = fields[2];

        if is_pseudo_fs(fstype) {
            continue;
        }
        if !is_real_device(device, fstype) {
            continue;
        }
        // The same block device may be mounted several times (bind mounts);
        // count it once.
        if !seen_devices.insert(device.to_string()) {
            continue;
        }

        if let Some((mount_total, mount_used)) = mount_usage(&mountpoint) {
            total = total.saturating_add(mount_total);
            used = used.saturating_add(mount_used);
            found_any = true;
        }
    }

    if found_any {
        (Some(total), Some(used))
    } else {
        (None, None)
    }
}

fn is_real_device(device: &str, fstype: &str) -> bool {
    device.starts_with("/dev/") || matches!(fstype, "zfs" | "btrfs")
}

fn is_pseudo_fs(fstype: &str) -> bool {
    matches!(
        fstype,
        "proc"
            | "sysfs"
            | "devtmpfs"
            | "devpts"
            | "tmpfs"
            | "ramfs"
            | "cgroup"
            | "cgroup2"
            | "pstore"
            | "bpf"
            | "tracefs"
            | "debugfs"
            | "securityfs"
            | "configfs"
            | "fusectl"
            | "mqueue"
            | "hugetlbfs"
            | "rpc_pipefs"
            | "autofs"
            | "binfmt_misc"
            | "nsfs"
            | "squashfs"
            | "overlay"
            | "selinuxfs"
            | "efivarfs"
            | "fuse.gvfsd-fuse"
            | "fuse.lxcfs"
            | "nfsd"
            | "sunrpc"
            | "procfs"
            | "sysfs_dev"
    )
}

fn unescape_mount(field: &str) -> String {
    field
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

/// Query a mount point with `statvfs` and return `(total_bytes, used_bytes)`.
///
/// `struct statvfs` starts with eleven pointer-sized unsigned words:
/// `f_bsize`, `f_frsize`, `f_blocks`, `f_bfree`, `f_bavail`, …  Because
/// `unsigned long` has the same width as a pointer on every supported Linux
/// ABI, reading the raw buffer by word index is portable across glibc and musl
/// without depending on the (version-specific) tail of the struct.
fn mount_usage(mountpoint: &str) -> Option<(u64, u64)> {
    let path = CString::new(mountpoint).ok()?;
    // Over-sized and 16-byte aligned so any libc variant stays in bounds.
    let mut buf = AlignedBuf([0u8; 256]);

    let rc = unsafe { statvfs(path.as_ptr(), buf.0.as_mut_ptr() as *mut c_void) };
    if rc != 0 {
        return None;
    }

    let word = std::mem::size_of::<usize>();
    let f_frsize = read_word(&buf.0, 1, word);
    let f_blocks = read_word(&buf.0, 2, word);
    let f_bfree = read_word(&buf.0, 3, word);

    if f_frsize == 0 {
        return None;
    }

    let total = f_blocks.saturating_mul(f_frsize);
    let used = f_blocks.saturating_sub(f_bfree).saturating_mul(f_frsize);
    Some((total, used))
}

#[repr(align(16))]
struct AlignedBuf([u8; 256]);

fn read_word(buf: &[u8; 256], index: usize, word: usize) -> u64 {
    let start = index * word;
    if word == 8 {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&buf[start..start + 8]);
        u64::from_ne_bytes(bytes)
    } else {
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(&buf[start..start + 4]);
        u32::from_ne_bytes(bytes) as u64
    }
}

fn cpu_model() -> Option<String> {
    let content = read_file("/proc/cpuinfo")?;
    let mut model_name: Option<String> = None;
    let mut hardware: Option<String> = None;
    let mut processor: Option<String> = None;

    for line in content.lines() {
        let (key, value) = match line.split_once(':') {
            Some(kv) => kv,
            None => continue,
        };
        let key = key.trim();
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match key {
            "model name" if model_name.is_none() => model_name = Some(value.to_string()),
            "Hardware" if hardware.is_none() => hardware = Some(value.to_string()),
            "Processor" if processor.is_none() => processor = Some(value.to_string()),
            _ => {}
        }
    }

    model_name.or(hardware).or(processor)
}

fn cpu_cores() -> Option<u64> {
    if let Some(content) = read_file("/proc/cpuinfo") {
        let count = content
            .lines()
            .filter(|line| line.starts_with("processor"))
            .count();
        if count > 0 {
            return Some(count as u64);
        }
    }
    std::thread::available_parallelism()
        .ok()
        .map(|n| n.get() as u64)
}

fn hostname() -> Option<String> {
    if let Some(content) = read_file("/proc/sys/kernel/hostname") {
        let name = content.trim();
        if !name.is_empty() {
            return Some(name.to_string());
        }
    }
    std::env::var("HOSTNAME")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn kernel_version() -> Option<String> {
    read_file("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn uptime_secs() -> Option<u64> {
    let content = read_file("/proc/uptime")?;
    let first = content.split_whitespace().next()?;
    let seconds: f64 = first.parse().ok()?;
    if seconds.is_finite() && seconds >= 0.0 {
        Some(seconds as u64)
    } else {
        None
    }
}

fn os_name() -> Option<String> {
    let content = read_file("/etc/os-release").or_else(|| read_file("/usr/lib/os-release"))?;
    for line in content.lines() {
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == "PRETTY_NAME" {
                let name = value.trim().trim_matches('"').trim();
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

fn timezone_name() -> Option<String> {
    if let Ok(tz) = std::env::var("TZ") {
        let tz = tz.trim().trim_start_matches(':');
        if !tz.is_empty() {
            return Some(tz.to_string());
        }
    }

    if let Some(content) = read_file("/etc/timezone") {
        if let Some(first) = content.lines().next() {
            let name = first.trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }

    if let Ok(target) = fs::read_link("/etc/localtime") {
        let path = target.to_string_lossy();
        if let Some(idx) = path.find("zoneinfo/") {
            let name = &path[idx + "zoneinfo/".len()..];
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }

    None
}
