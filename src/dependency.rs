use std::{
    collections::HashMap,
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
    checks::{check, collect_dependency_env, description, poll_interval, retry, timeout},
    config::{ProcessConfig, SupervisorCommand},
    log::Logger,
};

fn wait_for_dependencies(
    config: &ProcessConfig,
    shutdown: &AtomicBool,
    logger: &Arc<Mutex<Logger>>,
    exit_registry: &Arc<Mutex<HashMap<String, i32>>>,
) -> bool {
    let agent = ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .timeout_global(Some(Duration::from_secs(5)))
            .build(),
    );

    let total = config.depends.len();
    for (i, dep) in config.depends.iter().enumerate() {
        let start = Instant::now();
        let mut first_failure_logged = false;

        loop {
            if shutdown.load(Ordering::Relaxed) {
                return false;
            }

            if let Some(t) = timeout(dep)
                && start.elapsed() > t
            {
                let desc = description(dep);
                logger
                    .lock()
                    .unwrap()
                    .log_line(&config.name, &format!("dependency timed out: {desc}"));
                return false;
            }

            if check(dep, &agent, exit_registry) {
                if let crate::config::Dependency::ProcessExited { name, .. } = dep
                    && let Some(&code) = exit_registry.lock().unwrap().get(name)
                        && code != 0 {
                            let desc = description(dep);
                            logger.lock().unwrap().log_line(
                                &config.name,
                                &format!("dependency failed: {desc} (exit code {code})"),
                            );
                            return false;
                        }
                let desc = description(dep);
                let remaining_count = total - i - 1;
                if remaining_count == 0 {
                    logger
                        .lock()
                        .unwrap()
                        .log_line(&config.name, &format!("dependency satisfied: {desc}"));
                } else {
                    let remaining: Vec<String> =
                        config.depends[i + 1..].iter().map(description).collect();
                    logger.lock().unwrap().log_line(
                        &config.name,
                        &format!(
                            "dependency satisfied: {desc} (remaining: {})",
                            remaining.join(", ")
                        ),
                    );
                }
                break;
            }

            if !first_failure_logged {
                first_failure_logged = true;
                if !retry(dep) {
                    let desc = description(dep);
                    logger.lock().unwrap().log_line(
                        &config.name,
                        &format!("dependency failed (retry disabled): {desc}"),
                    );
                    return false;
                }
                let desc = description(dep);
                logger
                    .lock()
                    .unwrap()
                    .log_line(&config.name, &format!("dependency not ready: {desc}"));
            }

            thread::sleep(poll_interval(dep));
        }
    }

    true
}

