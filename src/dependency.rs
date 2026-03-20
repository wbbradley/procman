use std::{
    collections::{HashMap, HashSet},
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

use anyhow::{Result, anyhow};

use crate::{
    config::{Dependency, FileFormat, ProcessConfig, SupervisorCommand},
    log::Logger,
};

fn read_file_value(
    path: &str,
    format: &FileFormat,
    key: &serde_json_path::JsonPath,
) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let root: serde_json::Value = match format {
        FileFormat::Json => serde_json::from_str(&content).ok()?,
        FileFormat::Yaml => serde_yaml::from_str(&content).ok()?,
    };
    let node_list = key.query(&root);
    let value = node_list.first()?;
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        serde_json::Value::Null => None,
        _ => serde_json::to_string(value).ok(),
    }
}

pub fn collect_dependency_env(deps: &[Dependency]) -> Result<HashMap<String, String>> {
    let mut env = HashMap::new();
    for dep in deps {
        if let Dependency::FileContainsKey {
            path,
            format,
            key,
            env: Some(env_var),
            ..
        } = dep
        {
            let value = read_file_value(path, format, key)
                .ok_or_else(|| anyhow!("failed to extract key '{key}' from {path}"))?;
            env.insert(env_var.clone(), value);
        }
    }
    Ok(env)
}

fn check(
    dep: &Dependency,
    agent: &ureq::Agent,
    exit_registry: &Arc<Mutex<HashSet<String>>>,
) -> bool {
    match dep {
        Dependency::HttpHealthCheck { url, code, .. } => match agent.get(url).call() {
            Ok(response) => response.status() == *code,
            Err(_) => false,
        },
        Dependency::TcpConnect { address, .. } => {
            use std::net::ToSocketAddrs;
            address
                .to_socket_addrs()
                .ok()
                .and_then(|mut addrs| addrs.next())
                .map(|addr| {
                    std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok()
                })
                .unwrap_or(false)
        }
        Dependency::FileContainsKey {
            path, format, key, ..
        } => read_file_value(path, format, key).is_some(),
        Dependency::FileExists { path } => Path::new(path).exists(),
        Dependency::ProcessExited { name } => exit_registry.lock().unwrap().contains(name),
        Dependency::TcpNotListening { address, .. } => {
            use std::net::ToSocketAddrs;
            !address
                .to_socket_addrs()
                .ok()
                .and_then(|mut addrs| addrs.next())
                .map(|addr| {
                    std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok()
                })
                .unwrap_or(false)
        }
        Dependency::FileNotExists { path } => !Path::new(path).exists(),
        Dependency::ProcessNotRunning { pattern } => std::process::Command::new("pgrep")
            .args(["-f", pattern])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| !s.success())
            .unwrap_or(true),
    }
}

fn poll_interval(dep: &Dependency) -> Duration {
    match dep {
        Dependency::HttpHealthCheck { poll_interval, .. } => {
            poll_interval.unwrap_or(Duration::from_secs(1))
        }
        Dependency::TcpConnect { poll_interval, .. } => {
            poll_interval.unwrap_or(Duration::from_secs(1))
        }
        Dependency::FileContainsKey { poll_interval, .. } => {
            poll_interval.unwrap_or(Duration::from_secs(1))
        }
        Dependency::FileExists { .. } => Duration::from_secs(1),
        Dependency::ProcessExited { .. } => Duration::from_millis(100),
        Dependency::TcpNotListening { poll_interval, .. } => {
            poll_interval.unwrap_or(Duration::from_secs(1))
        }
        Dependency::FileNotExists { .. } => Duration::from_secs(1),
        Dependency::ProcessNotRunning { .. } => Duration::from_secs(1),
    }
}

fn timeout(dep: &Dependency) -> Duration {
    match dep {
        Dependency::HttpHealthCheck { timeout, .. } => timeout.unwrap_or(Duration::from_secs(60)),
        Dependency::TcpConnect { timeout, .. } => timeout.unwrap_or(Duration::from_secs(60)),
        Dependency::FileContainsKey { timeout, .. } => timeout.unwrap_or(Duration::from_secs(60)),
        Dependency::FileExists { .. } => Duration::from_secs(60),
        Dependency::ProcessExited { .. } => Duration::from_secs(60),
        Dependency::TcpNotListening { timeout, .. } => timeout.unwrap_or(Duration::from_secs(60)),
        Dependency::FileNotExists { .. } => Duration::from_secs(60),
        Dependency::ProcessNotRunning { .. } => Duration::from_secs(60),
    }
}

