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
use nix::{
    fcntl::{OFlag, open},
    sys::stat::Mode,
    unistd::mkfifo,
};

use crate::{config::ProcessConfig, log::Logger, procfile::CommandParser};

pub struct FifoServer {
    path: String,
    shutdown: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl FifoServer {
    pub fn start(
        path: String,
        tx: mpsc::Sender<ProcessConfig>,
        parser: Arc<Mutex<CommandParser>>,
        shutdown: Arc<AtomicBool>,
        logger: Arc<Mutex<Logger>>,
    ) -> Result<Self> {
        // Delete stale FIFO if it exists (we hold the advisory lock, so it's safe)
        let _ = std::fs::remove_file(&path);

        mkfifo(path.as_str(), Mode::S_IRUSR | Mode::S_IWUSR)
            .with_context(|| format!("creating FIFO at {path}"))?;

        let fifo_path = path.clone();
        let shutdown_clone = Arc::clone(&shutdown);
        let thread = thread::spawn(move || {
            Self::reader_loop(&fifo_path, &tx, &parser, &shutdown_clone, &logger);
        });

        Ok(Self {
            path,
            shutdown,
            thread: Some(thread),
        })
    }

    fn reader_loop(
        path: &str,
        tx: &mpsc::Sender<ProcessConfig>,
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
        self.shutdown.store(true, Ordering::Relaxed);
        // Open FIFO for writing (non-blocking) to unblock the reader if it's in open().
        // If the reader already exited, the open fails with ENXIO — that's fine.
        let _ = open(
            self.path.as_str(),
            OFlag::O_WRONLY | OFlag::O_NONBLOCK,
            Mode::empty(),
        );
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use std::{io::Write, sync::atomic::AtomicUsize};

    use super::*;

    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn test_fifo_path(name: &str) -> String {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!(
                "procman_test_fifo_{name}_{}_{id}",
                std::process::id()
            ))
            .to_str()
            .unwrap()
            .to_string()
    }

    fn make_parser_and_logger() -> (Arc<Mutex<CommandParser>>, Arc<Mutex<Logger>>) {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("procman_parser_{}_{id}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Procfile");
        std::fs::write(&path, "echo placeholder\n").unwrap();
        let (_pf, parser) = crate::procfile::parse(path.to_str().unwrap()).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let logger = Logger::new(&["fifo".to_string()]).unwrap();
        (Arc::new(Mutex::new(parser)), Arc::new(Mutex::new(logger)))
    }

    #[test]
    fn fifo_lifecycle_creates_and_cleans_up() {
        let path = test_fifo_path("lifecycle");
        let (tx, _rx) = mpsc::channel();
        let (parser, logger) = make_parser_and_logger();
        let shutdown = Arc::new(AtomicBool::new(false));
        let server =
            FifoServer::start(path.clone(), tx, parser, Arc::clone(&shutdown), logger).unwrap();
        assert!(std::path::Path::new(&path).exists());
        server.stop();
        assert!(!std::path::Path::new(&path).exists());
    }

    #[test]
    fn fifo_receives_single_command() {
        let path = test_fifo_path("single_cmd");
        let (tx, rx) = mpsc::channel();
        let (parser, logger) = make_parser_and_logger();
        let shutdown = Arc::new(AtomicBool::new(false));
        let server =
            FifoServer::start(path.clone(), tx, parser, Arc::clone(&shutdown), logger).unwrap();

        {
            let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            writeln!(f, "sleep 42").unwrap();
        }

        let cmd = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(cmd.program, "sleep");
        assert_eq!(cmd.args, vec!["42"]);

        server.stop();
    }

    #[test]
    fn fifo_receives_multiple_commands_in_order() {
        let path = test_fifo_path("multi_cmd");
        let (tx, rx) = mpsc::channel();
        let (parser, logger) = make_parser_and_logger();
        let shutdown = Arc::new(AtomicBool::new(false));
        let server =
            FifoServer::start(path.clone(), tx, parser, Arc::clone(&shutdown), logger).unwrap();

        {
            let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            writeln!(f, "sleep 1").unwrap();
            writeln!(f, "sleep 2").unwrap();
            writeln!(f, "sleep 3").unwrap();
        }

        let cmd1 = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let cmd2 = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let cmd3 = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(cmd1.args, vec!["1"]);
        assert_eq!(cmd2.args, vec!["2"]);
        assert_eq!(cmd3.args, vec!["3"]);

        server.stop();
    }

    #[test]
    fn fifo_skips_empty_lines_and_comments() {
        let path = test_fifo_path("skip_empty");
        let (tx, rx) = mpsc::channel();
        let (parser, logger) = make_parser_and_logger();
        let shutdown = Arc::new(AtomicBool::new(false));
        let server =
            FifoServer::start(path.clone(), tx, parser, Arc::clone(&shutdown), logger).unwrap();

        {
            let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            writeln!(f).unwrap();
            writeln!(f, "# comment").unwrap();
            writeln!(f, "  ").unwrap();
            writeln!(f, "sleep 99").unwrap();
            writeln!(f).unwrap();
        }

        let cmd = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(cmd.program, "sleep");
        assert_eq!(cmd.args, vec!["99"]);
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());

        server.stop();
    }

    #[test]
    fn fifo_continues_after_parse_error() {
        let path = test_fifo_path("parse_err");
        let (tx, rx) = mpsc::channel();
        let (parser, logger) = make_parser_and_logger();
        let shutdown = Arc::new(AtomicBool::new(false));
        let server =
            FifoServer::start(path.clone(), tx, parser, Arc::clone(&shutdown), logger).unwrap();

        {
            let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            writeln!(f, "FOO=bar").unwrap();
            writeln!(f, "sleep 77").unwrap();
        }

        let cmd = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(cmd.program, "sleep");
        assert_eq!(cmd.args, vec!["77"]);

        server.stop();
    }

    #[test]
    fn fifo_shutdown_flag_stops_reader() {
        let path = test_fifo_path("shutdown");
        let (tx, _rx) = mpsc::channel();
        let (parser, logger) = make_parser_and_logger();
        let shutdown = Arc::new(AtomicBool::new(false));
        let server =
            FifoServer::start(path.clone(), tx, parser, Arc::clone(&shutdown), logger).unwrap();

        server.stop();
        assert!(!std::path::Path::new(&path).exists());
    }
}
