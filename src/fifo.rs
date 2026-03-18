use std::{
    fs::File,
    io::{BufRead, BufReader},
    sync::{
        Arc,
        Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use nix::{sys::stat::Mode, unistd::mkfifo};

use crate::{
    log::Logger,
    procfile::{Command, CommandParser},
};

pub struct FifoServer {
    path: String,
    thread: Option<thread::JoinHandle<()>>,
}

impl FifoServer {
    pub fn start(
        path: String,
        tx: mpsc::Sender<Command>,
        parser: Arc<Mutex<CommandParser>>,
        shutdown: Arc<AtomicBool>,
        logger: Arc<Mutex<Logger>>,
    ) -> Result<Self> {
        // Delete stale FIFO if it exists (we hold the advisory lock, so it's safe)
        let _ = std::fs::remove_file(&path);

        mkfifo(path.as_str(), Mode::S_IRUSR | Mode::S_IWUSR)
            .with_context(|| format!("creating FIFO at {path}"))?;

        let fifo_path = path.clone();
        let thread = thread::spawn(move || {
            Self::reader_loop(&fifo_path, &tx, &parser, &shutdown, &logger);
        });

        Ok(Self {
            path,
            thread: Some(thread),
        })
    }

    fn reader_loop(
        path: &str,
        tx: &mpsc::Sender<Command>,
        parser: &Arc<Mutex<CommandParser>>,
        shutdown: &Arc<AtomicBool>,
        logger: &Arc<Mutex<Logger>>,
    ) {
        while !shutdown.load(Ordering::Relaxed) {
            // Open FIFO — blocks until a writer connects
            let file = match File::open(path) {
                Ok(f) => f,
                Err(_) => {
                    if shutdown.load(Ordering::Relaxed) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
            };

            let reader = BufReader::new(file);
            for line in reader.lines() {
                if shutdown.load(Ordering::Relaxed) {
                    return;
                }
                let line = match line {
                    Ok(l) => l,
                    Err(_) => break,
                };
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }

                let cmd = match parser.lock().unwrap().parse_command_line(trimmed) {
                    Ok(cmd) => cmd,
                    Err(e) => {
                        logger
                            .lock()
                            .unwrap()
                            .log_line("fifo", &format!("parse error: {e}"));
                        continue;
                    }
                };

                if tx.send(cmd).is_err() {
                    return;
                }
            }
            // EOF — writer disconnected, loop back to re-open for next client
        }
    }

    pub fn stop(mut self) {
        // Open FIFO for writing to unblock the reader thread if it's stuck in open()
        let _ = File::create(&self.path);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}
