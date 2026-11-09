//! Platform service integration (systemd on Linux, SCM on Windows).

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod imp;

#[cfg(target_os = "windows")]
#[path = "windows.rs"]
mod imp;

pub use imp::{install, restart, run, start, status, stop, uninstall};
