mod config;
mod config_parser;
mod dependency;
mod fifo;
mod fifo_path;
mod log;
mod output;
mod process;
mod signal;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, mpsc},
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use nix::fcntl::{Flock, FlockArg};

#[derive(Parser)]
#[command(
    version,
    about = "A process supervisor driven by procman.yaml",
    after_help = "\
EXAMPLES:
    # Run all procman.yaml commands (default)
    procman run

    # Run procman.yaml commands and accept dynamic additions via a FIFO
    procman serve &

    # Scripted service bringup — wait for health, then add a worker
    while ! curl -sf http://localhost:8080/health; do sleep 1; done
    procman start \"redis-server --port 6380\"

    # Gracefully shut down a running server
    procman stop

SIGNALS:
    On SIGINT or SIGTERM, all children receive SIGTERM. After a 2-second
    grace period, remaining processes are sent SIGKILL."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Spawn all procman.yaml commands and wait for exit or signal
    Run {
        /// Path to config file
        #[arg(default_value = "procman.yaml")]
        config: String,
        /// Extra environment variables (repeatable, KEY=VALUE format)
        #[arg(short = 'e', long = "env", value_name = "KEY=VALUE")]
        env: Vec<String>,
        /// Pause shutdown on child failure for interactive debugging
        #[arg(long)]
        debug: bool,
    },

    /// Run procman.yaml commands and listen on a FIFO for dynamically added commands
    ///
    /// Starts all commands from procman.yaml, then listens on a FIFO (auto-derived
    /// from the config path) for additional commands sent via `procman start`.
    Serve {
        /// Path to config file
        #[arg(default_value = "procman.yaml")]
        config: String,
        /// Extra environment variables (repeatable, KEY=VALUE format)
        #[arg(short = 'e', long = "env", value_name = "KEY=VALUE")]
        env: Vec<String>,
        /// Pause shutdown on child failure for interactive debugging
        #[arg(long)]
        debug: bool,
    },

    /// Send a command to a running `procman serve` instance
    ///
    /// Opens the FIFO (auto-derived from the config path) for writing and sends
    /// the command as a JSON message. Fails immediately if no server is listening.
    Start {
        /// Command line to send — the process name is derived from the program
        /// basename (e.g. "redis-server --port 6380" runs as "redis-server")
        command: String,
        /// Path to config file
        #[arg(long, default_value = "procman.yaml")]
        config: String,
        /// Extra environment variables (repeatable, KEY=VALUE format)
        #[arg(short = 'e', long = "env", value_name = "KEY=VALUE")]
        env: Vec<String>,
    },

    /// Send a shutdown command to a running `procman serve` instance
    Stop {
        /// Path to config file
        #[arg(default_value = "procman.yaml")]
        config: String,
    },
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

fn build_run_message(command: &str, extra_env: &HashMap<String, String>) -> Result<String> {
    let tokens =
        shell_words::split(command).with_context(|| format!("parsing command: {command}"))?;
    anyhow::ensure!(!tokens.is_empty(), "empty command");
    let name = std::path::Path::new(&tokens[0])
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| tokens[0].clone());
    let msg = fifo::FifoMessage::Run {
        name,
        run: command.to_string(),
        env: if extra_env.is_empty() {
            None
        } else {
            Some(extra_env.clone())
        },
        depends: None,
        once: None,
    };
    Ok(serde_json::to_string(&msg)?)
}

fn build_shutdown_message() -> String {
    let msg = fifo::FifoMessage::Shutdown {
        user: std::env::var("USER").ok(),
        message: Some("User-initiated via CLI".to_string()),
    };
    serde_json::to_string(&msg).unwrap()
}

fn write_to_fifo(fifo_path: &str, payload: &str) -> Result<()> {
    use std::io::Write;

    use nix::{
        fcntl::{OFlag, open},
        sys::stat::Mode,
    };

    let fd = open(
        fifo_path,
        OFlag::O_WRONLY | OFlag::O_NONBLOCK,
        Mode::empty(),
    )
    .map_err(|_| anyhow::anyhow!("no procman server listening on {fifo_path}"))?;

    let mut file = std::fs::File::from(fd);
    writeln!(file, "{payload}")?;
    Ok(())
}

