//! Platform service integration (systemd on Linux, SCM on Windows).

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod imp;

#[cfg(target_os = "windows")]
#[path = "windows.rs"]
mod imp;

pub use imp::{install, restart, run, start, status, stop, uninstall};

/// Platform-independent service state, so `status` prints the same set of
/// values on Linux (systemd) and Windows (SCM).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Running,
    Stopped,
    StartPending,
    StopPending,
    Paused,
    /// systemd can report `failed`; SCM has no equivalent state.
    #[allow(dead_code)]
    Failed,
    Unknown,
}

impl ServiceState {
    /// Stable machine-readable token shared by both platforms.
    pub fn token(self) -> &'static str {
        match self {
            ServiceState::Running => "running",
            ServiceState::Stopped => "stopped",
            ServiceState::StartPending => "start_pending",
            ServiceState::StopPending => "stop_pending",
            ServiceState::Paused => "paused",
            ServiceState::Failed => "failed",
            ServiceState::Unknown => "unknown",
        }
    }

    /// Human-readable Chinese label.
    pub fn label(self) -> &'static str {
        match self {
            ServiceState::Running => "运行中",
            ServiceState::Stopped => "已停止",
            ServiceState::StartPending => "正在启动",
            ServiceState::StopPending => "正在停止",
            ServiceState::Paused => "已暂停",
            ServiceState::Failed => "失败",
            ServiceState::Unknown => "未知",
        }
    }
}

/// Unified `status` payload rendered identically on every platform.
pub struct StatusReport<'a> {
    pub service: &'a str,
    pub state: ServiceState,
    pub pid: Option<u32>,
    pub binary: Option<String>,
}

impl StatusReport<'_> {
    pub fn print(&self) {
        println!("服务: {}", self.service);
        println!("状态: {} ({})", self.state.label(), self.state.token());
        match self.pid {
            Some(pid) if pid != 0 => println!("PID:  {}", pid),
            _ => println!("PID:  -"),
        }
        match self.binary.as_deref() {
            Some(bin) if !bin.is_empty() => println!("程序: {}", bin),
            _ => println!("程序: -"),
        }
    }
}
