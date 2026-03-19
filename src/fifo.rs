use std::{
    collections::HashMap,
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
use serde::{Deserialize, Serialize};

use crate::{
    config::{DependencyDef, ProcessConfig, SupervisorCommand},
    log::Logger,
};

#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FifoMessage {
    Run {
        name: String,
        run: String,
        #[serde(default)]
        env: Option<HashMap<String, String>>,
        #[serde(default)]
        depends: Option<Vec<DependencyDef>>,
        #[serde(default)]
        once: Option<bool>,
    },
    Shutdown {
        #[serde(default)]
        user: Option<String>,
        #[serde(default)]
        message: Option<String>,
    },
}

impl FifoMessage {
    fn into_supervisor_command(
        self,
        name_counts: &mut HashMap<String, usize>,
    ) -> Result<SupervisorCommand> {
        match self {
            FifoMessage::Run {
                name,
                run,
                env,
                depends,
                once,
            } => {
                if run.trim().is_empty() {
                    anyhow::bail!("empty run command for {name}");
                }

                let mut merged_env: HashMap<String, String> = std::env::vars().collect();
                if let Some(extra) = env {
                    for (k, v) in extra {
                        merged_env.insert(k, v);
                    }
                }

                let depends = depends
                    .unwrap_or_default()
                    .into_iter()
                    .map(DependencyDef::into_dependency)
                    .collect::<Result<Vec<_>>>()?;

                let deduped_name = dedup_name(name, name_counts);

                Ok(SupervisorCommand::Spawn(ProcessConfig {
                    name: deduped_name,
                    env: merged_env,
                    run,
                    depends,
                    once: once.unwrap_or(false),
                    for_each: None,
                }))
            }
            FifoMessage::Shutdown { user, message } => {
                let parts: Vec<&str> = [user.as_deref(), message.as_deref()]
                    .into_iter()
                    .flatten()
                    .collect();
                let msg = if parts.is_empty() {
                    "shutdown requested via FIFO".to_string()
                } else {
                    parts.join(": ")
                };
                Ok(SupervisorCommand::Shutdown { message: msg })
            }
        }
    }
}

fn dedup_name(name: String, name_counts: &mut HashMap<String, usize>) -> String {
    let count = name_counts.entry(name.clone()).or_insert(0);
    let result = if *count == 0 {
        name.clone()
    } else {
        format!("{name}.{count}")
    };
    *count += 1;
    result
}

pub struct FifoServer {
    path: String,
    shutdown: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl FifoServer {
    pub fn start(
        path: String,
        tx: mpsc::Sender<SupervisorCommand>,
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
            Self::reader_loop(&fifo_path, &tx, &shutdown_clone, &logger);
        });

