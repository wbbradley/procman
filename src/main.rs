mod fifo;
mod log;
mod process;
mod procfile;
mod signal;

use std::sync::{Arc, Mutex, mpsc};

use anyhow::{Context, Result};
use clap::Parser;
use nix::fcntl::{Flock, FlockArg};

#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Path to Procfile
    #[arg(default_value = "Procfile")]
    procfile: String,

    /// Run in server mode, listening on the named FIFO
    #[arg(short, long)]
    server: Option<String>,

    /// Send a command to a running server via the named FIFO
    #[arg(short, long, conflicts_with = "server")]
    client: Option<String>,

    /// Command string to send (used with --client)
    #[arg(requires = "client")]
    command: Option<String>,
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

fn main() -> Result<()> {
    let cli = Cli::parse();

    if let Some(ref fifo_path) = cli.client {
        let command = cli.command.as_deref().unwrap_or("");
        return run_client(fifo_path, command);
    }

    let lock_file =
        std::fs::File::open(&cli.procfile).with_context(|| format!("opening {}", cli.procfile))?;
    let _lock = Flock::lock(lock_file, FlockArg::LockExclusiveNonblock).map_err(|(_, errno)| {
        anyhow::anyhow!(
            "another procman instance appears to be running (could not lock {}): {}",
            cli.procfile,
            errno
        )
    })?;

    let (procfile, parser) = procfile::parse(&cli.procfile)?;

    let shutdown = signal::setup()?;

    let names: Vec<String> = procfile.commands.iter().map(|c| c.name.clone()).collect();
    let logger = Arc::new(Mutex::new(log::Logger::new(&names)?));

    let (tx, rx) = mpsc::channel::<procfile::Command>();

    let fifo_server = if let Some(ref fifo_path) = cli.server {
        let parser = Arc::new(Mutex::new(parser));
        Some(fifo::FifoServer::start(
            fifo_path.clone(),
            tx,
            parser,
            Arc::clone(&shutdown),
            Arc::clone(&logger),
        )?)
    } else {
        drop(tx);
        None
    };

    let group = process::ProcessGroup::spawn(&procfile.commands, Arc::clone(&logger))?;
    let exit_code = group.wait_and_shutdown(shutdown, rx, logger);

    if let Some(server) = fifo_server {
        server.stop();
    }

    std::process::exit(exit_code);
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

        std::thread::sleep(std::time::Duration::from_millis(50));
        run_client(&path, "sleep 123").unwrap();

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
        let procfile_path = dir.join("Procfile");
        std::fs::write(&procfile_path, "echo placeholder\n").unwrap();

        let (_pf, parser) = procfile::parse(procfile_path.to_str().unwrap()).unwrap();
        let parser = Arc::new(Mutex::new(parser));
        let logger = Arc::new(Mutex::new(log::Logger::new(&["fifo".to_string()]).unwrap()));

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

        std::thread::sleep(std::time::Duration::from_millis(50));
        run_client(&fifo_path, "cat /etc/hostname").unwrap();

        let cmd = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert_eq!(cmd.program, "cat");
        assert_eq!(cmd.args, vec!["/etc/hostname"]);

        server.stop();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