pub fn spawn_waiter(
    config: ProcessConfig,
    tx: mpsc::Sender<SupervisorCommand>,
    shutdown: Arc<AtomicBool>,
    logger: Arc<Mutex<Logger>>,
    pending: Arc<AtomicUsize>,
    exit_registry: Arc<Mutex<HashMap<String, i32>>>,
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

        if wait_for_dependencies(&config, &shutdown, &logger, &exit_registry) {
            match collect_dependency_env(&config.depends) {
                Ok(dep_env) => {
                    logger
                        .lock()
                        .unwrap()
                        .log_line(&name, "all dependencies satisfied, starting");
                    let mut config = config;
                    config.env.extend(dep_env);
                    let _ = tx.send(SupervisorCommand::Spawn(config));
                }
                Err(e) => {
                    logger
                        .lock()
                        .unwrap()
                        .log_line(&name, &format!("env extraction failed: {e}"));
                    shutdown.store(true, Ordering::Relaxed);
                }
            }
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
    use crate::config::Dependency;

    fn make_exit_registry() -> Arc<Mutex<HashMap<String, i32>>> {
        Arc::new(Mutex::new(HashMap::new()))
    }

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
            run: "true".to_string(),
            condition: None,
            depends,
            once: false,
            for_each: None,
            autostart: true,
            watches: vec![],
        }
    }

    fn make_logger(names: &[&str]) -> Arc<Mutex<Logger>> {
        let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let log_dir =
            std::env::temp_dir().join(format!("procman_dep_log_{}_{id}", std::process::id()));
        Arc::new(Mutex::new(Logger::new_for_test(&names, log_dir).unwrap()))
    }

    #[test]
    fn wait_for_file_dependency() {
        let path = temp_path("wait_file");
        let _ = std::fs::remove_file(&path);

        let config = make_config(
            "waiter",
            vec![Dependency::FileExists {
                path: path.clone(),
                retry: true,
            }],
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
            make_exit_registry(),
        );

        // Create the file after a short delay
        thread::sleep(Duration::from_millis(100));
        std::fs::write(&path, "").unwrap();

        let received = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        match received {
            SupervisorCommand::Spawn(config) => assert_eq!(config.name, "waiter"),
            _ => panic!("expected Spawn"),
        }
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
                retry: true,
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
            make_exit_registry(),
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
            vec![Dependency::FileExists {
                path: path.clone(),
                retry: true,
            }],
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
            make_exit_registry(),
        );

        // Set shutdown immediately
        shutdown.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        assert_eq!(pending.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn wait_for_tcp_connect_dependency() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();

        let config = make_config(
            "tcp-waiter",
            vec![Dependency::TcpConnect {
                address: addr,
                poll_interval: None,
                timeout: None,
                retry: true,
            }],
        );
        let (tx, rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let logger = make_logger(&["tcp-waiter"]);
        let pending = Arc::new(AtomicUsize::new(1));

        let handle = spawn_waiter(
            config,
            tx,
            Arc::clone(&shutdown),
            logger,
            Arc::clone(&pending),
            make_exit_registry(),
        );

        let received = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        match received {
            SupervisorCommand::Spawn(config) => assert_eq!(config.name, "tcp-waiter"),
            _ => panic!("expected Spawn"),
        }
        handle.join().unwrap();
        assert_eq!(pending.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn wait_for_process_exited_dependency() {
        let config = make_config(
            "api",
            vec![Dependency::ProcessExited {
                name: "migrate".to_string(),
                timeout: Some(Duration::from_secs(60)),
                retry: true,
            }],
        );
        let (tx, rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let logger = make_logger(&["api"]);
        let pending = Arc::new(AtomicUsize::new(1));
        let exit_registry = make_exit_registry();

        let _handle = spawn_waiter(
            config,
            tx,
            Arc::clone(&shutdown),
            logger,
            Arc::clone(&pending),
            Arc::clone(&exit_registry),
        );

        // Insert after short delay
        thread::sleep(Duration::from_millis(100));
        exit_registry
            .lock()
            .unwrap()
            .insert("migrate".to_string(), 0);

        let received = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        match received {
            SupervisorCommand::Spawn(config) => assert_eq!(config.name, "api"),
            _ => panic!("expected Spawn"),
        }
        assert_eq!(pending.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn file_contains_rejects_invalid_jsonpath() {
        use crate::config::DependencyDef;
        let def = DependencyDef::FileContainsKey {
            file_contains: crate::config::FileContainsDef {
                path: "/tmp/test.yaml".to_string(),
                format: "yaml".to_string(),
                key: "$[invalid".to_string(),
                env: None,
                poll_interval: None,
                timeout_seconds: None,
                retry: None,
            },
        };
        let err = def.into_dependency().unwrap_err();
        assert!(err.to_string().contains("invalid JSONPath"), "{err}");
    }

    #[test]
    fn sequential_deps_block_on_first() {
        let path = temp_path("seq_block");
        std::fs::write(&path, "").unwrap();

        let config = make_config(
            "seq-block",
            vec![
                Dependency::ProcessExited {
                    name: "setup".to_string(),
                    timeout: Some(Duration::from_secs(60)),
                    retry: true,
                },
                Dependency::FileExists {
                    path: path.clone(),
                    retry: true,
                },
            ],
        );
        let (tx, rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let logger = make_logger(&["seq-block"]);
        let pending = Arc::new(AtomicUsize::new(1));
        let exit_registry = make_exit_registry();

        let _handle = spawn_waiter(
            config,
            tx,
            Arc::clone(&shutdown),
            logger,
            Arc::clone(&pending),
            Arc::clone(&exit_registry),
        );

        // File already exists, but Spawn should NOT arrive until "setup" exits
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());

        // Now satisfy the first dep
        exit_registry.lock().unwrap().insert("setup".to_string(), 0);

        let received = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        match received {
            SupervisorCommand::Spawn(config) => assert_eq!(config.name, "seq-block"),
            _ => panic!("expected Spawn"),
        }

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn sequential_timeout_starts_per_dep() {
        let path = temp_path("seq_timeout_per");
        std::fs::write(&path, "").unwrap();

        let config = make_config(
            "seq-per-dep",
            vec![
                Dependency::ProcessExited {
                    name: "setup".to_string(),
                    timeout: Some(Duration::from_secs(60)),
                    retry: true,
                },
                Dependency::FileExists {
                    path: path.clone(),
                    retry: true,
                },
            ],
        );
        let (tx, rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let logger = make_logger(&["seq-per-dep"]);
        let pending = Arc::new(AtomicUsize::new(1));
        let exit_registry = make_exit_registry();

        let _handle = spawn_waiter(
            config,
            tx,
            Arc::clone(&shutdown),
            logger,
            Arc::clone(&pending),
            Arc::clone(&exit_registry),
        );

        // Satisfy dep 0 after 100ms — dep 1's timeout starts fresh from that point
        thread::sleep(Duration::from_millis(100));
        exit_registry.lock().unwrap().insert("setup".to_string(), 0);

        // Should succeed because the file already exists (dep 1 timeout is 60s from now)
        let received = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        match received {
            SupervisorCommand::Spawn(config) => assert_eq!(config.name, "seq-per-dep"),
            _ => panic!("expected Spawn"),
        }
        assert!(!shutdown.load(Ordering::Relaxed));

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn sequential_timeout_on_blocked_dep() {
        let path = temp_path("seq_timeout_blocked");
        std::fs::write(&path, "").unwrap();

        let config = make_config(
            "seq-timeout",
            vec![
                Dependency::HttpHealthCheck {
                    url: "http://127.0.0.1:1".to_string(),
                    code: 200,
                    poll_interval: Some(Duration::from_millis(50)),
                    timeout: Some(Duration::from_millis(200)),
                    retry: true,
                },
                Dependency::FileExists {
                    path: path.clone(),
                    retry: true,
                },
            ],
        );
        let (tx, rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let logger = make_logger(&["seq-timeout"]);
        let pending = Arc::new(AtomicUsize::new(1));

        let handle = spawn_waiter(
            config,
            tx,
            Arc::clone(&shutdown),
            logger,
            Arc::clone(&pending),
            make_exit_registry(),
        );
        handle.join().unwrap();

        // Dep 0 timed out — shutdown set, no Spawn sent
        assert!(shutdown.load(Ordering::Relaxed));
        assert!(rx.try_recv().is_err());

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn retry_false_fails_immediately() {
        let path = temp_path("retry_false");
        let _ = std::fs::remove_file(&path);

        let config = make_config(
            "retry-false",
            vec![Dependency::FileExists {
                path: path.clone(),
                retry: false,
            }],
        );
        let (tx, _rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let logger = make_logger(&["retry-false"]);
        let pending = Arc::new(AtomicUsize::new(1));

        let start = Instant::now();
        let handle = spawn_waiter(
            config,
            tx,
            Arc::clone(&shutdown),
            logger,
            Arc::clone(&pending),
            make_exit_registry(),
        );
        handle.join().unwrap();

        // Should have failed almost immediately — well under 1 second
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(shutdown.load(Ordering::Relaxed));
        assert_eq!(pending.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn retry_true_retries_as_before() {
        let path = temp_path("retry_true");
        let _ = std::fs::remove_file(&path);

        let config = make_config(
            "retry-true",
            vec![Dependency::FileExists {
                path: path.clone(),
                retry: true,
            }],
        );
        let (tx, rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let logger = make_logger(&["retry-true"]);
        let pending = Arc::new(AtomicUsize::new(1));

        let _handle = spawn_waiter(
            config,
            tx,
            Arc::clone(&shutdown),
            logger,
            Arc::clone(&pending),
            make_exit_registry(),
        );

        // Create the file after a short delay — should retry and succeed
        thread::sleep(Duration::from_millis(200));
        std::fs::write(&path, "").unwrap();

        let received = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        match received {
            SupervisorCommand::Spawn(config) => assert_eq!(config.name, "retry-true"),
            _ => panic!("expected Spawn"),
        }

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn retry_default_is_true() {
        let yaml = "api:\n  depends:\n    - path: /tmp/retry_default_test\n  run: echo hi\n";
        let def: std::collections::HashMap<String, serde_yaml::Value> =
            serde_yaml::from_str(yaml).unwrap();
        let api_val = def.get("api").unwrap();
        let depends = api_val.get("depends").unwrap().as_sequence().unwrap();
        let dep_def: crate::config::DependencyDef =
            serde_yaml::from_value(depends[0].clone()).unwrap();
        let dep = dep_def.into_dependency().unwrap();
        match dep {
            Dependency::FileExists { retry, .. } => assert!(retry),
            _ => panic!("expected FileExists"),
        }
    }

    #[test]
    fn failed_process_exited_triggers_shutdown() {
        let config = make_config(
            "api",
            vec![Dependency::ProcessExited {
                name: "migrate".to_string(),
                timeout: Some(Duration::from_secs(60)),
                retry: true,
            }],
        );
        let (tx, rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let logger = make_logger(&["api"]);
        let pending = Arc::new(AtomicUsize::new(1));
        let exit_registry = make_exit_registry();

        let handle = spawn_waiter(
            config,
            tx,
            Arc::clone(&shutdown),
            logger,
            Arc::clone(&pending),
            Arc::clone(&exit_registry),
        );

        // Simulate failed exit
        thread::sleep(Duration::from_millis(100));
        exit_registry
            .lock()
            .unwrap()
            .insert("migrate".to_string(), 1);

        handle.join().unwrap();

        // Should NOT receive a Spawn command
        assert!(rx.try_recv().is_err());
        // Should have triggered shutdown
        assert!(shutdown.load(Ordering::Relaxed));
        assert_eq!(pending.load(Ordering::Relaxed), 0);
    }
}
