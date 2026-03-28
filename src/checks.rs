use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Result, anyhow};

use crate::config::{Dependency, FileFormat};

pub fn read_file_value(
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

pub fn check(
    dep: &Dependency,
    agent: &ureq::Agent,
    exit_registry: &Arc<Mutex<HashMap<String, i32>>>,
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
        Dependency::FileExists { path, .. } => Path::new(path).exists(),
        Dependency::ProcessExited { name, .. } => exit_registry.lock().unwrap().contains_key(name),
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
        Dependency::FileNotExists { path, .. } => !Path::new(path).exists(),
        Dependency::ProcessNotRunning { pattern, .. } => std::process::Command::new("pgrep")
            .args(["-f", pattern])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| !s.success())
            .unwrap_or(true),
    }
}

pub fn poll_interval(dep: &Dependency) -> Duration {
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

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);

pub fn timeout(dep: &Dependency) -> Option<Duration> {
    match dep {
        Dependency::HttpHealthCheck { timeout, .. } => *timeout,
        Dependency::TcpConnect { timeout, .. } => *timeout,
        Dependency::FileContainsKey { timeout, .. } => *timeout,
        Dependency::FileExists { .. } => None,
        Dependency::ProcessExited { timeout, .. } => *timeout,
        Dependency::TcpNotListening { timeout, .. } => *timeout,
        Dependency::FileNotExists { .. } => None,
        Dependency::ProcessNotRunning { .. } => None,
    }
}

pub fn retry(dep: &Dependency) -> bool {
    match dep {
        Dependency::HttpHealthCheck { retry, .. } => *retry,
        Dependency::TcpConnect { retry, .. } => *retry,
        Dependency::FileContainsKey { retry, .. } => *retry,
        Dependency::FileExists { retry, .. } => *retry,
        Dependency::ProcessExited { retry, .. } => *retry,
        Dependency::TcpNotListening { retry, .. } => *retry,
        Dependency::FileNotExists { retry, .. } => *retry,
        Dependency::ProcessNotRunning { retry, .. } => *retry,
    }
}

pub fn description(dep: &Dependency) -> String {
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
        Dependency::FileExists { path, .. } => {
            format!("file exists: {path}")
        }
        Dependency::ProcessExited { name, .. } => {
            format!("process exited: {name}")
        }
        Dependency::TcpNotListening { address, .. } => {
            format!("tcp not listening: {address}")
        }
        Dependency::FileNotExists { path, .. } => {
            format!("file not exists: {path}")
        }
        Dependency::ProcessNotRunning { pattern, .. } => {
            format!("process not running: {pattern}")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::config::FileFormat;

    fn make_exit_registry() -> Arc<Mutex<HashMap<String, i32>>> {
        Arc::new(Mutex::new(HashMap::new()))
    }

    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_path(name: &str) -> String {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!(
                "procman_check_test_{name}_{}_{id}",
                std::process::id()
            ))
            .to_str()
            .unwrap()
            .to_string()
    }

    fn make_agent() -> ureq::Agent {
        ureq::Agent::new_with_config(
            ureq::config::Config::builder()
                .timeout_global(Some(Duration::from_secs(5)))
                .build(),
        )
    }

    #[test]
    fn file_exists_check_returns_false_then_true() {
        let path = temp_path("check_file");
        let _ = std::fs::remove_file(&path);
        let dep = Dependency::FileExists {
            path: path.clone(),
            retry: true,
        };
        let agent = make_agent();
        let exit_registry = make_exit_registry();
        assert!(!check(&dep, &agent, &exit_registry));
        std::fs::write(&path, "").unwrap();
        assert!(check(&dep, &agent, &exit_registry));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn process_exited_check_returns_false_then_true() {
        let dep = Dependency::ProcessExited {
            name: "migrate".to_string(),
            timeout: Some(Duration::from_secs(60)),
            retry: true,
        };
        let agent = make_agent();
        let exit_registry = make_exit_registry();

        assert!(!check(&dep, &agent, &exit_registry));
        exit_registry
            .lock()
            .unwrap()
            .insert("migrate".to_string(), 0);
        assert!(check(&dep, &agent, &exit_registry));
    }

    #[test]
    fn tcp_connect_check_returns_false_for_closed_port() {
        let dep = Dependency::TcpConnect {
            address: "127.0.0.1:1".to_string(),
            poll_interval: None,
            timeout: None,
            retry: true,
        };
        let agent = make_agent();
        let exit_registry = make_exit_registry();
        assert!(!check(&dep, &agent, &exit_registry));
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
            retry: true,
        };
        let agent = make_agent();
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
            retry: true,
        };
        let agent = make_agent();
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
            retry: true,
        };
        let agent = make_agent();
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
            retry: true,
        };
        let agent = make_agent();
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
            retry: true,
        };
        let agent = make_agent();
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
            retry: true,
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
            retry: true,
        }];
        let env = collect_dependency_env(&deps).unwrap();
        assert!(env.is_empty());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn tcp_not_listening_check_returns_true_for_free_port() {
        let dep = Dependency::TcpNotListening {
            address: "127.0.0.1:19291".to_string(),
            poll_interval: None,
            timeout: None,
            retry: true,
        };
        let agent = make_agent();
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
            retry: true,
        };
        let agent = make_agent();
        let exit_registry = make_exit_registry();
        assert!(!check(&dep, &agent, &exit_registry));
    }

    #[test]
    fn file_not_exists_check_returns_true_for_missing_file() {
        let dep = Dependency::FileNotExists {
            path: "/tmp/procman_nonexistent_file_99999".to_string(),
            retry: true,
        };
        let agent = make_agent();
        let exit_registry = make_exit_registry();
        assert!(check(&dep, &agent, &exit_registry));
    }

    #[test]
    fn file_not_exists_check_returns_false_for_existing_file() {
        let path = temp_path("not_exists_existing");
        std::fs::write(&path, "").unwrap();
        let dep = Dependency::FileNotExists {
            path: path.clone(),
            retry: true,
        };
        let agent = make_agent();
        let exit_registry = make_exit_registry();
        assert!(!check(&dep, &agent, &exit_registry));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn process_not_running_check_returns_true_for_no_match() {
        let dep = Dependency::ProcessNotRunning {
            pattern: "zzz_procman_nonexistent_process_zzz".to_string(),
            retry: true,
        };
        let agent = make_agent();
        let exit_registry = make_exit_registry();
        assert!(check(&dep, &agent, &exit_registry));
    }

    #[test]
    fn process_not_running_check_returns_false_for_running_process() {
        // pgrep -f "procman" should match the test binary itself
        let dep = Dependency::ProcessNotRunning {
            pattern: "procman".to_string(),
            retry: true,
        };
        let agent = make_agent();
        let exit_registry = make_exit_registry();
        assert!(!check(&dep, &agent, &exit_registry));
    }
}