        Ok(Self {
            path,
            shutdown,
            thread: Some(thread),
        })
    }

    fn reader_loop(
        path: &str,
        tx: &mpsc::Sender<SupervisorCommand>,
        shutdown: &Arc<AtomicBool>,
        logger: &Arc<Mutex<Logger>>,
    ) {
        let mut name_counts: HashMap<String, usize> = HashMap::new();

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
                if trimmed.is_empty() {
                    continue;
                }

                let msg: FifoMessage = match serde_json::from_str(trimmed) {
                    Ok(msg) => msg,
                    Err(e) => {
                        logger
                            .lock()
                            .unwrap()
                            .log_line("fifo", &format!("parse error: {e}"));
                        continue;
                    }
                };

                let cmd = match msg.into_supervisor_command(&mut name_counts) {
                    Ok(cmd) => cmd,
                    Err(e) => {
                        logger
                            .lock()
                            .unwrap()
                            .log_line("fifo", &format!("command error: {e}"));
                        continue;
                    }
                };

                match &cmd {
                    SupervisorCommand::Spawn(config) => {
                        logger
                            .lock()
                            .unwrap()
                            .log_line("procman", &format!("{} submitted via FIFO", config.name));
                    }
                    SupervisorCommand::Shutdown { message } => {
                        logger
                            .lock()
                            .unwrap()
                            .log_line("procman", &format!("shutdown via FIFO: {message}"));
                    }
                }

                if tx.send(cmd).is_err() {
                    return;
                }
            }
            // EOF — writer disconnected, loop back to re-open for next client
        }
    }

    pub fn stop(mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        // Retry the wake-up open until the reader thread exits. The reader may be
        // between its shutdown check and File::open, so a single O_WRONLY attempt
        // can race (ENXIO if no reader is blocked yet). Retrying closes the window.
        if let Some(thread) = self.thread.take() {
            while !thread.is_finished() {
                let _ = open(
                    self.path.as_str(),
                    OFlag::O_WRONLY | OFlag::O_NONBLOCK,
                    Mode::empty(),
                );
                thread::sleep(Duration::from_millis(1));
            }
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

    fn make_logger() -> Arc<Mutex<Logger>> {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("procman_parser_{}_{id}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log_dir = dir.join("logs");
        let logger =
            Logger::new_for_test(&["fifo".to_string(), "procman".to_string()], log_dir).unwrap();
        Arc::new(Mutex::new(logger))
    }

    #[test]
    fn fifo_lifecycle_creates_and_cleans_up() {
        let path = test_fifo_path("lifecycle");
        let (tx, _rx) = mpsc::channel();
        let logger = make_logger();
        let shutdown = Arc::new(AtomicBool::new(false));
        let server = FifoServer::start(path.clone(), tx, Arc::clone(&shutdown), logger).unwrap();
        assert!(std::path::Path::new(&path).exists());
        server.stop();
        assert!(!std::path::Path::new(&path).exists());
    }

    #[test]
    fn fifo_receives_single_command() {
        let path = test_fifo_path("single_cmd");
        let (tx, rx) = mpsc::channel();
        let logger = make_logger();
        let shutdown = Arc::new(AtomicBool::new(false));
        let server = FifoServer::start(path.clone(), tx, Arc::clone(&shutdown), logger).unwrap();

        {
            let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            writeln!(f, r#"{{"type":"run","name":"sleep","run":"sleep 42"}}"#).unwrap();
        }

        let cmd = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        match cmd {
            SupervisorCommand::Spawn(config) => {
                assert_eq!(config.run, "sleep 42");
            }
            _ => panic!("expected Spawn"),
        }

        server.stop();
    }

    #[test]
    fn fifo_receives_multiple_commands_in_order() {
        let path = test_fifo_path("multi_cmd");
        let (tx, rx) = mpsc::channel();
        let logger = make_logger();
        let shutdown = Arc::new(AtomicBool::new(false));
        let server = FifoServer::start(path.clone(), tx, Arc::clone(&shutdown), logger).unwrap();

        {
            let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            writeln!(f, r#"{{"type":"run","name":"sleep","run":"sleep 1"}}"#).unwrap();
            writeln!(f, r#"{{"type":"run","name":"sleep","run":"sleep 2"}}"#).unwrap();
            writeln!(f, r#"{{"type":"run","name":"sleep","run":"sleep 3"}}"#).unwrap();
        }

        let cmd1 = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let cmd2 = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let cmd3 = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        match (cmd1, cmd2, cmd3) {
            (
                SupervisorCommand::Spawn(c1),
                SupervisorCommand::Spawn(c2),
                SupervisorCommand::Spawn(c3),
            ) => {
                assert_eq!(c1.run, "sleep 1");
                assert_eq!(c2.run, "sleep 2");
                assert_eq!(c3.run, "sleep 3");
            }
            _ => panic!("expected Spawn commands"),
        }

        server.stop();
    }

    #[test]
    fn fifo_skips_empty_lines_and_comments() {
        let path = test_fifo_path("skip_empty");
        let (tx, rx) = mpsc::channel();
        let logger = make_logger();
        let shutdown = Arc::new(AtomicBool::new(false));
        let server = FifoServer::start(path.clone(), tx, Arc::clone(&shutdown), logger).unwrap();

        {
            let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            writeln!(f).unwrap();
            writeln!(f, "# comment").unwrap();
            writeln!(f, "  ").unwrap();
            writeln!(f, r#"{{"type":"run","name":"sleep","run":"sleep 99"}}"#).unwrap();
            writeln!(f).unwrap();
        }

        let cmd = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        match cmd {
            SupervisorCommand::Spawn(config) => {
                assert_eq!(config.run, "sleep 99");
            }
            _ => panic!("expected Spawn"),
        }
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());

        server.stop();
    }

    #[test]
    fn fifo_continues_after_parse_error() {
        let path = test_fifo_path("parse_err");
        let (tx, rx) = mpsc::channel();
        let logger = make_logger();
        let shutdown = Arc::new(AtomicBool::new(false));
        let server = FifoServer::start(path.clone(), tx, Arc::clone(&shutdown), logger).unwrap();

        {
            let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            writeln!(f, "not valid json").unwrap();
            writeln!(f, r#"{{"type":"run","name":"sleep","run":"sleep 77"}}"#).unwrap();
        }

        let cmd = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        match cmd {
            SupervisorCommand::Spawn(config) => {
                assert_eq!(config.run, "sleep 77");
            }
            _ => panic!("expected Spawn"),
        }

        server.stop();
    }

    #[test]
    fn fifo_shutdown_flag_stops_reader() {
        let path = test_fifo_path("shutdown");
        let (tx, _rx) = mpsc::channel();
        let logger = make_logger();
        let shutdown = Arc::new(AtomicBool::new(false));
        let server = FifoServer::start(path.clone(), tx, Arc::clone(&shutdown), logger).unwrap();

        server.stop();
        assert!(!std::path::Path::new(&path).exists());
    }

    #[test]
    fn fifo_receives_shutdown_command() {
        let path = test_fifo_path("shutdown_cmd");
        let (tx, rx) = mpsc::channel();
        let logger = make_logger();
        let shutdown = Arc::new(AtomicBool::new(false));
        let server = FifoServer::start(path.clone(), tx, Arc::clone(&shutdown), logger).unwrap();

        {
            let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            writeln!(
                f,
                r#"{{"type":"shutdown","user":"testuser","message":"bye"}}"#
            )
            .unwrap();
        }

        let cmd = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        match cmd {
            SupervisorCommand::Shutdown { message } => {
                assert!(message.contains("testuser"));
                assert!(message.contains("bye"));
            }
            _ => panic!("expected Shutdown"),
        }

        server.stop();
    }

    #[test]
    fn fifo_dedup_names() {
        let path = test_fifo_path("dedup");
        let (tx, rx) = mpsc::channel();
        let logger = make_logger();
        let shutdown = Arc::new(AtomicBool::new(false));
        let server = FifoServer::start(path.clone(), tx, Arc::clone(&shutdown), logger).unwrap();

        {
            let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            writeln!(f, r#"{{"type":"run","name":"worker","run":"echo a"}}"#).unwrap();
            writeln!(f, r#"{{"type":"run","name":"worker","run":"echo b"}}"#).unwrap();
        }

        let cmd1 = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let cmd2 = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        match (cmd1, cmd2) {
            (SupervisorCommand::Spawn(c1), SupervisorCommand::Spawn(c2)) => {
                assert_eq!(c1.name, "worker");
                assert_eq!(c2.name, "worker.1");
            }
            _ => panic!("expected Spawn commands"),
        }

        server.stop();
    }

    #[test]
    fn fifo_receives_once_flag() {
        let path = test_fifo_path("once_flag");
        let (tx, rx) = mpsc::channel();
        let logger = make_logger();
        let shutdown = Arc::new(AtomicBool::new(false));
        let server = FifoServer::start(path.clone(), tx, Arc::clone(&shutdown), logger).unwrap();

        {
            let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            writeln!(
                f,
                r#"{{"type":"run","name":"migrate","run":"echo done","once":true}}"#
            )
            .unwrap();
        }

        let cmd = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        match cmd {
            SupervisorCommand::Spawn(config) => {
                assert!(config.once);
            }
            _ => panic!("expected Spawn"),
        }

        server.stop();
    }

    #[test]
    fn dedup_name_increments() {
        let mut counts = HashMap::new();
        assert_eq!(dedup_name("foo".to_string(), &mut counts), "foo");
        assert_eq!(dedup_name("foo".to_string(), &mut counts), "foo.1");
        assert_eq!(dedup_name("foo".to_string(), &mut counts), "foo.2");
        assert_eq!(dedup_name("bar".to_string(), &mut counts), "bar");
        assert_eq!(dedup_name("bar".to_string(), &mut counts), "bar.1");
    }
}
