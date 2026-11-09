//! `pulse-agent` — lightweight server metric reporter for the Pulse panel.

mod agent;
mod cli;
mod http;
mod json;
mod log;
mod metrics;
mod service;
mod util;

use std::process::ExitCode;

use cli::{Command, Config};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let (command, cfg) = match cli::parse(args) {
        Ok(parsed) => parsed,
        Err(err) => {
            eprintln!("错误: {}", err);
            eprintln!("使用 `{} help` 查看用法。", cli::BIN_NAME);
            return ExitCode::from(2);
        }
    };

    log::set_level(cfg.log_level);
    log::set_log_file(cfg.log_file.clone());

    // Fail fast with a clear message for the commands that need panel credentials.
    if matches!(
        command,
        Command::Run | Command::Once | Command::Check | Command::Install
    ) {
        if let Err(err) = cfg.api_base().and_then(|_| cfg.api_key().map(|_| ())) {
            log::error(&format!("配置错误: {}", err));
            return ExitCode::from(2);
        }
    }

    let result = match command {
        Command::Help => {
            print!("{}", cli::help_text());
            Ok(())
        }
        Command::Version => {
            println!("{} {}", cli::BIN_NAME, cli::VERSION);
            Ok(())
        }
        Command::Run => service::run(&cfg),
        Command::Once => run_once(&cfg),
        Command::Check => run_check(&cfg),
        Command::Install => service::install(&cfg),
        Command::Uninstall => service::uninstall(&cfg),
        Command::Start => service::start(&cfg),
        Command::Stop => service::stop(&cfg),
        Command::Restart => service::restart(&cfg),
        Command::Status => service::status(&cfg),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("错误: {}", err);
            ExitCode::from(1)
        }
    }
}

fn run_once(cfg: &Config) -> Result<(), String> {
    let reporter = agent::Agent::new(cfg)?;
    let outcome = reporter.report()?;
    println!("上报成功。");
    if let Some(interval) = outcome.recommended_interval {
        println!("面板建议间隔: {} 秒", interval);
    }
    Ok(())
}

fn run_check(cfg: &Config) -> Result<(), String> {
    let reporter = agent::Agent::new(cfg)?;
    let info = reporter.check()?;

    println!("认证成功。面板返回:");
    println!("{}", info);

    if let Some(name) = info.get_path(&["server", "name"]).and_then(|v| v.as_str()) {
        println!("服务器名称: {}", name);
    }
    if let Some(interval) = info.get("report_interval").and_then(|v| v.as_u64()) {
        println!("建议上报间隔: {} 秒", interval);
    }
    Ok(())
}
