use std::sync::{Arc, atomic::AtomicBool};

use anyhow::{Context, Result};
use signal_hook::{
    consts::{SIGINT, SIGTERM},
    flag,
};

pub fn setup() -> Result<Arc<AtomicBool>> {
    let shutdown = Arc::new(AtomicBool::new(false));
    flag::register(SIGINT, Arc::clone(&shutdown)).context("registering SIGINT handler")?;
    flag::register(SIGTERM, Arc::clone(&shutdown)).context("registering SIGTERM handler")?;
    Ok(shutdown)
}
