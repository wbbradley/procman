use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::{
    config::{DependencyDef, ProcessConfig},
    output,
};

#[derive(Deserialize)]
struct YamlProcessDef {
    env: Option<HashMap<String, String>>,
    run: String,
    depends: Option<Vec<DependencyDef>>,
    once: Option<bool>,
}

pub fn parse(path: &str) -> Result<Vec<ProcessConfig>> {
    let content = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    let defs: HashMap<String, YamlProcessDef> =
        serde_yaml::from_str(&content).with_context(|| format!("parsing YAML from {path}"))?;

    if defs.is_empty() {
        bail!("no processes found in {path}");
    }

    let base_env: HashMap<String, String> = std::env::vars().collect();

    let mut configs = Vec::new();
    for (name, def) in defs {
        let mut env = base_env.clone();
        if let Some(proc_env) = def.env {
            for (k, v) in proc_env {
                env.insert(k, v);
            }
        }

        // Validate non-template runs can be shell-parsed
        if !def.run.contains("${{") {
            let tokens = shell_words::split(&def.run)
                .with_context(|| format!("parsing run command for process {name}"))?;
            if tokens.is_empty() {
                bail!("empty run command for process {name}");
            }
        } else if def.run.trim().is_empty() {
            bail!("empty run command for process {name}");
        }

        let depends: Vec<_> = def
            .depends
            .unwrap_or_default()
            .into_iter()
            .map(DependencyDef::into_dependency)
            .collect::<Result<Vec<_>>>()
            .with_context(|| format!("parsing dependencies for process {name}"))?;

        configs.push(ProcessConfig {
            name,
            env,
            run: def.run,
            depends,
            once: def.once.unwrap_or(false),
        });
    }

    output::validate_config_templates(&configs)?;
    Ok(configs)
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use super::*;
    use crate::config::Dependency;

    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn write_yaml(content: &str) -> String {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("procman_yaml_test_{}_{id}", std::process::id(),));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("procman.yaml");
        std::fs::write(&path, content).unwrap();
        path.to_str().unwrap().to_string()
    }

    #[test]
    fn parse_simple_run() {
        let path = write_yaml("web:\n  run: echo hello\n");
        let configs = parse(&path).unwrap();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, "web");
        assert_eq!(configs[0].run, "echo hello");
        assert!(configs[0].depends.is_empty());
        assert!(!configs[0].once);
    }

    #[test]
    fn parse_with_env() {
        let path = write_yaml(
            "worker:\n  env:\n    RUST_LOG: debug\n    PORT: \"3000\"\n  run: my-server --port 3000\n",
        );
        let configs = parse(&path).unwrap();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, "worker");
        assert_eq!(configs[0].env.get("RUST_LOG").unwrap(), "debug");
        assert_eq!(configs[0].env.get("PORT").unwrap(), "3000");
        assert_eq!(configs[0].run, "my-server --port 3000");
    }

    #[test]
    fn parse_with_http_dependency() {
        let path = write_yaml(
            "api:\n  depends:\n    - url: http://localhost:8080/health\n      code: 200\n  run: worker start\n",
        );
        let configs = parse(&path).unwrap();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].depends.len(), 1);
        match &configs[0].depends[0] {
            Dependency::HttpHealthCheck {
                url,
                code,
                poll_interval,
                timeout,
            } => {
                assert_eq!(url, "http://localhost:8080/health");
                assert_eq!(*code, 200);
                assert!(poll_interval.is_none());
                assert!(timeout.is_none());
            }
            _ => panic!("expected HttpHealthCheck"),
        }
    }

    #[test]
    fn parse_with_file_dependency() {
        let path =
            write_yaml("api:\n  depends:\n    - path: /tmp/ready.flag\n  run: worker start\n");
        let configs = parse(&path).unwrap();
        assert_eq!(configs[0].depends.len(), 1);
        match &configs[0].depends[0] {
            Dependency::FileExists { path } => {
                assert_eq!(path, "/tmp/ready.flag");
            }
            _ => panic!("expected FileExists"),
        }
    }

    #[test]
    fn parse_with_http_dependency_options() {
        let path = write_yaml(
            "api:\n  depends:\n    - url: http://localhost:8080/\n      code: 200\n      poll_interval: 0.5\n      timeout_seconds: 30\n  run: worker\n",
        );
        let configs = parse(&path).unwrap();
        match &configs[0].depends[0] {
            Dependency::HttpHealthCheck {
                poll_interval,
                timeout,
                ..
            } => {
                assert_eq!(*poll_interval, Some(Duration::from_millis(500)));
                assert_eq!(*timeout, Some(Duration::from_secs(30)));
            }
            _ => panic!("expected HttpHealthCheck"),
        }
    }

    #[test]
    fn parse_multiple_processes() {
        let path = write_yaml("web:\n  run: echo web\nworker:\n  run: echo worker\n");
        let configs = parse(&path).unwrap();
        assert_eq!(configs.len(), 2);
        let names: Vec<&str> = configs.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"web"));
        assert!(names.contains(&"worker"));
    }

    #[test]
    fn parse_invalid_yaml_returns_error() {
        let path = write_yaml("not: valid: yaml: [[[");
        assert!(parse(&path).is_err());
    }

    #[test]
    fn parse_empty_processes_returns_error() {
        let path = write_yaml("{}");
        assert!(parse(&path).is_err());
    }

    #[test]
    fn parse_missing_run_returns_error() {
        let path = write_yaml("web:\n  env:\n    FOO: bar\n");
        assert!(parse(&path).is_err());
    }

    #[test]
    fn parse_with_process_exited_dependency() {
        let path = write_yaml(
            "api:\n  depends:\n    - process_exited: db-migrate\n  run: api-server start\n",
        );
        let configs = parse(&path).unwrap();
        assert_eq!(configs[0].depends.len(), 1);
        match &configs[0].depends[0] {
            Dependency::ProcessExited { name } => {
                assert_eq!(name, "db-migrate");
            }
            _ => panic!("expected ProcessExited"),
        }
    }

    #[test]
    fn parse_with_tcp_dependency() {
        let path = write_yaml(
            "api:\n  depends:\n    - tcp: \"127.0.0.1:50051\"\n  run: api-server start\n",
        );
        let configs = parse(&path).unwrap();
        assert_eq!(configs[0].depends.len(), 1);
        match &configs[0].depends[0] {
            Dependency::TcpConnect {
                address,
                poll_interval,
                timeout,
            } => {
                assert_eq!(address, "127.0.0.1:50051");
                assert!(poll_interval.is_none());
                assert!(timeout.is_none());
            }
            _ => panic!("expected TcpConnect"),
        }
    }

    #[test]
    fn parse_with_tcp_dependency_options() {
        let path = write_yaml(
            "api:\n  depends:\n    - tcp: \"127.0.0.1:50051\"\n      poll_interval: 0.5\n      timeout_seconds: 30\n  run: api-server start\n",
        );
        let configs = parse(&path).unwrap();
        match &configs[0].depends[0] {
            Dependency::TcpConnect {
                address,
                poll_interval,
                timeout,
            } => {
                assert_eq!(address, "127.0.0.1:50051");
                assert_eq!(*poll_interval, Some(Duration::from_millis(500)));
                assert_eq!(*timeout, Some(Duration::from_secs(30)));
            }
            _ => panic!("expected TcpConnect"),
        }
    }

    #[test]
    fn parse_with_once_flag() {
        let path = write_yaml("migrate:\n  run: echo done\n  once: true\n");
        let configs = parse(&path).unwrap();
        assert_eq!(configs.len(), 1);
        assert!(configs[0].once);
    }

    #[test]
    fn parse_with_template_in_env() {
        let yaml = "\
setup:
  run: echo done
  once: true
app:
  depends:
    - process_exited: setup
  env:
    DB_URL: \"${{ setup.DB_URL }}\"
  run: echo app
";
        let path = write_yaml(yaml);
        let configs = parse(&path).unwrap();
        assert_eq!(configs.len(), 2);
        let app = configs.iter().find(|c| c.name == "app").unwrap();
        assert_eq!(app.env.get("DB_URL").unwrap(), "${{ setup.DB_URL }}");
    }

    #[test]
    fn parse_with_file_contains_dependency() {
        let path = write_yaml(
            "api:\n  depends:\n    - file_contains:\n        path: /tmp/config.yaml\n        format: yaml\n        key: database.url\n        env: DATABASE_URL\n  run: api-server start\n",
        );
        let configs = parse(&path).unwrap();
        assert_eq!(configs[0].depends.len(), 1);
        match &configs[0].depends[0] {
            Dependency::FileContainsKey { path, key, env, .. } => {
                assert_eq!(path, "/tmp/config.yaml");
                assert_eq!(key, "database.url");
                assert_eq!(env.as_deref(), Some("DATABASE_URL"));
            }
            _ => panic!("expected FileContainsKey"),
        }
    }

    #[test]
    fn parse_with_template_in_run() {
        let yaml = "\
setup:
  run: echo done
  once: true
app:
  depends:
    - process_exited: setup
  run: echo ${{ setup.DB_URL }}
";
        let path = write_yaml(yaml);
        let configs = parse(&path).unwrap();
        assert_eq!(configs.len(), 2);
        let app = configs.iter().find(|c| c.name == "app").unwrap();
        assert_eq!(app.run, "echo ${{ setup.DB_URL }}");
    }
}
