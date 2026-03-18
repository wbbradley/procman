mod command_parser;
mod config;
mod config_parser;
mod dependency;
mod fifo;
mod log;
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
    /// Opens the FIFO for writing and sends the command line. Fails immediately
    /// if no server is listening. The server parses the command using the same
    /// rules as command lines (including env var substitution).
    Start {
        /// Path to the FIFO of the running server
        fifo: String,
        /// Command line to send — the process name is derived from the program
        /// basename (e.g. "redis-server --port 6380" runs as "redis-server")
        command: String,
    },
}

fn run_client(fifo_path: &str, command: &str) -> Result<()> {
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
    writeln!(file, "{command}")?;
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
    let parser = command_parser::CommandParser::new();

    let shutdown = signal::setup()?;

    let mut names: Vec<String> = configs.iter().map(|c| c.name.clone()).collect();
    names.insert(0, "procman".to_string());
    let logger = Arc::new(Mutex::new(log::Logger::new(&names)?));

    let mode = if fifo_path.is_some() { "serve" } else { "run" };
    logger.lock().unwrap().log_line(
        "procman",
        &format!("started with {} process(es), mode={mode}", configs.len()),
    );

    let (tx, rx) = mpsc::channel::<config::ProcessConfig>();

    let fifo_server = if let Some(ref fifo_path) = fifo_path {
        let parser = Arc::new(Mutex::new(parser));
        let server = fifo::FifoServer::start(
            fifo_path.clone(),
            tx.clone(),
            parser,
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
        Commands::Start { fifo, command } => run_client(&fifo, &command),
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

    /// Retry run_client until the reader thread is blocked in File::open.
    /// Each failed attempt (ENXIO) is a no-op; the first success writes exactly once.
    fn run_client_until_ready(path: &str, command: &str) {
        for _ in 0..100_000 {
            if run_client(path, command).is_ok() {
                return;
            }
            std::thread::yield_now();
        }
        panic!("run_client never succeeded — reader never became ready");
    }

    #[test]
    fn run_client_writes_command() {
        use std::io::Read;

        let path = test_fifo_path("writes_cmd");
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

        run_client_until_ready(&path, "sleep 123");

        let received = reader.join().unwrap();
        assert_eq!(received, "sleep 123\n");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn run_client_error_no_fifo() {
        let path = test_fifo_path("no_fifo");
        let _ = std::fs::remove_file(&path);
        let result = run_client(&path, "sleep 1");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no procman server listening"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn run_client_error_no_reader() {
        let path = test_fifo_path("no_reader");
        let _ = std::fs::remove_file(&path);
        nix::unistd::mkfifo(
            path.as_str(),
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();

        let result = run_client(&path, "sleep 1");
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

        let parser = Arc::new(Mutex::new(command_parser::CommandParser::new()));
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
            Arc::clone(&parser),
            Arc::clone(&shutdown),
            Arc::clone(&logger),
        )
        .unwrap();

        run_client_until_ready(&fifo_path, "cat /etc/hostname");

        let cmd = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert_eq!(cmd.program, "cat");
        assert_eq!(cmd.args, vec!["/etc/hostname"]);

        server.stop();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
