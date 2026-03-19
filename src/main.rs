mod config;
mod config_parser;
mod dependency;
mod fifo;
mod log;
mod output;
mod process;
mod signal;

use std::sync::{Arc, Mutex, mpsc};

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
    procman serve /tmp/myapp.fifo &

    # Scripted service bringup — wait for health, then add a worker
    while ! curl -sf http://localhost:8080/health; do sleep 1; done
    procman start /tmp/myapp.fifo \"redis-server --port 6380\"

    # Gracefully shut down a running server
    procman stop /tmp/myapp.fifo

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
    },

    /// Run procman.yaml commands and listen on a FIFO for dynamically added commands
    ///
    /// Starts all commands from procman.yaml, then listens on the given FIFO
    /// for additional commands sent via `procman start`. The process name is
    /// derived from the program basename.
    Serve {
        /// Path for the named FIFO (created automatically, removed on exit)
        fifo: String,
        /// Path to config file
        #[arg(default_value = "procman.yaml")]
        config: String,
    },

    /// Send a command to a running `procman serve` instance
    ///
    /// Opens the FIFO for writing and sends the command as a JSON message.
    /// Fails immediately if no server is listening.
    Start {
        /// Path to the FIFO of the running server
        fifo: String,
        /// Command line to send — the process name is derived from the program
        /// basename (e.g. "redis-server --port 6380" runs as "redis-server")
        command: String,
    },

    /// Send a shutdown command to a running `procman serve` instance
    Stop {
        /// Path to the FIFO of the running server
        fifo: String,
    },
}

fn build_run_message(command: &str) -> Result<String> {
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
        env: None,
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

fn run_supervisor(config_path: String, fifo_path: Option<String>) -> Result<()> {
    let lock_file =
        std::fs::File::open(&config_path).with_context(|| format!("opening {}", config_path))?;
    let _lock = Flock::lock(lock_file, FlockArg::LockExclusiveNonblock).map_err(|(_, errno)| {
        anyhow::anyhow!(
            "another procman instance appears to be running (could not lock {}): {}",
            config_path,
            errno
        )
    })?;

    let configs = config_parser::parse(&config_path)?;

    let shutdown = signal::setup()?;

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

    let group =
        process::ProcessGroup::spawn(&configs, tx, Arc::clone(&shutdown), Arc::clone(&logger))?;
    let exit_code = group.wait_and_shutdown(shutdown, rx, Arc::clone(&logger));

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
    });

    match command {
        Commands::Start { fifo, command } => {
            let payload = build_run_message(&command)?;
            write_to_fifo(&fifo, &payload)
        }
        Commands::Stop { fifo } => {
            let payload = build_shutdown_message();
            write_to_fifo(&fifo, &payload)
        }
        Commands::Run { config } => run_supervisor(config, None),
        Commands::Serve { config, fifo } => run_supervisor(config, Some(fifo)),
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

        let payload = build_run_message("sleep 123").unwrap();
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

        let payload = build_run_message("cat /etc/hostname").unwrap();
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
}