fn run_supervisor(
    config_path: String,
    fifo_path: Option<String>,
    extra_env: HashMap<String, String>,
    debug: bool,
) -> Result<()> {
    if debug {
        anyhow::ensure!(
            std::io::IsTerminal::is_terminal(&std::io::stdin()),
            "--debug requires an interactive terminal"
        );
    }

    let lock_file =
        std::fs::File::open(&config_path).with_context(|| format!("opening {}", config_path))?;
    let _lock = Flock::lock(lock_file, FlockArg::LockExclusiveNonblock).map_err(|(_, errno)| {
        anyhow::anyhow!(
            "another procman instance appears to be running (could not lock {}): {}",
            config_path,
            errno
        )
    })?;

    let configs = config_parser::parse(&config_path, &extra_env)?;

    let (shutdown, signal_triggered) = signal::setup()?;

    let mut names: Vec<String> = configs.iter().map(|c| c.name.clone()).collect();
    names.insert(0, "procman".to_string());
    let logger = Arc::new(Mutex::new(log::Logger::new(&names)?));

    let mode = if fifo_path.is_some() { "serve" } else { "run" };
    logger.lock().unwrap().log_line(
        "procman",
        &format!("started with {} process(es), mode={mode}", configs.len()),
    );

    let (tx, rx) = mpsc::channel::<config::SupervisorCommand>();

    let fifo_server = if let Some(ref fifo_path) = fifo_path {
        let server = fifo::FifoServer::start(
            fifo_path.clone(),
            tx.clone(),
            Arc::clone(&shutdown),
            Arc::clone(&logger),
        )?;
        logger
            .lock()
            .unwrap()
            .log_line("procman", &format!("FIFO server listening on {fifo_path}"));
        Some(server)
    } else {
        None
    };

    let group = process::ProcessGroup::spawn(
        &configs,
        tx,
        Arc::clone(&shutdown),
        Arc::clone(&logger),
        debug,
        fifo_path.is_some(),
    )?;
    let exit_code = group.wait_and_shutdown(shutdown, signal_triggered, rx, Arc::clone(&logger));

    logger.lock().unwrap().log_line(
        "procman",
        &format!("shutting down with exit code {exit_code}"),
    );

    if let Some(server) = fifo_server {
        server.stop();
    }

    std::process::exit(exit_code);
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let command = cli.command.unwrap_or(Commands::Run {
        config: "procman.yaml".to_string(),
        env: Vec::new(),
        debug: false,
    });

    match command {
        Commands::Start {
            config,
            command,
            env,
        } => {
            let fifo = fifo_path::derive_fifo_path(&config)?;
            let extra_env = parse_env_args(&env)?;
            let payload = build_run_message(&command, &extra_env)?;
            write_to_fifo(fifo.to_str().unwrap(), &payload)
        }
        Commands::Stop { config } => {
            let fifo = fifo_path::derive_fifo_path(&config)?;
            let payload = build_shutdown_message();
            write_to_fifo(fifo.to_str().unwrap(), &payload)
        }
        Commands::Run { config, env, debug } => {
            let extra_env = parse_env_args(&env)?;
            run_supervisor(config, None, extra_env, debug)
        }
        Commands::Serve { config, env, debug } => {
            let fifo = fifo_path::derive_fifo_path(&config)?;
            let extra_env = parse_env_args(&env)?;
            run_supervisor(
                config,
                Some(fifo.to_str().unwrap().to_string()),
                extra_env,
                debug,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;

    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn test_fifo_path(name: &str) -> String {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!(
                "procman_test_main_{name}_{}_{id}",
                std::process::id()
            ))
            .to_str()
            .unwrap()
            .to_string()
    }

    /// Retry write_to_fifo until the reader thread is blocked in File::open.
    /// Each failed attempt (ENXIO) is a no-op; the first success writes exactly once.
    fn write_to_fifo_until_ready(path: &str, payload: &str) {
        for _ in 0..100_000 {
            if write_to_fifo(path, payload).is_ok() {
                return;
            }
            std::thread::yield_now();
        }
        panic!("write_to_fifo never succeeded — reader never became ready");
    }

    #[test]
    fn write_to_fifo_writes_json() {
        use std::io::Read;

        let path = test_fifo_path("writes_json");
        let _ = std::fs::remove_file(&path);
        nix::unistd::mkfifo(
            path.as_str(),
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();

        let reader_path = path.clone();
        let reader = std::thread::spawn(move || {
            let mut f = std::fs::File::open(&reader_path).unwrap();
            let mut buf = String::new();
            f.read_to_string(&mut buf).unwrap();
            buf
        });

        let payload = build_run_message("sleep 123", &HashMap::new()).unwrap();
        write_to_fifo_until_ready(&path, &payload);

        let received = reader.join().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(received.trim()).unwrap();
        assert_eq!(parsed["type"], "run");
        assert_eq!(parsed["name"], "sleep");
        assert_eq!(parsed["run"], "sleep 123");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn write_to_fifo_error_no_fifo() {
        let path = test_fifo_path("no_fifo");
        let _ = std::fs::remove_file(&path);
        let result = write_to_fifo(&path, "test");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no procman server listening"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn write_to_fifo_error_no_reader() {
        let path = test_fifo_path("no_reader");
        let _ = std::fs::remove_file(&path);
        nix::unistd::mkfifo(
            path.as_str(),
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();

        let result = write_to_fifo(&path, "test");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no procman server listening"),
            "unexpected error: {err}"
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn integration_server_and_client() {
        let fifo_path = test_fifo_path("integration");
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("procman_integration_{}_{id}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("procman.yaml");
        std::fs::write(&config_path, "placeholder:\n  run: echo placeholder\n").unwrap();

        let log_dir = dir.join("logs");
        let logger = Arc::new(Mutex::new(
            log::Logger::new_for_test(&["fifo".to_string(), "procman".to_string()], log_dir)
                .unwrap(),
        ));

        let (tx, rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));

        let server = fifo::FifoServer::start(
            fifo_path.clone(),
            tx,
            Arc::clone(&shutdown),
            Arc::clone(&logger),
        )
        .unwrap();

        let payload = build_run_message("cat /etc/hostname", &HashMap::new()).unwrap();
        write_to_fifo_until_ready(&fifo_path, &payload);

        let cmd = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        match cmd {
            config::SupervisorCommand::Spawn(config) => {
                assert_eq!(config.run, "cat /etc/hostname");
            }
            _ => panic!("expected Spawn"),
        }

        server.stop();
        std::fs::remove_dir_all(&dir).unwrap();
    }

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
    fn build_run_message_includes_env() {
        let mut env = HashMap::new();
        env.insert("FOO".to_string(), "bar".to_string());
        let json = build_run_message("echo hello", &env).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["env"]["FOO"], "bar");
    }

    #[test]
    fn build_run_message_omits_empty_env() {
        let json = build_run_message("echo hello", &HashMap::new()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed["env"].is_null());
    }
}
