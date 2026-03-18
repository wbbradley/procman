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