fn description(dep: &Dependency) -> String {
    match dep {
        Dependency::HttpHealthCheck { url, code, .. } => {
            format!("HTTP {code} from {url}")
        }
        Dependency::TcpConnect { address, .. } => {
            format!("tcp connect: {address}")
        }
        Dependency::FileContainsKey { path, key, .. } => {
            format!("file contains key '{key}' in {path}")
        }
        Dependency::FileExists { path } => {
            format!("file exists: {path}")
        }
        Dependency::ProcessExited { name } => {
            format!("process exited: {name}")
        }
        Dependency::TcpNotListening { address, .. } => {
            format!("tcp not listening: {address}")
        }
        Dependency::FileNotExists { path } => {
            format!("file not exists: {path}")
        }
        Dependency::ProcessNotRunning { pattern } => {
            format!("process not running: {pattern}")
        }
    }
}

fn wait_for_dependencies(
    config: &ProcessConfig,
    shutdown: &AtomicBool,
    logger: &Arc<Mutex<Logger>>,
    exit_registry: &Arc<Mutex<HashSet<String>>>,
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

            if start.elapsed() > timeout(dep) {
                let desc = description(dep);
                logger
                    .lock()
                    .unwrap()
                    .log_line(&config.name, &format!("dependency timed out: {desc}"));
                return false;
            }

            if check(dep, &agent, exit_registry) {
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
    exit_registry: Arc<Mutex<HashSet<String>>>,
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
    use crate::config::FileFormat;

    fn make_exit_registry() -> Arc<Mutex<HashSet<String>>> {
        Arc::new(Mutex::new(HashSet::new()))
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
            depends,
            once: false,
            for_each: None,
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
    fn file_exists_check_returns_false_then_true() {
        let path = temp_path("check_file");
        let _ = std::fs::remove_file(&path);
        let dep = Dependency::FileExists { path: path.clone() };
        let agent = ureq::Agent::new_with_config(
            ureq::config::Config::builder()
                .timeout_global(Some(Duration::from_secs(5)))
                .build(),
        );

        let exit_registry = make_exit_registry();
        assert!(!check(&dep, &agent, &exit_registry));
        std::fs::write(&path, "").unwrap();
        assert!(check(&dep, &agent, &exit_registry));
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
            make_exit_registry(),
        );

        // Set shutdown immediately
        shutdown.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        assert_eq!(pending.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn process_exited_check_returns_false_then_true() {
        let dep = Dependency::ProcessExited {
            name: "migrate".to_string(),
        };
        let agent = ureq::Agent::new_with_config(
            ureq::config::Config::builder()
                .timeout_global(Some(Duration::from_secs(5)))
                .build(),
        );
        let exit_registry = make_exit_registry();

        assert!(!check(&dep, &agent, &exit_registry));
        exit_registry.lock().unwrap().insert("migrate".to_string());
        assert!(check(&dep, &agent, &exit_registry));
    }

    #[test]
    fn tcp_connect_check_returns_false_for_closed_port() {
        let dep = Dependency::TcpConnect {
            address: "127.0.0.1:1".to_string(),
            poll_interval: None,
            timeout: None,
        };
        let agent = ureq::Agent::new_with_config(
            ureq::config::Config::builder()
                .timeout_global(Some(Duration::from_secs(5)))
                .build(),
        );
        let exit_registry = make_exit_registry();
        assert!(!check(&dep, &agent, &exit_registry));
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
            }],
        );
        let (tx, rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let logger = make_logger(&["tcp-waiter"]);
        let pending = Arc::new(AtomicUsize::new(1));

        let _handle = spawn_waiter(
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
        assert_eq!(pending.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn wait_for_process_exited_dependency() {
        let config = make_config(
            "api",
            vec![Dependency::ProcessExited {
                name: "migrate".to_string(),
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
        exit_registry.lock().unwrap().insert("migrate".to_string());

        let received = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        match received {
            SupervisorCommand::Spawn(config) => assert_eq!(config.name, "api"),
            _ => panic!("expected Spawn"),
        }
        assert_eq!(pending.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn file_contains_check_returns_false_for_missing_file() {
        let dep = Dependency::FileContainsKey {
            path: "/tmp/procman_nonexistent_file_12345".to_string(),
            format: FileFormat::Yaml,
            key: serde_json_path::JsonPath::parse("$.foo").unwrap(),
            env: None,
            poll_interval: None,
            timeout: None,
        };
        let agent = ureq::Agent::new_with_config(
            ureq::config::Config::builder()
                .timeout_global(Some(Duration::from_secs(5)))
                .build(),
        );
        let exit_registry = make_exit_registry();
        assert!(!check(&dep, &agent, &exit_registry));
    }

    #[test]
    fn file_contains_check_returns_false_for_missing_key() {
        let path = temp_path("contains_missing_key");
        std::fs::write(&path, "other_key: value\n").unwrap();
        let dep = Dependency::FileContainsKey {
            path: path.clone(),
            format: FileFormat::Yaml,
            key: serde_json_path::JsonPath::parse("$.foo").unwrap(),
            env: None,
            poll_interval: None,
            timeout: None,
        };
        let agent = ureq::Agent::new_with_config(
            ureq::config::Config::builder()
                .timeout_global(Some(Duration::from_secs(5)))
                .build(),
        );
        let exit_registry = make_exit_registry();
        assert!(!check(&dep, &agent, &exit_registry));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn file_contains_check_returns_true_for_yaml() {
        let path = temp_path("contains_yaml");
        std::fs::write(&path, "database:\n  url: postgres://localhost\n").unwrap();
        let dep = Dependency::FileContainsKey {
            path: path.clone(),
            format: FileFormat::Yaml,
            key: serde_json_path::JsonPath::parse("$.database").unwrap(),
            env: None,
            poll_interval: None,
            timeout: None,
        };
        let agent = ureq::Agent::new_with_config(
            ureq::config::Config::builder()
                .timeout_global(Some(Duration::from_secs(5)))
                .build(),
        );
        let exit_registry = make_exit_registry();
        assert!(check(&dep, &agent, &exit_registry));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn file_contains_check_returns_true_for_json() {
        let path = temp_path("contains_json");
        std::fs::write(&path, r#"{"api_key": "secret123"}"#).unwrap();
        let dep = Dependency::FileContainsKey {
            path: path.clone(),
            format: FileFormat::Json,
            key: serde_json_path::JsonPath::parse("$.api_key").unwrap(),
            env: None,
            poll_interval: None,
            timeout: None,
        };
        let agent = ureq::Agent::new_with_config(
            ureq::config::Config::builder()
                .timeout_global(Some(Duration::from_secs(5)))
                .build(),
        );
        let exit_registry = make_exit_registry();
        assert!(check(&dep, &agent, &exit_registry));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn file_contains_check_dot_path_navigation() {
        let path = temp_path("contains_dotpath");
        std::fs::write(&path, "a:\n  b:\n    c: deep_value\n").unwrap();
        let dep = Dependency::FileContainsKey {
            path: path.clone(),
            format: FileFormat::Yaml,
            key: serde_json_path::JsonPath::parse("$.a.b.c").unwrap(),
            env: None,
            poll_interval: None,
            timeout: None,
        };
        let agent = ureq::Agent::new_with_config(
            ureq::config::Config::builder()
                .timeout_global(Some(Duration::from_secs(5)))
                .build(),
        );
        let exit_registry = make_exit_registry();
        assert!(check(&dep, &agent, &exit_registry));

        // Also verify the value
        let key = serde_json_path::JsonPath::parse("$.a.b.c").unwrap();
        assert_eq!(
            read_file_value(&path, &FileFormat::Yaml, &key),
            Some("deep_value".to_string())
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn collect_dependency_env_extracts_values() {
        let path = temp_path("collect_env");
        std::fs::write(&path, "database:\n  url: postgres://localhost:5432/test\n").unwrap();
        let deps = vec![Dependency::FileContainsKey {
            path: path.clone(),
            format: FileFormat::Yaml,
            key: serde_json_path::JsonPath::parse("$.database.url").unwrap(),
            env: Some("DATABASE_URL".to_string()),
            poll_interval: None,
            timeout: None,
        }];
        let env = collect_dependency_env(&deps).unwrap();
        assert_eq!(
            env.get("DATABASE_URL").unwrap(),
            "postgres://localhost:5432/test"
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn file_contains_array_filter() {
        let path = temp_path("contains_array_filter");
        std::fs::write(
            &path,
            "envs:\n  - alias: local\n    rpc: \"http://127.0.0.1:9000\"\n  - alias: remote\n    rpc: \"http://example.com:9000\"\n",
        )
        .unwrap();
        let key = serde_json_path::JsonPath::parse("$.envs[?(@.alias == 'local')].rpc").unwrap();
        assert_eq!(
            read_file_value(&path, &FileFormat::Yaml, &key),
            Some("http://127.0.0.1:9000".to_string())
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn collect_dependency_env_skips_no_env_deps() {
        let path = temp_path("collect_env_skip");
        std::fs::write(&path, "key: value\n").unwrap();
        let deps = vec![Dependency::FileContainsKey {
            path: path.clone(),
            format: FileFormat::Yaml,
            key: serde_json_path::JsonPath::parse("$.key").unwrap(),
            env: None,
            poll_interval: None,
            timeout: None,
        }];
        let env = collect_dependency_env(&deps).unwrap();
        assert!(env.is_empty());
        std::fs::remove_file(&path).unwrap();
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
            },
        };
        let env = std::collections::HashMap::new();
        let err = def.into_dependency(&env).unwrap_err();
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
                },
                Dependency::FileExists { path: path.clone() },
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
        exit_registry.lock().unwrap().insert("setup".to_string());

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
                },
                Dependency::FileExists { path: path.clone() },
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
        exit_registry.lock().unwrap().insert("setup".to_string());

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
    fn tcp_not_listening_check_returns_true_for_free_port() {
        let dep = Dependency::TcpNotListening {
            address: "127.0.0.1:19291".to_string(),
            poll_interval: None,
            timeout: None,
        };
        let agent = ureq::Agent::new_with_config(
            ureq::config::Config::builder()
                .timeout_global(Some(Duration::from_secs(5)))
                .build(),
        );
        let exit_registry = make_exit_registry();
        assert!(check(&dep, &agent, &exit_registry));
    }

    #[test]
    fn tcp_not_listening_check_returns_false_for_bound_port() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let dep = Dependency::TcpNotListening {
            address: addr,
            poll_interval: None,
            timeout: None,
        };
        let agent = ureq::Agent::new_with_config(
            ureq::config::Config::builder()
                .timeout_global(Some(Duration::from_secs(5)))
                .build(),
        );
        let exit_registry = make_exit_registry();
        assert!(!check(&dep, &agent, &exit_registry));
    }

    #[test]
    fn file_not_exists_check_returns_true_for_missing_file() {
        let dep = Dependency::FileNotExists {
            path: "/tmp/procman_nonexistent_file_99999".to_string(),
        };
        let agent = ureq::Agent::new_with_config(
            ureq::config::Config::builder()
                .timeout_global(Some(Duration::from_secs(5)))
                .build(),
        );
        let exit_registry = make_exit_registry();
        assert!(check(&dep, &agent, &exit_registry));
    }

    #[test]
    fn file_not_exists_check_returns_false_for_existing_file() {
        let path = temp_path("not_exists_existing");
        std::fs::write(&path, "").unwrap();
        let dep = Dependency::FileNotExists { path: path.clone() };
        let agent = ureq::Agent::new_with_config(
            ureq::config::Config::builder()
                .timeout_global(Some(Duration::from_secs(5)))
                .build(),
        );
        let exit_registry = make_exit_registry();
        assert!(!check(&dep, &agent, &exit_registry));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn process_not_running_check_returns_true_for_no_match() {
        let dep = Dependency::ProcessNotRunning {
            pattern: "zzz_procman_nonexistent_process_zzz".to_string(),
        };
        let agent = ureq::Agent::new_with_config(
            ureq::config::Config::builder()
                .timeout_global(Some(Duration::from_secs(5)))
                .build(),
        );
        let exit_registry = make_exit_registry();
        assert!(check(&dep, &agent, &exit_registry));
    }

    #[test]
    fn process_not_running_check_returns_false_for_running_process() {
        // pgrep -f "procman" should match the test binary itself
        let dep = Dependency::ProcessNotRunning {
            pattern: "procman".to_string(),
        };
        let agent = ureq::Agent::new_with_config(
            ureq::config::Config::builder()
                .timeout_global(Some(Duration::from_secs(5)))
                .build(),
        );
        let exit_registry = make_exit_registry();
        assert!(!check(&dep, &agent, &exit_registry));
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
                },
                Dependency::FileExists { path: path.clone() },
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
}
