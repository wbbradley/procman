mod args;
mod checks;
mod config;
mod config_parser;
mod dependency;
mod log;
mod output;
mod pman;
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
    # Run all jobs defined in a config file
    procman procman.yaml

    # Pass extra environment variables
    procman myfile.yaml -e PORT=3000 -e DEBUG=1

    # Pass user-defined args (defined in config args: section)
    procman myfile.yaml -- --rust-log debug --enable-feature

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
    /// Arguments for config-defined args (passed after --)
    #[arg(last = true)]
    user_args: Vec<String>,
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
    user_args: Vec<String>,
    debug: bool,
) -> Result<()> {
    if debug {
        anyhow::ensure!(
            std::io::IsTerminal::is_terminal(&std::io::stdin()),
            "--debug requires an interactive terminal"
        );
    }

    // Phase 1: parse config header for arg definitions
    let header = config_parser::parse_header(&config_path)?;

    // Phase 2: parse user args using definitions
    let arg_values = args::parse_user_args(&user_args, &header.arg_defs)?;

    // Phase 3: build env with correct precedence
    // arg env vars (defaults + user overrides) < -e flags
    let mut merged_env = HashMap::new();
    for def in &header.arg_defs {
        if let Some(ref env_name) = def.env
            && let Some(value) = arg_values.get(&def.name)
        {
            merged_env.insert(env_name.clone(), value.clone());
        }
    }
    merged_env.extend(extra_env);

    // Phase 4: full config parse with arg values for template resolution
    let (configs, _) = config_parser::parse(&config_path, &merged_env, &arg_values)?;

    let (shutdown, signal_triggered) = signal::setup()?;

    let mut names: Vec<String> = configs.iter().map(|c| c.name.clone()).collect();
    names.insert(0, "procman".to_string());
    let logger = Arc::new(Mutex::new(log::Logger::new(
        &names,
        header.log_dir.as_deref(),
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
    run_supervisor(cli.file, extra_env, cli.user_args, cli.debug)
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
