//! Small helpers shared across modules: current time, UTC formatting and
//! subprocess execution for the service installers.

// The subprocess helpers are only used by the Linux installer; the Windows one
// talks to the SCM API directly.
#![allow(dead_code)]

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch (0 if the clock is before 1970).
pub fn unix_now() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(_) => 0,
    }
}

/// Format a Unix timestamp as `YYYY-MM-DD HH:MM:SS UTC`.
///
/// Uses Howard Hinnant's `civil_from_days` algorithm so no date library is
/// needed on either platform.
pub fn format_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        y, mo, d, h, mi, s
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Outcome of running an external command.
pub struct CmdOutput {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Run a command and capture its output. Never panics on a non-zero exit.
pub fn run_command(program: &str, args: &[&str]) -> Result<CmdOutput, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("无法执行 {}: {}", program, e))?;
    Ok(CmdOutput {
        success: output.status.success(),
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
}

/// Run a command and turn a non-zero exit status into an error with context.
pub fn run_command_checked(program: &str, args: &[&str]) -> Result<CmdOutput, String> {
    let out = run_command(program, args)?;
    if out.success {
        Ok(out)
    } else {
        let mut detail = String::new();
        if !out.stdout.is_empty() {
            detail.push_str(&out.stdout);
        }
        if !out.stderr.is_empty() {
            if !detail.is_empty() {
                detail.push_str(" / ");
            }
            detail.push_str(&out.stderr);
        }
        if detail.is_empty() {
            detail = format!("exit code {:?}", out.code);
        }
        Err(format!("{} {} 失败: {}", program, args.join(" "), detail))
    }
}
