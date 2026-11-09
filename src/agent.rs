//! The reporting loop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::cli::Config;
use crate::http::{Client, Response};
use crate::json::{self, Json};
use crate::log;
use crate::metrics;
use crate::util;

/// Fallback interval when neither the config nor the panel suggests one.
pub const DEFAULT_INTERVAL: u64 = 60;

pub struct ReportOutcome {
    pub recommended_interval: Option<u64>,
}

pub struct Agent {
    client: Client,
    report_url: String,
    server_url: String,
    api_key: String,
    interval_override: Option<u64>,
}

impl Agent {
    pub fn new(cfg: &Config) -> Result<Agent, String> {
        let base = cfg.api_base()?;
        let api_key = cfg.api_key()?;
        let client = Client::new(cfg.insecure, Duration::from_secs(cfg.timeout))?;

        Ok(Agent {
            client,
            report_url: format!("{}/report", base),
            server_url: format!("{}/server", base),
            api_key,
            interval_override: cfg.interval,
        })
    }

    /// `GET /api/v1/server`: validate the key and read the recommended interval.
    pub fn check(&self) -> Result<Json, String> {
        let resp = self.client.get(&self.server_url, &self.api_key)?;
        interpret(&resp)?;
        json::parse(&resp.body).map_err(|e| format!("解析面板响应失败: {}", e))
    }

    /// Collect one sample and post it.
    pub fn report(&self) -> Result<ReportOutcome, String> {
        let sample = metrics::sample();
        let payload = sample.to_json(util::unix_now());
        log::debug(&format!("上报请求体 {} 字节", payload.len()));

        let resp = self.client.post_json(&self.report_url, &self.api_key, &payload)?;
        interpret(&resp)?;

        Ok(ReportOutcome {
            recommended_interval: recommended_interval(&resp.body),
        })
    }

    /// Run forever (until `stop` is set) and report on an interval.
    pub fn run(&self, stop: Option<&AtomicBool>) {
        let mut interval = self.interval_override.unwrap_or(0);

        if interval == 0 {
            match self.client.get(&self.server_url, &self.api_key) {
                Ok(resp) => match interpret(&resp) {
                    Ok(()) => {
                        if let Some(v) = recommended_interval(&resp.body) {
                            interval = v;
                        }
                        if let Some(name) = server_name(&resp.body) {
                            log::info(&format!("已连接面板，服务器: {}", name));
                        }
                    }
                    Err(e) => log::warn(&format!("自检失败，仍将尝试上报: {}", e)),
                },
                Err(e) => log::warn(&format!("自检请求失败，仍将尝试上报: {}", e)),
            }
        }

        if interval == 0 {
            interval = DEFAULT_INTERVAL;
        }
        if self.interval_override.is_some() {
            log::info(&format!("开始上报，使用固定间隔 {} 秒", interval));
        } else {
            log::info(&format!("开始上报，当前间隔 {} 秒", interval));
        }

        loop {
            if is_stopped(stop) {
                break;
            }

            match self.report() {
                Ok(outcome) => {
                    if let Some(server_interval) = outcome.recommended_interval {
                        log::info(&format!(
                            "上报成功（面板建议间隔 {} 秒）",
                            server_interval
                        ));
                        if self.interval_override.is_none() && server_interval > 0 {
                            interval = server_interval;
                        }
                    } else {
                        log::info("上报成功");
                    }
                }
                Err(e) => log::error(&format!("上报失败: {}", e)),
            }

            if !sleep_interruptible(interval, stop) {
                break;
            }
        }

        log::info("上报已停止");
    }
}

fn is_stopped(stop: Option<&AtomicBool>) -> bool {
    stop.map(|flag| flag.load(Ordering::SeqCst)).unwrap_or(false)
}

/// Sleep for `seconds`, waking early (returning `false`) when `stop` is set.
fn sleep_interruptible(seconds: u64, stop: Option<&AtomicBool>) -> bool {
    let total_ms = seconds.max(1).saturating_mul(1000);
    let mut remaining = total_ms;

    while remaining > 0 {
        if is_stopped(stop) {
            return false;
        }
        let step = remaining.min(500);
        std::thread::sleep(Duration::from_millis(step));
        remaining -= step;
    }

    !is_stopped(stop)
}

/// Map non-2xx responses and `{"ok":false}` bodies to a descriptive error.
fn interpret(resp: &Response) -> Result<(), String> {
    let parsed = json::parse(&resp.body).ok();
    let ok_flag = parsed
        .as_ref()
        .and_then(|j| j.get("ok"))
        .and_then(|j| j.as_bool());

    if (200..300).contains(&resp.status) && ok_flag != Some(false) {
        return Ok(());
    }

    let code = parsed
        .as_ref()
        .and_then(|j| j.get_path(&["error", "code"]))
        .and_then(|j| j.as_str())
        .unwrap_or("");
    let message = parsed
        .as_ref()
        .and_then(|j| j.get_path(&["error", "message"]))
        .and_then(|j| j.as_str())
        .unwrap_or("");

    if code.is_empty() && message.is_empty() {
        Err(format!(
            "HTTP {}: {}",
            resp.status,
            truncate(&resp.body, 300)
        ))
    } else if code.is_empty() {
        Err(format!("HTTP {}: {}", resp.status, message))
    } else if message.is_empty() {
        Err(format!("HTTP {} [{}]", resp.status, code))
    } else {
        Err(format!("HTTP {} [{}] {}", resp.status, code, message))
    }
}

fn recommended_interval(body: &str) -> Option<u64> {
    let parsed = json::parse(body).ok()?;
    parsed.get("report_interval").and_then(|v| v.as_u64())
}

fn server_name(body: &str) -> Option<String> {
    let parsed = json::parse(body).ok()?;
    parsed
        .get_path(&["server", "name"])
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn truncate(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max).collect();
    out.push('…');
    out
}

// ---------------------------------------------------------------------------
// Unix signal handling so systemd can stop the service cleanly.
// ---------------------------------------------------------------------------

#[cfg(unix)]
pub static SIGNAL_STOP: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
pub fn install_signal_handlers() {
    extern "C" {
        fn signal(signum: i32, handler: extern "C" fn(i32)) -> usize;
    }
    extern "C" fn on_signal(_signum: i32) {
        SIGNAL_STOP.store(true, Ordering::SeqCst);
    }

    const SIGHUP: i32 = 1;
    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;

    unsafe {
        signal(SIGINT, on_signal);
        signal(SIGTERM, on_signal);
        signal(SIGHUP, on_signal);
    }
}
