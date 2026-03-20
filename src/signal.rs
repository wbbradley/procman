use std::sync::{Arc, atomic::AtomicBool};

use anyhow::{Context, Result};
use signal_hook::{
    consts::{SIGINT, SIGTERM},
    flag,
};

pub fn setup() -> Result<(Arc<AtomicBool>, Arc<AtomicBool>)> {
    let shutdown = Arc::new(AtomicBool::new(false));
    let signal_triggered = Arc::new(AtomicBool::new(false));
    flag::register(SIGINT, Arc::clone(&shutdown)).context("registering SIGINT handler")?;
    flag::register(SIGTERM, Arc::clone(&shutdown)).context("registering SIGTERM handler")?;
    flag::register(SIGINT, Arc::clone(&signal_triggered))
        .context("registering SIGINT signal_triggered")?;
    flag::register(SIGTERM, Arc::clone(&signal_triggered))
        .context("registering SIGTERM signal_triggered")?;
    Ok((shutdown, signal_triggered))
}
