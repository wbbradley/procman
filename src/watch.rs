use std::{
    collections::HashMap,
    sync::{
        Arc,
        Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    checks,
    config::{OnFailAction, ProcessConfig, SupervisorCommand, Watch},
    log::Logger,
};

struct WatchState {
    consecutive_failures: u32,
    last_check: Instant,
    triggered: bool,
}

pub fn spawn_watcher(
    process_name: String,
    watches: Vec<Watch>,
    tx: mpsc::Sender<SupervisorCommand>,
    shutdown: Arc<AtomicBool>,
    logger: Arc<Mutex<Logger>>,
    exit_registry: Arc<Mutex<HashMap<String, i32>>>,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name(format!("watch:{process_name}"))
        .spawn(move || {
            watcher_loop(
                &process_name,
                &watches,
                &tx,
                &shutdown,
                &logger,
                &exit_registry,
            );
        })
        .expect("failed to spawn watcher thread")
}

fn should_stop(
    process_name: &str,
    shutdown: &AtomicBool,
    exit_registry: &Arc<Mutex<HashMap<String, i32>>>,
) -> bool {
    shutdown.load(Ordering::Relaxed) || exit_registry.lock().unwrap().contains_key(process_name)
}

fn sleep_interruptible(duration: Duration, shutdown: &AtomicBool) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        thread::sleep(Duration::from_millis(100).min(deadline - Instant::now()));
    }
}

fn build_watch_env(process_name: &str, watch: &Watch, failures: u32) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert("PROCMAN_WATCH_NAME".to_string(), watch.name.clone());
    env.insert(
        "PROCMAN_WATCH_PROCESS".to_string(),
        process_name.to_string(),
    );
    env.insert(
        "PROCMAN_WATCH_CHECK".to_string(),
        checks::description(&watch.check),
    );
    env.insert("PROCMAN_WATCH_FAILURES".to_string(), failures.to_string());
    env
}

/// Returns true if the watcher should exit after this action.
fn execute_action(
    process_name: &str,
    watch: &Watch,
    failures: u32,
    tx: &mpsc::Sender<SupervisorCommand>,
    logger: &Arc<Mutex<Logger>>,
) -> bool {
    let desc = checks::description(&watch.check);
    match &watch.on_fail {
        OnFailAction::Shutdown => {
            let message = format!(
                "{process_name}: watch '{}' triggered shutdown ({failures} failures: {desc})",
                watch.name
            );
            logger.lock().unwrap().log_line(process_name, &message);
            let _ = tx.send(SupervisorCommand::Shutdown { message });
            true
        }
        OnFailAction::Debug => {
            let message = format!(
                "{process_name}: watch '{}' triggered debug pause ({failures} failures: {desc})",
                watch.name
            );
            logger.lock().unwrap().log_line(process_name, &message);
            let _ = tx.send(SupervisorCommand::DebugPause { message });
            true
        }
        OnFailAction::Log => {
            logger.lock().unwrap().log_line(
                process_name,
                &format!(
                    "watch '{}' breach ({failures} failures: {desc})",
                    watch.name
                ),
            );
            false
        }
        OnFailAction::Spawn(target) => {
            let watch_env = build_watch_env(process_name, watch, failures);
            logger.lock().unwrap().log_line(
                process_name,
                &format!(
                    "watch '{}' spawning '{}' ({failures} failures: {desc})",
                    watch.name, target
                ),
            );
            let config = ProcessConfig {
                name: target.clone(),
                env: watch_env,
                run: String::new(),
                condition: None,
                depends: vec![],
                once: false,
                for_each: None,
                autostart: true,
                watches: vec![],
                is_task: false,
            };
            let _ = tx.send(SupervisorCommand::Spawn(config));
            false
        }
    }
}

