mod args;
mod checks;
mod config;
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

use anyhow::{Context, Result};
use clap::Parser;

#[derive(Parser)]
#[command(
    version,
    about = "A process supervisor driven by a .pman process definition file",
    after_help = "\
EXAMPLES:
    # Run all jobs defined in a config file
    procman myapp.pman

    # Pass extra environment variables
    procman myapp.pman -e PORT=3000 -e DEBUG=1

    # Pass user-defined args (defined in config args: section)
    procman myapp.pman -- --rust-log debug --enable-feature

    # Pause shutdown on child failure for interactive debugging
    procman myapp.pman --debug

    # Validate the config file and exit without starting processes
    procman myapp.pman --check

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
    /// Validate the config file and exit without starting processes
    #[arg(long)]
    check: bool,
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
    check: bool,
) -> Result<()> {
    if debug {
        anyhow::ensure!(
            std::io::IsTerminal::is_terminal(&std::io::stdin()),
            "--debug requires an interactive terminal"
        );
    }

    // Phase 1: parse config header for arg definitions
    let content =
        std::fs::read_to_string(&config_path).with_context(|| format!("reading {config_path}"))?;
    let header = pman::parse_header(&content, &config_path)?;

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
    let (configs, _) = pman::parse(&content, &config_path, &merged_env, &arg_values)?;

    if check {
        println!("{config_path}: ok");
        return Ok(());
    }

    let (shutdown, signal_triggered) = signal::setup()?;

    let mut names: Vec<String> = configs.iter().map(|c| c.name.clone()).collect();
    names.insert(0, "procman".to_string());
    let logger = Arc::new(Mutex::new(log::Logger::new(
        &names,
        header.log_dir.as_deref(),
        header.log_time,
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
    run_supervisor(cli.file, extra_env, cli.user_args, cli.debug, cli.check)
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

    #[test]
    fn check_flag_valid_pman() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.pman");
        std::fs::write(&path, r#"job web { run "echo hello" }"#).unwrap();
        let config_path = path.to_str().unwrap().to_string();
        let result = run_supervisor(config_path, HashMap::new(), vec![], false, true);
        assert!(result.is_ok());
    }

    #[test]
    fn check_flag_invalid_pman() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.pman");
        std::fs::write(&path, "this is not valid pman syntax !!!").unwrap();
        let config_path = path.to_str().unwrap().to_string();
        let result = run_supervisor(config_path, HashMap::new(), vec![], false, true);
        assert!(result.is_err());
    }

    #[test]
    fn check_flag_with_imports() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.pman");
        std::fs::write(&db_path, r#"job migrate { run "migrate" }"#).unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "db.pman" as db
            service api {
                wait { after @db::migrate }
                run "serve"
            }
            "#,
        )
        .unwrap();

        let config_path = root_path.to_str().unwrap().to_string();
        let result = run_supervisor(config_path, HashMap::new(), vec![], false, true);
        assert!(result.is_ok(), "got: {}", result.unwrap_err());
    }
}
