mod log;
mod process;
mod procfile;
mod signal;

use std::sync::{Arc, Mutex};

use anyhow::Result;

fn main() -> Result<()> {
    let procfile_path = std::env::args().nth(1).unwrap_or_else(|| "Procfile".into());
    let procfile = procfile::parse(&procfile_path)?;

    let shutdown = signal::setup()?;

    let names: Vec<String> = procfile.commands.iter().map(|c| c.name.clone()).collect();
    let logger = Arc::new(Mutex::new(log::Logger::new(&names)?));

    let group = process::ProcessGroup::spawn(&procfile.commands, logger)?;
    let exit_code = group.wait_and_shutdown(shutdown);
    std::process::exit(exit_code);
}
