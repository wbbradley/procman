use std::{
    path::Path,
    sync::{
        Arc,
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    config::{Dependency, ProcessConfig},
    log::Logger,
};

fn check(dep: &Dependency, agent: &ureq::Agent) -> bool {
    match dep {
        Dependency::HttpHealthCheck { url, code, .. } => match agent.get(url).call() {
            Ok(response) => response.status() == *code,
            Err(_) => false,
        },
        Dependency::FileExists { path } => Path::new(path).exists(),
    }
}

fn poll_interval(dep: &Dependency) -> Duration {
    match dep {
        Dependency::HttpHealthCheck { poll_interval, .. } => {
            poll_interval.unwrap_or(Duration::from_secs(1))
        }
        Dependency::FileExists { .. } => Duration::from_secs(1),
    }
}

fn timeout(dep: &Dependency) -> Duration {
    match dep {
        Dependency::HttpHealthCheck { timeout, .. } => timeout.unwrap_or(Duration::from_secs(60)),
        Dependency::FileExists { .. } => Duration::from_secs(60),
    }
}

fn description(dep: &Dependency) -> String {
    match dep {
        Dependency::HttpHealthCheck { url, code, .. } => {
            format!("HTTP {code} from {url}")
        }
        Dependency::FileExists { path } => {
            format!("file exists: {path}")
        }
    }
}

fn wait_for_dependencies(
    config: &ProcessConfig,
    shutdown: &AtomicBool,
    logger: &Arc<Mutex<Logger>>,
) -> bool {
    let agent = ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .timeout_global(Some(Duration::from_secs(5)))
            .build(),
    );
    let mut satisfied = vec![false; config.depends.len()];
    let starts: Vec<Instant> = config.depends.iter().map(|_| Instant::now()).collect();

    loop {
        if shutdown.load(Ordering::Relaxed) {
            return false;
        }

        let mut min_interval = Duration::from_secs(1);
        let mut all_satisfied = true;

        for (i, dep) in config.depends.iter().enumerate() {
            if satisfied[i] {
                continue;
            }

            if starts[i].elapsed() > timeout(dep) {
                let desc = description(dep);
                logger
                    .lock()
                    .unwrap()
                    .log_line(&config.name, &format!("dependency timed out: {desc}"));
                return false;
            }

            if check(dep, &agent) {
                satisfied[i] = true;
                let desc = description(dep);
                logger
                    .lock()
                    .unwrap()
                    .log_line(&config.name, &format!("dependency satisfied: {desc}"));
            } else {
                all_satisfied = false;
                min_interval = min_interval.min(poll_interval(dep));
            }
        }

        if all_satisfied {
            return true;
        }

        thread::sleep(min_interval);
    }
}

pub fn spawn_waiter(
    config: ProcessConfig,
    tx: mpsc::Sender<ProcessConfig>,
    shutdown: Arc<AtomicBool>,
    logger: Arc<Mutex<Logger>>,
    pending: Arc<AtomicUsize>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let name = config.name.clone();
        logger.lock().unwrap().log_line(
            &name,
            &format!(
                "waiting for {} dependenc{}",
                config.depends.len(),
                if config.depends.len() == 1 {
                    "y"
                } else {
                    "ies"
                }
            ),
        );

        if wait_for_dependencies(&config, &shutdown, &logger) {
            logger
                .lock()
                .unwrap()
                .log_line(&name, "all dependencies satisfied, starting");
            let _ = tx.send(config);
        } else if !shutdown.load(Ordering::Relaxed) {
            // Dependency timed out — trigger shutdown
            shutdown.store(true, Ordering::Relaxed);
        }

        pending.fetch_sub(1, Ordering::Relaxed);
    })
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::atomic::AtomicUsize};

    use super::*;

    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_path(name: &str) -> String {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!(
                "procman_dep_test_{name}_{}_{id}",
                std::process::id()
            ))
            .to_str()
            .unwrap()
            .to_string()
    }

    fn make_config(name: &str, depends: Vec<Dependency>) -> ProcessConfig {
        ProcessConfig {
            name: name.to_string(),
            env: HashMap::new(),
            program: "true".to_string(),
            args: vec![],
            depends,
        }
    }

    fn make_logger(names: &[&str]) -> Arc<Mutex<Logger>> {
        let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        Arc::new(Mutex::new(Logger::new(&names).unwrap()))
    }

    #[test]
    fn file_exists_check_returns_false_then_true() {
        let path = temp_path("check_file");
        let _ = std::fs::remove_file(&path);
        let dep = Dependency::FileExists { path: path.clone() };
        let agent = ureq::Agent::new_with_config(
            ureq::config::Config::builder()
                .timeout_global(Some(Duration::from_secs(5)))
                .build(),
        );

        assert!(!check(&dep, &agent));
        std::fs::write(&path, "").unwrap();
        assert!(check(&dep, &agent));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn wait_for_file_dependency() {
        let path = temp_path("wait_file");
        let _ = std::fs::remove_file(&path);

        let config = make_config(
            "waiter",
            vec![Dependency::FileExists { path: path.clone() }],
        );
        let (tx, rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let logger = make_logger(&["waiter"]);
        let pending = Arc::new(AtomicUsize::new(1));

        let _handle = spawn_waiter(
            config,
            tx,
            Arc::clone(&shutdown),
            logger,
            Arc::clone(&pending),
        );

        // Create the file after a short delay
        thread::sleep(Duration::from_millis(100));
        std::fs::write(&path, "").unwrap();

        let received = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(received.name, "waiter");
        assert_eq!(pending.load(Ordering::Relaxed), 0);

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn dependency_timeout_sets_shutdown() {
        let config = make_config(
            "timeout-test",
            vec![Dependency::HttpHealthCheck {
                url: "http://127.0.0.1:1".to_string(),
                code: 200,
                poll_interval: Some(Duration::from_millis(50)),
                timeout: Some(Duration::from_millis(200)),
            }],
        );
        let (tx, _rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let logger = make_logger(&["timeout-test"]);
        let pending = Arc::new(AtomicUsize::new(1));

        let handle = spawn_waiter(
            config,
            tx,
            Arc::clone(&shutdown),
            logger,
            Arc::clone(&pending),
        );
        handle.join().unwrap();

        assert!(shutdown.load(Ordering::Relaxed));
        assert_eq!(pending.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn waiter_respects_shutdown() {
        let path = temp_path("shutdown_file");
        let _ = std::fs::remove_file(&path);

        let config = make_config(
            "shutdown-test",
            vec![Dependency::FileExists { path: path.clone() }],
        );
        let (tx, _rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let logger = make_logger(&["shutdown-test"]);
        let pending = Arc::new(AtomicUsize::new(1));

        let handle = spawn_waiter(
            config,
            tx,
            Arc::clone(&shutdown),
            logger,
            Arc::clone(&pending),
        );

        // Set shutdown immediately
        shutdown.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        assert_eq!(pending.load(Ordering::Relaxed), 0);
    }
}
