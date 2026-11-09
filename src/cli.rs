//! Command-line parsing.
//!
//! Configuration comes exclusively from CLI flags and environment variables
//! (there is no config file). Flags take precedence over environment variables,
//! and both take precedence over built-in defaults.

use std::env;

use crate::log::Level;

pub const BIN_NAME: &str = "pulse-agent";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Run,
    Once,
    Check,
    Install,
    Uninstall,
    Start,
    Stop,
    Restart,
    Status,
    Help,
    Version,
}

impl Command {
    fn from_str(s: &str) -> Option<Command> {
        Some(match s {
            "run" | "start-agent" => Command::Run,
            "once" | "report" => Command::Once,
            "check" | "ping" => Command::Check,
            "install" => Command::Install,
            "uninstall" | "remove" => Command::Uninstall,
            "start" => Command::Start,
            "stop" => Command::Stop,
            "restart" => Command::Restart,
            "status" => Command::Status,
            "help" => Command::Help,
            "version" => Command::Version,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub url: String,
    pub api_key: String,
    pub interval: Option<u64>,
    pub timeout: u64,
    pub insecure: bool,
    pub service_name: String,
    pub install_dir: Option<String>,
    pub log_file: Option<String>,
    pub log_level: Level,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            url: String::new(),
            api_key: String::new(),
            interval: None,
            timeout: 10,
            insecure: false,
            service_name: "PulseAgent".to_string(),
            install_dir: None,
            log_file: None,
            log_level: Level::Error,
        }
    }
}

impl Config {
    pub fn from_env() -> Config {
        let mut cfg = Config::default();

        if let Ok(v) = env::var("PULSE_URL") {
            cfg.url = v.trim().to_string();
        }
        if let Ok(v) = env::var("PULSE_API_KEY") {
            cfg.api_key = v.trim().to_string();
        }
        if cfg.api_key.is_empty() {
            if let Ok(v) = env::var("PULSE_KEY") {
                cfg.api_key = v.trim().to_string();
            }
        }
        if let Ok(v) = env::var("PULSE_INTERVAL") {
            if let Ok(n) = v.trim().parse::<u64>() {
                if n > 0 {
                    cfg.interval = Some(n);
                }
            }
        }
        if let Ok(v) = env::var("PULSE_TIMEOUT") {
            if let Ok(n) = v.trim().parse::<u64>() {
                if n > 0 {
                    cfg.timeout = n;
                }
            }
        }
        if let Ok(v) = env::var("PULSE_INSECURE") {
            cfg.insecure = truthy(&v);
        }
        if let Ok(v) = env::var("PULSE_SERVICE_NAME") {
            if !v.trim().is_empty() {
                cfg.service_name = v.trim().to_string();
            }
        }
        if let Ok(v) = env::var("PULSE_INSTALL_DIR") {
            if !v.trim().is_empty() {
                cfg.install_dir = Some(v.trim().to_string());
            }
        }
        if let Ok(v) = env::var("PULSE_LOG_FILE") {
            if !v.trim().is_empty() {
                cfg.log_file = Some(v.trim().to_string());
            }
        }
        if let Ok(v) = env::var("PULSE_LOG_LEVEL") {
            if let Some(level) = Level::parse(&v) {
                cfg.log_level = level;
            }
        }
        if let Ok(v) = env::var("PULSE_VERBOSE") {
            if truthy(&v) {
                cfg.log_level = Level::Debug;
            }
        }

        cfg
    }

    /// Normalised `.../api/v1` base URL.
    pub fn api_base(&self) -> Result<String, String> {
        let raw = self.url.trim();
        if raw.is_empty() {
            return Err("缺少面板地址：请使用 --url 或环境变量 PULSE_URL 指定".to_string());
        }
        if !(raw.starts_with("http://") || raw.starts_with("https://")) {
            return Err(format!("面板地址必须以 http:// 或 https:// 开头：{}", raw));
        }
        let mut base = raw.trim_end_matches('/').to_string();
        if !base.ends_with("/api/v1") {
            base.push_str("/api/v1");
        }
        Ok(base)
    }

