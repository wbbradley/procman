mod checks;
mod config;
mod config_parser;
mod dependency;
mod log;
mod output;
mod process;
mod signal;
mod watch;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, mpsc},
};

use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(
    version,
    about = "A process supervisor driven by a YAML config file",
    after_help = "\
EXAMPLES:
    # Run all jobs defined in procman.yaml
    procman procman.yaml

    # Pass extra environment variables
    procman myfile.yaml -e PORT=3000 -e DEBUG=1

    # Pause shutdown on child failure for interactive debugging
    procman procman.yaml --debug

SIGNALS:
    On SIGINT or SIGTERM, all children receive SIGTERM. After a 2-second
    grace period, remaining processes are sent SIGKILL."
)]
struct Cli {
    /// Path to config file
    file: String,
    /// Extra environment variables (repeatable, KEY=VALUE format)
    #[arg(short = 'e', long = "env", value_name = "KEY=VALUE")]
    env: Vec<String>,
    /// Pause shutdown on child failure for interactive debugging
    #[arg(long)]
    debug: bool,
}

fn parse_env_args(args: &[String]) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for arg in args {
        let (key, value) = arg
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("invalid env argument (expected KEY=VALUE): {arg}"))?;
        if key.is_empty() {
            anyhow::bail!("invalid env argument (empty key): {arg}");
        }
        map.insert(key.to_string(), value.to_string());
    }
    Ok(map)
}

fn run_supervisor(
    config_path: String,
    extra_env: HashMap<String, String>,
    debug: bool,
) -> Result<()> {
    if debug {
        anyhow::ensure!(
            std::io::IsTerminal::is_terminal(&std::io::stdin()),
            "--debug requires an interactive terminal"
        );
    }

    let (configs, custom_log_dir) = config_parser::parse(&config_path, &extra_env)?;

    let (shutdown, signal_triggered) = signal::setup()?;

    let mut names: Vec<String> = configs.iter().map(|c| c.name.clone()).collect();
    names.insert(0, "procman".to_string());
    let logger = Arc::new(Mutex::new(log::Logger::new(
        &names,
        custom_log_dir.as_deref(),
    )?));

    logger
        .lock()
        .unwrap()
        .log_line("procman", &format!("started with {} job(s)", configs.len()));

    let (tx, rx) = mpsc::channel::<config::SupervisorCommand>();

    let group = process::ProcessGroup::spawn(
        &configs,
        tx,
        Arc::clone(&shutdown),
        Arc::clone(&logger),
        debug,
    )?;
    let exit_code = group.wait_and_shutdown(shutdown, signal_triggered, rx, Arc::clone(&logger));

    logger.lock().unwrap().log_line(
        "procman",
        &format!("shutting down with exit code {exit_code}"),
    );

    std::process::exit(exit_code);
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let extra_env = parse_env_args(&cli.env)?;
    run_supervisor(cli.file, extra_env, cli.debug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_env_args_valid() {
        let args = vec!["FOO=bar".to_string(), "BAZ=qux".to_string()];
        let map = parse_env_args(&args).unwrap();
        assert_eq!(map.get("FOO").unwrap(), "bar");
        assert_eq!(map.get("BAZ").unwrap(), "qux");
    }

    #[test]
    fn parse_env_args_empty_value() {
        let args = vec!["KEY=".to_string()];
        let map = parse_env_args(&args).unwrap();
        assert_eq!(map.get("KEY").unwrap(), "");
    }

    #[test]
    fn parse_env_args_missing_equals() {
        let args = vec!["NOEQUALS".to_string()];
        let err = parse_env_args(&args).unwrap_err().to_string();
        assert!(err.contains("KEY=VALUE"), "unexpected error: {err}");
    }

    #[test]
    fn parse_env_args_empty_key() {
        let args = vec!["=value".to_string()];
        let err = parse_env_args(&args).unwrap_err().to_string();
        assert!(err.contains("empty key"), "unexpected error: {err}");
    }

    #[test]
    fn parse_env_args_equals_in_value() {
        let args = vec!["URL=http://host:8080/path?a=1".to_string()];
        let map = parse_env_args(&args).unwrap();
        assert_eq!(map.get("URL").unwrap(), "http://host:8080/path?a=1");
    }
}