fn watcher_loop(
    process_name: &str,
    watches: &[Watch],
    tx: &mpsc::Sender<SupervisorCommand>,
    shutdown: &Arc<AtomicBool>,
    logger: &Arc<Mutex<Logger>>,
    exit_registry: &Arc<Mutex<HashMap<String, i32>>>,
) {
    let agent = ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .timeout_global(Some(Duration::from_secs(5)))
            .build(),
    );

    let now = Instant::now();
    let mut states: Vec<WatchState> = watches
        .iter()
        .map(|_| WatchState {
            consecutive_failures: 0,
            last_check: now,
            triggered: false,
        })
        .collect();

    // Wait for the maximum initial delay, checking shutdown periodically
    if let Some(max_delay) = watches.iter().map(|w| w.initial_delay).max()
        && !max_delay.is_zero()
    {
        logger.lock().unwrap().log_line(
            process_name,
            &format!(
                "watches: waiting {:.1}s initial delay",
                max_delay.as_secs_f64()
            ),
        );
        sleep_interruptible(max_delay, shutdown);
    }

    loop {
        if should_stop(process_name, shutdown, exit_registry) {
            return;
        }

        let now = Instant::now();
        for (i, watch) in watches.iter().enumerate() {
            let state = &mut states[i];

            if now.duration_since(state.last_check) < watch.poll_interval {
                continue;
            }

            state.last_check = now;

            if checks::check(&watch.check, &agent, exit_registry) {
                if state.consecutive_failures > 0 {
                    logger.lock().unwrap().log_line(
                        process_name,
                        &format!(
                            "watch '{}' recovered after {} consecutive failure(s)",
                            watch.name, state.consecutive_failures
                        ),
                    );
                    state.consecutive_failures = 0;
                    state.triggered = false;
                }
            } else {
                state.consecutive_failures += 1;
                if state.consecutive_failures >= watch.failure_threshold && !state.triggered {
                    state.triggered = true;
                    if execute_action(process_name, watch, state.consecutive_failures, tx, logger) {
                        return;
                    }
                }
            }
        }

        // Sleep for the minimum poll interval across all watches
        let min_interval = watches
            .iter()
            .map(|w| w.poll_interval)
            .min()
            .unwrap_or(Duration::from_secs(5));
        let sleep_time = min_interval.max(Duration::from_millis(10));
        sleep_interruptible(sleep_time, shutdown);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;
    use crate::config::Dependency;

    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_path(name: &str) -> String {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!(
                "procman_watch_test_{name}_{}_{id}",
                std::process::id()
            ))
            .to_str()
            .unwrap()
            .to_string()
    }

    fn make_logger() -> Arc<Mutex<Logger>> {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let log_dir =
            std::env::temp_dir().join(format!("procman_watch_log_{}_{id}", std::process::id()));
        Arc::new(Mutex::new(
            Logger::new_for_test(&["test".to_string()], log_dir).unwrap(),
        ))
    }

    fn make_exit_registry() -> Arc<Mutex<HashMap<String, i32>>> {
        Arc::new(Mutex::new(HashMap::new()))
    }

    fn make_watch(name: &str, check: Dependency, on_fail: OnFailAction) -> Watch {
        Watch {
            name: name.to_string(),
            check,
            initial_delay: Duration::ZERO,
            poll_interval: Duration::from_millis(50),
            failure_threshold: 2,
            on_fail,
        }
    }

    #[test]
    fn watcher_triggers_shutdown_on_threshold() {
        let path = temp_path("shutdown");
        let watch = make_watch(
            "health",
            Dependency::FileExists {
                path: path.clone(),
                poll_interval: None,
                timeout: None,
                retry: true,
            },
            OnFailAction::Shutdown,
        );

        let (tx, rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let logger = make_logger();
        let exit_registry = make_exit_registry();

        let handle = spawn_watcher(
            "web".to_string(),
            vec![watch],
            tx,
            Arc::clone(&shutdown),
            logger,
            exit_registry,
        );

        let cmd = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        match cmd {
            SupervisorCommand::Shutdown { message } => {
                assert!(message.contains("web"));
                assert!(message.contains("health"));
            }
            _ => panic!("expected Shutdown"),
        }

        shutdown.store(true, Ordering::Relaxed);
        handle.join().unwrap();
    }

    #[test]
    fn watcher_log_does_not_send_command() {
        let path = temp_path("log_only");
        let watch = Watch {
            name: "disk".to_string(),
            check: Dependency::FileExists {
                path: path.clone(),
                poll_interval: None,
                timeout: None,
                retry: true,
            },
            initial_delay: Duration::ZERO,
            poll_interval: Duration::from_millis(50),
            failure_threshold: 1,
            on_fail: OnFailAction::Log,
        };

        let (tx, rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let logger = make_logger();
        let exit_registry = make_exit_registry();

        let handle = spawn_watcher(
            "app".to_string(),
            vec![watch],
            tx,
            Arc::clone(&shutdown),
            logger,
            exit_registry,
        );

        // Wait enough for threshold to be hit
        thread::sleep(Duration::from_millis(200));
        shutdown.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        // Channel should be empty — Log action doesn't send commands
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn watcher_spawn_sends_correct_env() {
        let path = temp_path("spawn_env");
        let watch = Watch {
            name: "recovery-check".to_string(),
            check: Dependency::FileExists {
                path: path.clone(),
                poll_interval: None,
                timeout: None,
                retry: true,
            },
            initial_delay: Duration::ZERO,
            poll_interval: Duration::from_millis(50),
            failure_threshold: 1,
            on_fail: OnFailAction::Spawn("recovery-script".to_string()),
        };

        let (tx, rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let logger = make_logger();
        let exit_registry = make_exit_registry();

        let handle = spawn_watcher(
            "web".to_string(),
            vec![watch],
            tx,
            Arc::clone(&shutdown),
            logger,
            exit_registry,
        );

        let cmd = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        match cmd {
            SupervisorCommand::Spawn(config) => {
                assert_eq!(config.name, "recovery-script");
                assert_eq!(
                    config.env.get("PROCMAN_WATCH_NAME").unwrap(),
                    "recovery-check"
                );
                assert_eq!(config.env.get("PROCMAN_WATCH_PROCESS").unwrap(), "web");
                assert!(config.env.contains_key("PROCMAN_WATCH_CHECK"));
                assert_eq!(config.env.get("PROCMAN_WATCH_FAILURES").unwrap(), "1");
            }
            _ => panic!("expected Spawn"),
        }

        shutdown.store(true, Ordering::Relaxed);
        handle.join().unwrap();
    }

    #[test]
    fn watcher_stops_on_process_exit() {
        let path = temp_path("exit_stop");
        let watch = make_watch(
            "health",
            Dependency::FileExists {
                path: path.clone(),
                poll_interval: None,
                timeout: None,
                retry: true,
            },
            OnFailAction::Shutdown,
        );

        let (tx, _rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let logger = make_logger();
        let exit_registry = make_exit_registry();

        // Pre-add process to exit registry
        exit_registry.lock().unwrap().insert("web".to_string(), 0);

        let handle = spawn_watcher(
            "web".to_string(),
            vec![watch],
            tx,
            shutdown,
            logger,
            exit_registry,
        );

        // Should exit quickly since process is already in exit_registry
        handle.join().unwrap();
    }

    #[test]
    fn watcher_stops_on_shutdown_flag() {
        let path = temp_path("shutdown_flag");
        let watch = make_watch(
            "health",
            Dependency::FileExists {
                path: path.clone(),
                poll_interval: None,
                timeout: None,
                retry: true,
            },
            OnFailAction::Shutdown,
        );

        let (tx, _rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(true)); // pre-set
        let logger = make_logger();
        let exit_registry = make_exit_registry();

        let handle = spawn_watcher(
            "web".to_string(),
            vec![watch],
            tx,
            shutdown,
            logger,
            exit_registry,
        );

        handle.join().unwrap();
    }

    #[test]
    fn watcher_recovery_resets_counter() {
        let path = temp_path("recovery");
        let watch = Watch {
            name: "health".to_string(),
            check: Dependency::FileExists {
                path: path.clone(),
                poll_interval: None,
                timeout: None,
                retry: true,
            },
            initial_delay: Duration::ZERO,
            poll_interval: Duration::from_millis(50),
            failure_threshold: 3,
            on_fail: OnFailAction::Shutdown,
        };

        let (tx, rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let logger = make_logger();
        let exit_registry = make_exit_registry();

        let handle = spawn_watcher(
            "web".to_string(),
            vec![watch],
            tx,
            Arc::clone(&shutdown),
            logger,
            exit_registry,
        );

        // Let it fail twice (below threshold of 3)
        thread::sleep(Duration::from_millis(200));

        // Create file to trigger recovery
        std::fs::write(&path, "ok").unwrap();
        thread::sleep(Duration::from_millis(200));

        // Remove file, let it fail again — needs full 3 failures again
        std::fs::remove_file(&path).unwrap();

        // Should eventually trigger shutdown after 3 more failures
        let cmd = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        match cmd {
            SupervisorCommand::Shutdown { .. } => {}
            _ => panic!("expected Shutdown"),
        }

        shutdown.store(true, Ordering::Relaxed);
        handle.join().unwrap();
    }

    #[test]
    fn watcher_respects_initial_delay() {
        let path = temp_path("delay");
        let watch = Watch {
            name: "delayed".to_string(),
            check: Dependency::FileExists {
                path: path.clone(),
                poll_interval: None,
                timeout: None,
                retry: true,
            },
            initial_delay: Duration::from_millis(300),
            poll_interval: Duration::from_millis(50),
            failure_threshold: 1,
            on_fail: OnFailAction::Shutdown,
        };

        let (tx, rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let logger = make_logger();
        let exit_registry = make_exit_registry();
        let start = Instant::now();

        let handle = spawn_watcher(
            "web".to_string(),
            vec![watch],
            tx,
            Arc::clone(&shutdown),
            logger,
            exit_registry,
        );

        let _cmd = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let elapsed = start.elapsed();

        // Should have waited at least the initial delay
        assert!(
            elapsed >= Duration::from_millis(250),
            "elapsed: {elapsed:?}"
        );

        shutdown.store(true, Ordering::Relaxed);
        handle.join().unwrap();
    }

    #[test]
    fn watcher_multiple_watches_independent() {
        let missing_path = temp_path("multi_missing");
        let present_path = temp_path("multi_present");
        std::fs::write(&present_path, "ok").unwrap();

        let failing_watch = make_watch(
            "failing",
            Dependency::FileExists {
                path: missing_path.clone(),
                poll_interval: None,
                timeout: None,
                retry: true,
            },
            OnFailAction::Shutdown,
        );
        let passing_watch = make_watch(
            "passing",
            Dependency::FileExists {
                path: present_path.clone(),
                poll_interval: None,
                timeout: None,
                retry: true,
            },
            OnFailAction::Shutdown,
        );

        let (tx, rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let logger = make_logger();
        let exit_registry = make_exit_registry();

        let handle = spawn_watcher(
            "web".to_string(),
            vec![failing_watch, passing_watch],
            tx,
            Arc::clone(&shutdown),
            logger,
            exit_registry,
        );

        let cmd = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        match cmd {
            SupervisorCommand::Shutdown { message } => {
                assert!(message.contains("failing"), "message: {message}");
            }
            _ => panic!("expected Shutdown"),
        }

        shutdown.store(true, Ordering::Relaxed);
        handle.join().unwrap();
        let _ = std::fs::remove_file(&present_path);
    }
}