    pub fn api_key(&self) -> Result<String, String> {
        if self.api_key.trim().is_empty() {
            return Err("缺少 API KEY：请使用 --key 或环境变量 PULSE_API_KEY 指定".to_string());
        }
        Ok(self.api_key.trim().to_string())
    }
}

fn truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on" | "y"
    )
}

fn take_value(
    args: &[String],
    index: &mut usize,
    inline: Option<String>,
    name: &str,
) -> Result<String, String> {
    if let Some(value) = inline {
        return Ok(value);
    }
    if *index + 1 >= args.len() {
        return Err(format!("选项 {} 缺少参数值", name));
    }
    *index += 1;
    Ok(args[*index].clone())
}

fn parse_positive(value: &str, name: &str) -> Result<u64, String> {
    let parsed: u64 = value
        .trim()
        .parse()
        .map_err(|_| format!("{} 需要一个正整数，收到: {}", name, value))?;
    if parsed == 0 {
        return Err(format!("{} 必须大于 0", name));
    }
    Ok(parsed)
}

/// Parse arguments (excluding the program name).
pub fn parse<I>(args: I) -> Result<(Command, Config), String>
where
    I: IntoIterator<Item = String>,
{
    let args: Vec<String> = args.into_iter().collect();
    let mut cfg = Config::from_env();
    let mut command: Option<Command> = None;
    let mut index = 0usize;

    while index < args.len() {
        let arg = args[index].clone();

        if arg == "-h" || arg == "--help" {
            return Ok((Command::Help, cfg));
        }
        if arg == "-V" || arg == "--version" {
            return Ok((Command::Version, cfg));
        }

        if let Some(cmd) = Command::from_str(&arg) {
            if command.is_some() {
                return Err(format!("重复的命令：{}", arg));
            }
            command = Some(cmd);
            index += 1;
            continue;
        }

        if let Some(rest) = arg.strip_prefix("--") {
            let (name, inline) = match rest.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (rest.to_string(), None),
            };
            match name.as_str() {
                "url" | "server" | "server-url" => {
                    cfg.url = take_value(&args, &mut index, inline, "--url")?;
                }
                "key" | "api-key" => {
                    cfg.api_key = take_value(&args, &mut index, inline, "--key")?;
                }
                "interval" => {
                    let v = take_value(&args, &mut index, inline, "--interval")?;
                    cfg.interval = Some(parse_positive(&v, "--interval")?);
                }
                "timeout" => {
                    let v = take_value(&args, &mut index, inline, "--timeout")?;
                    cfg.timeout = parse_positive(&v, "--timeout")?;
                }
                "service-name" => {
                    cfg.service_name = take_value(&args, &mut index, inline, "--service-name")?;
                }
                "install-dir" => {
                    cfg.install_dir = Some(take_value(&args, &mut index, inline, "--install-dir")?);
                }
                "log-file" => {
                    cfg.log_file = Some(take_value(&args, &mut index, inline, "--log-file")?);
                }
                "insecure" => {
                    if inline.is_some() {
                        return Err("选项 --insecure 不接受参数值".to_string());
                    }
                    cfg.insecure = true;
                }
                "log-level" => {
                    let v = take_value(&args, &mut index, inline, "--log-level")?;
                    cfg.log_level = Level::parse(&v).ok_or_else(|| {
                        format!("无效的日志等级: {}（可选 error|warn|info|debug）", v)
                    })?;
                }
                "verbose" => {
                    if inline.is_some() {
                        return Err("选项 --verbose 不接受参数值".to_string());
                    }
                    cfg.log_level = Level::Debug;
                }
                other => return Err(format!("未知选项：--{}", other)),
            }
            index += 1;
            continue;
        }

        if let Some(rest) = arg.strip_prefix('-') {
            match rest {
                "u" => cfg.url = take_value(&args, &mut index, None, "-u")?,
                "k" => cfg.api_key = take_value(&args, &mut index, None, "-k")?,
                "i" => {
                    let v = take_value(&args, &mut index, None, "-i")?;
                    cfg.interval = Some(parse_positive(&v, "-i")?);
                }
                "v" => cfg.log_level = Level::Debug,
                "" => {}
                _ => return Err(format!("未知选项：-{}", rest)),
            }
            index += 1;
            continue;
        }

        return Err(format!("无法识别的参数：{}", arg));
    }

    Ok((command.unwrap_or(Command::Run), cfg))
}

