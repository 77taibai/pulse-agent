//! Tiny logging facility with a configurable level.
//!
//! The default level is [`Level::Error`], so a normally-running agent stays
//! quiet; use `--log-level` (or `PULSE_LOG_LEVEL`) to see more.
//!
//! Logs go to stderr (captured by `journalctl` on Linux) and, when a log file
//! is configured, are appended to it. The log file matters on Windows, where a
//! service has no console attached.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;

use crate::util::format_utc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

impl Level {
    pub fn parse(value: &str) -> Option<Level> {
        match value.trim().to_ascii_lowercase().as_str() {
            "error" | "err" | "0" => Some(Level::Error),
            "warn" | "warning" | "1" => Some(Level::Warn),
            "info" | "2" => Some(Level::Info),
            "debug" | "trace" | "3" | "4" => Some(Level::Debug),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
        }
    }
}

impl std::fmt::Display for Level {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

static LEVEL: Mutex<Level> = Mutex::new(Level::Error);
static LOG_FILE: Mutex<Option<String>> = Mutex::new(None);

pub fn set_level(level: Level) {
    if let Ok(mut guard) = LEVEL.lock() {
        *guard = level;
    }
}

pub fn level() -> Level {
    LEVEL.lock().map(|g| *g).unwrap_or(Level::Error)
}

/// Point the logger at an append-only file (or `None` to disable).
pub fn set_log_file(path: Option<String>) {
    if let Ok(mut guard) = LOG_FILE.lock() {
        *guard = path;
    }
}

fn enabled(level: Level) -> bool {
    level <= self::level()
}

fn emit(level: Level, message: &str) {
    if !enabled(level) {
        return;
    }

    let line = format!(
        "{} [{}] {}\n",
        format_utc(crate::util::unix_now()),
        level.label(),
        message
    );

    // stderr never needs to be flushed explicitly and is harmless when closed.
    eprint!("{}", line);

    if let Ok(guard) = LOG_FILE.lock() {
        if let Some(path) = guard.as_ref() {
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
                let _ = f.write_all(line.as_bytes());
                let _ = f.flush();
            }
        }
    }
}

pub fn error(message: &str) {
    emit(Level::Error, message);
}

pub fn warn(message: &str) {
    emit(Level::Warn, message);
}

pub fn info(message: &str) {
    emit(Level::Info, message);
}

pub fn debug(message: &str) {
    emit(Level::Debug, message);
}