pub fn help_text() -> String {
    format!(
        r#"{bin} {version} — Pulse 服务器监控上报 Agent

用法:
  {bin} <命令> [选项]

命令:
  run              前台运行，循环采集并上报（默认命令）
  once             只采集并上报一次，打印结果后退出（用于调试）
  check            校验 API KEY，并显示面板返回的服务器信息
  install          安装为系统后台服务并启动
                   Linux: 写入 systemd unit 并 systemctl enable --now
                   Windows: 复制程序到安装目录并注册为 Windows 服务
  uninstall        停止并移除系统服务
  start            启动已安装的服务
  stop             停止服务
  restart          重启服务
  status           查看服务运行状态
  help             显示本帮助（也可用 -h / --help）
  version          显示版本（也可用 -V / --version）

选项:
  -u, --url <URL>          面板地址，例如 https://pulse.example.com
                           环境变量 PULSE_URL
  -k, --key <KEY>          API KEY（pk_ 开头）
                           环境变量 PULSE_API_KEY / PULSE_KEY
  -i, --interval <秒>      固定上报间隔；设置后忽略面板下发的建议值
                           环境变量 PULSE_INTERVAL
      --timeout <秒>       HTTP 超时秒数，默认 10
                           环境变量 PULSE_TIMEOUT
      --insecure           跳过 TLS 证书校验（面板使用自签名证书时）
                           环境变量 PULSE_INSECURE=1
      --service-name <名>  服务名，默认 PulseAgent（Linux 下即 unit 名）
                           环境变量 PULSE_SERVICE_NAME
      --install-dir <目录> 安装目录（覆盖平台默认值）
                           环境变量 PULSE_INSTALL_DIR
      --log-file <文件>     追加输出到日志文件（Windows 服务建议开启）
                           环境变量 PULSE_LOG_FILE
      --log-level <级别>   日志等级：error|warn|info|debug，默认 error
                           环境变量 PULSE_LOG_LEVEL
  -v, --verbose            等价于 --log-level debug
  -h, --help               显示本帮助
  -V, --version            显示版本

环境变量优先级低于命令行参数。

示例:
  {bin} check   --url https://pulse.example.com --key pk_xxxxxxxx
  {bin} once    --url https://pulse.example.com --key pk_xxxxxxxx -v
  {bin} install --url https://pulse.example.com --key pk_xxxxxxxx
  {bin} status
"#,
        bin = BIN_NAME,
        version = VERSION
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_flags_and_command() {
        let (cmd, cfg) = parse(args(&[
            "check",
            "--url",
            "https://panel.example.com/",
            "-k",
            "pk_abc",
            "--interval=30",
        ]))
        .unwrap();
        assert_eq!(cmd, Command::Check);
        assert_eq!(cfg.api_base().unwrap(), "https://panel.example.com/api/v1");
        assert_eq!(cfg.api_key().unwrap(), "pk_abc");
        assert_eq!(cfg.interval, Some(30));
    }

    #[test]
    fn defaults_to_run() {
        let (cmd, _cfg) = parse(args(&["--url", "http://h", "--key", "k"])).unwrap();
        assert_eq!(cmd, Command::Run);
    }

    #[test]
    fn does_not_double_append_api_prefix() {
        let (_cmd, cfg) = parse(args(&["--url", "https://h/api/v1/"])).unwrap();
        assert_eq!(cfg.api_base().unwrap(), "https://h/api/v1");
    }

    #[test]
    fn help_and_version() {
        assert_eq!(parse(args(&["help"])).unwrap().0, Command::Help);
        assert_eq!(parse(args(&["-h"])).unwrap().0, Command::Help);
        assert_eq!(parse(args(&["--version"])).unwrap().0, Command::Version);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse(args(&["run", "--nope"])).is_err());
        assert!(parse(args(&["-i", "0"])).is_err());
        assert!(parse(args(&["--url"])).is_err());
    }
}
