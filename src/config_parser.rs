use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::{
    config::{Dependency, DependencyDef, ForEachConfig, OnFailAction, ProcessConfig, WatchDef},
    output,
};

#[derive(Deserialize)]
struct ProcmanFile {
    config: Option<ConfigSection>,
    jobs: HashMap<String, YamlProcessDef>,
}

#[derive(Deserialize)]
struct ConfigSection {
    logs: Option<String>,
}

#[derive(Deserialize)]
struct ForEachDef {
    glob: String,
    #[serde(rename = "as")]
    variable: String,
}

#[derive(Deserialize)]
struct YamlProcessDef {
    env: Option<HashMap<String, String>>,
    run: String,
    depends: Option<Vec<DependencyDef>>,
    once: Option<bool>,
    for_each: Option<ForEachDef>,
    autostart: Option<bool>,
    watch: Option<Vec<WatchDef>>,
}

pub fn parse(
    path: &str,
    extra_env: &HashMap<String, String>,
) -> Result<(Vec<ProcessConfig>, Option<String>)> {
    let content = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    let file: ProcmanFile = match serde_yaml::from_str(&content) {
        Ok(f) => f,
        Err(e) => {
            // Detect old flat format and give a helpful error
            if let Ok(flat) = serde_yaml::from_str::<HashMap<String, serde_yaml::Value>>(&content)
                && flat.values().any(|v| v.get("run").is_some())
            {
                bail!(
                    "config format has changed: wrap process definitions under a 'jobs:' key. See README for the new format."
                );
            }
            return Err(e).with_context(|| format!("parsing YAML from {path}"));
        }
    };

    let log_dir = file.config.as_ref().and_then(|c| c.logs.clone());

    if file.jobs.is_empty() {
        bail!("no jobs found in {path}");
    }

    let mut base_env: HashMap<String, String> = std::env::vars().collect();
    base_env.extend(extra_env.iter().map(|(k, v)| (k.clone(), v.clone())));

    let mut configs = Vec::new();
    for (name, def) in file.jobs {
        let mut env = base_env.clone();
        if let Some(proc_env) = def.env {
            for (k, v) in proc_env {
                env.insert(k, v);
            }
        }

        if def.run.trim().is_empty() {
            bail!("empty run command for process {name}");
        }

        let depends: Vec<_> = def
            .depends
            .unwrap_or_default()
            .into_iter()
            .map(|d| d.into_dependency(&env))
            .collect::<Result<Vec<_>>>()
            .with_context(|| format!("parsing dependencies for process {name}"))?;

        let watches = def
            .watch
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(i, w)| w.into_watch(&name, i, &env))
            .collect::<Result<Vec<_>>>()
            .with_context(|| format!("parsing watches for process {name}"))?;

        configs.push(ProcessConfig {
            name,
            env,
            run: def.run,
            depends,
            once: def.once.unwrap_or(false),
            for_each: def.for_each.map(|fe| ForEachConfig {
                glob: fe.glob,
                variable: fe.variable,
            }),
            autostart: def.autostart.unwrap_or(true),
            watches,
        });
    }

    output::validate_config_templates(&configs)?;
    validate_dependency_graph(&configs)?;
    validate_watches(&configs)?;
    Ok((configs, log_dir))
}

fn validate_dependency_graph(configs: &[ProcessConfig]) -> Result<()> {
    use std::collections::HashSet;

    let names: HashSet<&str> = configs.iter().map(|c| c.name.as_str()).collect();
    let mut edges: HashMap<&str, Vec<&str>> = HashMap::new();

    for config in configs {
        let deps: Vec<&str> = config
            .depends
            .iter()
            .filter_map(|d| match d {
                Dependency::ProcessExited { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();

        for dep in &deps {
            if !names.contains(dep) {
                bail!(
                    "process '{}' depends on unknown process '{dep}'",
                    config.name
                );
            }
        }
        edges.insert(config.name.as_str(), deps);
    }

    // DFS cycle detection: 0=white, 1=gray (in stack), 2=black (done)
    let mut color: HashMap<&str, u8> = names.iter().map(|&n| (n, 0u8)).collect();
    let mut path: Vec<&str> = Vec::new();

    for &start in &names {
        if color[start] == 0
            && let Some(cycle) = dfs_find_cycle(start, &edges, &mut color, &mut path)
        {
            bail!("circular dependency: {}", cycle.join(" -> "));
        }
    }
    Ok(())
}

fn dfs_find_cycle<'a>(
    node: &'a str,
    edges: &HashMap<&'a str, Vec<&'a str>>,
    color: &mut HashMap<&'a str, u8>,
    path: &mut Vec<&'a str>,
) -> Option<Vec<String>> {
    color.insert(node, 1);
    path.push(node);

    if let Some(neighbors) = edges.get(node) {
        for &neighbor in neighbors {
            match color[neighbor] {
                1 => {
                    let start = path.iter().position(|&n| n == neighbor).unwrap();
                    let mut cycle: Vec<String> =
                        path[start..].iter().map(|s| s.to_string()).collect();
                    cycle.push(neighbor.to_string());
                    return Some(cycle);
                }
                0 => {
                    if let Some(cycle) = dfs_find_cycle(neighbor, edges, color, path) {
                        return Some(cycle);
                    }
                }
                _ => {}
            }
        }
    }

    color.insert(node, 2);
    path.pop();
    None
}

fn validate_watches(configs: &[ProcessConfig]) -> Result<()> {
    use std::collections::HashSet;

    let all_names: HashSet<&str> = configs.iter().map(|c| c.name.as_str()).collect();

    for config in configs {
        let mut watch_names: HashSet<&str> = HashSet::new();
        for watch in &config.watches {
            if !watch_names.insert(&watch.name) {
                bail!(
                    "process '{}' has duplicate watch name '{}'",
                    config.name,
                    watch.name
                );
            }

            if let OnFailAction::Spawn(ref target) = watch.on_fail {
                if !all_names.contains(target.as_str()) {
                    bail!(
                        "process '{}' watch '{}' references unknown spawn target '{target}'",
                        config.name,
                        watch.name,
                    );
                }
                let target_config = configs.iter().find(|c| c.name == *target).unwrap();
                if target_config.autostart {
                    bail!(
                        "process '{}' watch '{}' spawn target '{target}' must have autostart: false",
                        config.name,
                        watch.name,
                    );
                }
            }
        }
    }
    Ok(())
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
        let path = write_yaml("jobs:\n  web:\n    run: echo hello\n");
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, "web");
        assert_eq!(configs[0].run, "echo hello");
        assert!(configs[0].depends.is_empty());
        assert!(!configs[0].once);
    }

    #[test]
    fn parse_with_env() {
        let path = write_yaml(
            "jobs:\n  worker:\n    env:\n      RUST_LOG: debug\n      PORT: \"3000\"\n    run: my-server --port 3000\n",
        );
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, "worker");
        assert_eq!(configs[0].env.get("RUST_LOG").unwrap(), "debug");
        assert_eq!(configs[0].env.get("PORT").unwrap(), "3000");
        assert_eq!(configs[0].run, "my-server --port 3000");
    }

    #[test]
    fn parse_with_http_dependency() {
        let path = write_yaml(
            "jobs:\n  api:\n    depends:\n      - url: http://localhost:8080/health\n        code: 200\n    run: worker start\n",
        );
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].depends.len(), 1);
        match &configs[0].depends[0] {
            Dependency::HttpHealthCheck {
                url,
                code,
                poll_interval,
                timeout,
                ..
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
        let path = write_yaml(
            "jobs:\n  api:\n    depends:\n      - path: /tmp/ready.flag\n    run: worker start\n",
        );
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        assert_eq!(configs[0].depends.len(), 1);
        match &configs[0].depends[0] {
            Dependency::FileExists { path, .. } => {
                assert_eq!(path, "/tmp/ready.flag");
            }
            _ => panic!("expected FileExists"),
        }
    }

    #[test]
    fn parse_with_http_dependency_options() {
        let path = write_yaml(
            "jobs:\n  api:\n    depends:\n      - url: http://localhost:8080/\n        code: 200\n        poll_interval: 0.5\n        timeout_seconds: 30\n    run: worker\n",
        );
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
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
        let path =
            write_yaml("jobs:\n  web:\n    run: echo web\n  worker:\n    run: echo worker\n");
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        assert_eq!(configs.len(), 2);
        let names: Vec<&str> = configs.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"web"));
        assert!(names.contains(&"worker"));
    }

    #[test]
    fn parse_invalid_yaml_returns_error() {
        let path = write_yaml("not: valid: yaml: [[[");
        assert!(parse(&path, &HashMap::new()).is_err());
    }

    #[test]
    fn parse_empty_jobs_returns_error() {
        let path = write_yaml("jobs: {}");
        assert!(parse(&path, &HashMap::new()).is_err());
    }

    #[test]
    fn parse_missing_run_returns_error() {
        let path = write_yaml("jobs:\n  web:\n    env:\n      FOO: bar\n");
        assert!(parse(&path, &HashMap::new()).is_err());
    }

    #[test]
    fn parse_with_process_exited_dependency() {
        let path = write_yaml(
            "jobs:\n  api:\n    depends:\n      - process_exited: db-migrate\n    run: api-server start\n  db-migrate:\n    run: echo migrate\n    once: true\n",
        );
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        let api = configs.iter().find(|c| c.name == "api").unwrap();
        assert_eq!(api.depends.len(), 1);
        match &api.depends[0] {
            Dependency::ProcessExited { name, .. } => {
                assert_eq!(name, "db-migrate");
            }
            _ => panic!("expected ProcessExited"),
        }
    }

    #[test]
    fn parse_with_tcp_dependency() {
        let path = write_yaml(
            "jobs:\n  api:\n    depends:\n      - tcp: \"127.0.0.1:50051\"\n    run: api-server start\n",
        );
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        assert_eq!(configs[0].depends.len(), 1);
        match &configs[0].depends[0] {
            Dependency::TcpConnect {
                address,
                poll_interval,
                timeout,
                ..
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
            "jobs:\n  api:\n    depends:\n      - tcp: \"127.0.0.1:50051\"\n        poll_interval: 0.5\n        timeout_seconds: 30\n    run: api-server start\n",
        );
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        match &configs[0].depends[0] {
            Dependency::TcpConnect {
                address,
                poll_interval,
                timeout,
                ..
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
        let path = write_yaml("jobs:\n  migrate:\n    run: echo done\n    once: true\n");
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        assert_eq!(configs.len(), 1);
        assert!(configs[0].once);
    }

    #[test]
    fn parse_with_template_in_env() {
        let yaml = "\
jobs:
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
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        assert_eq!(configs.len(), 2);
        let app = configs.iter().find(|c| c.name == "app").unwrap();
        assert_eq!(app.env.get("DB_URL").unwrap(), "${{ setup.DB_URL }}");
    }

    #[test]
    fn parse_with_file_contains_dependency() {
        let path = write_yaml(
            "jobs:\n  api:\n    depends:\n      - file_contains:\n          path: /tmp/config.yaml\n          format: yaml\n          key: \"$.database.url\"\n          env: DATABASE_URL\n    run: api-server start\n",
        );
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        assert_eq!(configs[0].depends.len(), 1);
        match &configs[0].depends[0] {
            Dependency::FileContainsKey { path, key, env, .. } => {
                assert_eq!(path, "/tmp/config.yaml");
                assert_eq!(key.to_string(), "$.database.url");
                assert_eq!(env.as_deref(), Some("DATABASE_URL"));
            }
            _ => panic!("expected FileContainsKey"),
        }
    }

    #[test]
    fn parse_with_template_in_run() {
        let yaml = "\
jobs:
  setup:
    run: echo done
    once: true
  app:
    depends:
      - process_exited: setup
    run: echo ${{ setup.DB_URL }}
";
        let path = write_yaml(yaml);
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        assert_eq!(configs.len(), 2);
        let app = configs.iter().find(|c| c.name == "app").unwrap();
        assert_eq!(app.run, "echo ${{ setup.DB_URL }}");
    }

    #[test]
    fn parse_for_each_glob() {
        let yaml = "\
jobs:
  nodes:
    for_each:
      glob: \"/tmp/test-*.yaml\"
      as: CONFIG_PATH
    run: echo $CONFIG_PATH
    once: true
";
        let path = write_yaml(yaml);
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        assert_eq!(configs.len(), 1);
        let fe = configs[0].for_each.as_ref().unwrap();
        assert_eq!(fe.glob, "/tmp/test-*.yaml");
        assert_eq!(fe.variable, "CONFIG_PATH");
        assert!(configs[0].once);
    }

    #[test]
    fn parse_for_each_without_as_errors() {
        let yaml = "\
jobs:
  nodes:
    for_each:
      glob: \"/tmp/test-*.yaml\"
    run: echo hello
";
        let path = write_yaml(yaml);
        assert!(parse(&path, &HashMap::new()).is_err());
    }

    #[test]
    fn parse_circular_dependency_detected() {
        let yaml = "\
jobs:
  a:
    depends:
      - process_exited: b
    run: echo a
  b:
    depends:
      - process_exited: a
    run: echo b
";
        let path = write_yaml(yaml);
        let err = parse(&path, &HashMap::new()).unwrap_err();
        assert!(
            format!("{err}").contains("circular dependency"),
            "expected circular dependency error, got: {err}"
        );
    }

    #[test]
    fn parse_self_dependency_detected() {
        let yaml = "\
jobs:
  a:
    depends:
      - process_exited: a
    run: echo a
";
        let path = write_yaml(yaml);
        let err = parse(&path, &HashMap::new()).unwrap_err();
        assert!(
            format!("{err}").contains("circular dependency"),
            "expected circular dependency error, got: {err}"
        );
    }

    #[test]
    fn parse_three_way_cycle_detected() {
        let yaml = "\
jobs:
  a:
    depends:
      - process_exited: b
    run: echo a
  b:
    depends:
      - process_exited: c
    run: echo b
  c:
    depends:
      - process_exited: a
    run: echo c
";
        let path = write_yaml(yaml);
        let err = parse(&path, &HashMap::new()).unwrap_err();
        assert!(
            format!("{err}").contains("circular dependency"),
            "expected circular dependency error, got: {err}"
        );
    }

    #[test]
    fn parse_unknown_process_dependency_errors() {
        let yaml = "\
jobs:
  a:
    depends:
      - process_exited: nonexistent
    run: echo a
";
        let path = write_yaml(yaml);
        let err = parse(&path, &HashMap::new()).unwrap_err();
        assert!(
            format!("{err}").contains("unknown process"),
            "expected unknown process error, got: {err}"
        );
    }

    #[test]
    fn parse_multiline_run() {
        let yaml = "\
jobs:
  web:
    run: |
      echo starting
      exec my-server --port 3000
";
        let path = write_yaml(yaml);
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, "web");
        assert!(configs[0].run.contains('\n'));
    }

    #[test]
    fn parse_valid_dependency_chain_ok() {
        let yaml = "\
jobs:
  a:
    depends:
      - process_exited: b
    run: echo a
  b:
    run: echo b
";
        let path = write_yaml(yaml);
        parse(&path, &HashMap::new()).unwrap();
    }

    #[test]
    fn parse_rejects_invalid_jsonpath_key() {
        let path = write_yaml(
            "jobs:\n  api:\n    depends:\n      - file_contains:\n          path: /tmp/config.yaml\n          format: yaml\n          key: \"$[invalid\"\n    run: echo hi\n",
        );
        let err = parse(&path, &HashMap::new()).unwrap_err();
        assert!(
            format!("{err:?}").contains("invalid JSONPath"),
            "expected JSONPath error, got: {err:?}"
        );
    }

    #[test]
    fn parse_with_not_listening_dependency() {
        let path = write_yaml(
            "jobs:\n  api:\n    depends:\n      - not_listening: \"127.0.0.1:8080\"\n    run: api-server start\n",
        );
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        assert_eq!(configs[0].depends.len(), 1);
        match &configs[0].depends[0] {
            Dependency::TcpNotListening {
                address,
                poll_interval,
                timeout,
                ..
            } => {
                assert_eq!(address, "127.0.0.1:8080");
                assert!(poll_interval.is_none());
                assert!(timeout.is_none());
            }
            _ => panic!("expected TcpNotListening"),
        }
    }

    #[test]
    fn parse_with_not_exists_dependency() {
        let path = write_yaml(
            "jobs:\n  api:\n    depends:\n      - not_exists: /tmp/api.lock\n    run: api-server start\n",
        );
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        assert_eq!(configs[0].depends.len(), 1);
        match &configs[0].depends[0] {
            Dependency::FileNotExists { path, .. } => {
                assert_eq!(path, "/tmp/api.lock");
            }
            _ => panic!("expected FileNotExists"),
        }
    }

    #[test]
    fn parse_with_not_running_dependency() {
        let path = write_yaml(
            "jobs:\n  api:\n    depends:\n      - not_running: \"old-api.*\"\n    run: api-server start\n",
        );
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        assert_eq!(configs[0].depends.len(), 1);
        match &configs[0].depends[0] {
            Dependency::ProcessNotRunning { pattern, .. } => {
                assert_eq!(pattern, "old-api.*");
            }
            _ => panic!("expected ProcessNotRunning"),
        }
    }

    #[test]
    fn parse_extra_env_overrides_system() {
        let path = write_yaml("jobs:\n  web:\n    run: echo hello\n");
        let mut extra = HashMap::new();
        extra.insert("PROCMAN_TEST_EXTRA".to_string(), "from_cli".to_string());
        let (configs, _) = parse(&path, &extra).unwrap();
        assert_eq!(
            configs[0].env.get("PROCMAN_TEST_EXTRA").unwrap(),
            "from_cli"
        );
    }

    #[test]
    fn parse_yaml_env_overrides_extra_env() {
        let path =
            write_yaml("jobs:\n  web:\n    env:\n      MY_VAR: from_yaml\n    run: echo hello\n");
        let mut extra = HashMap::new();
        extra.insert("MY_VAR".to_string(), "from_cli".to_string());
        let (configs, _) = parse(&path, &extra).unwrap();
        assert_eq!(configs[0].env.get("MY_VAR").unwrap(), "from_yaml");
    }

    #[test]
    fn parse_with_retry_false() {
        let path = write_yaml(
            "jobs:\n  api:\n    depends:\n      - path: /tmp/ready.flag\n        retry: false\n    run: echo hi\n",
        );
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        match &configs[0].depends[0] {
            Dependency::FileExists { retry, .. } => assert!(!retry),
            _ => panic!("expected FileExists"),
        }
    }

    #[test]
    fn parse_with_retry_default() {
        let path = write_yaml(
            "jobs:\n  api:\n    depends:\n      - path: /tmp/ready.flag\n    run: echo hi\n",
        );
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        match &configs[0].depends[0] {
            Dependency::FileExists { retry, .. } => assert!(retry),
            _ => panic!("expected FileExists"),
        }
    }

    #[test]
    fn parse_autostart_false() {
        let path = write_yaml("jobs:\n  web:\n    run: echo hello\n    autostart: false\n");
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        assert_eq!(configs.len(), 1);
        assert!(!configs[0].autostart);
    }

    #[test]
    fn parse_autostart_default_true() {
        let path = write_yaml("jobs:\n  web:\n    run: echo hello\n");
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        assert!(configs[0].autostart);
    }

    #[test]
    fn parse_autostart_false_still_validated() {
        let yaml = "\
jobs:
  dormant:
    autostart: false
    depends:
      - process_exited: nonexistent
    run: echo dormant
";
        let path = write_yaml(yaml);
        let err = parse(&path, &HashMap::new()).unwrap_err();
        assert!(format!("{err}").contains("unknown process"), "{err}");
    }

    #[test]
    fn parse_watch_with_http_check() {
        let yaml = "\
jobs:
  web:
    run: echo hello
    watch:
      - name: health
        check:
          url: http://localhost:8080/health
          code: 200
        initial_delay: 5.0
        failure_threshold: 3
        on_fail: shutdown
";
        let path = write_yaml(yaml);
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        assert_eq!(configs[0].watches.len(), 1);
        let w = &configs[0].watches[0];
        assert_eq!(w.name, "health");
        assert_eq!(w.initial_delay, Duration::from_secs(5));
        assert_eq!(w.failure_threshold, 3);
        assert!(matches!(w.on_fail, OnFailAction::Shutdown));
    }

    #[test]
    fn parse_watch_with_tcp_check() {
        let yaml = "\
jobs:
  web:
    run: echo hello
    watch:
      - name: db
        check:
          tcp: localhost:5432
        poll_interval: 10.0
";
        let path = write_yaml(yaml);
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        let w = &configs[0].watches[0];
        assert_eq!(w.name, "db");
        assert_eq!(w.poll_interval, Duration::from_secs(10));
    }

    #[test]
    fn parse_watch_defaults() {
        let yaml = "\
jobs:
  web:
    run: echo hello
    watch:
      - name: disk
        check:
          path: /var/run/healthy
";
        let path = write_yaml(yaml);
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        let w = &configs[0].watches[0];
        assert_eq!(w.initial_delay, Duration::from_secs(0));
        assert_eq!(w.poll_interval, Duration::from_secs(5));
        assert_eq!(w.failure_threshold, 3);
        assert!(matches!(w.on_fail, OnFailAction::Shutdown));
    }

    #[test]
    fn parse_watch_spawn_on_fail() {
        let yaml = "\
jobs:
  web:
    run: echo hello
    watch:
      - name: recovery
        check:
          path: /tmp/healthy
        on_fail:
          spawn: recovery-script
  recovery-script:
    run: echo recovering
    autostart: false
";
        let path = write_yaml(yaml);
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        let web = configs.iter().find(|c| c.name == "web").unwrap();
        assert!(
            matches!(&web.watches[0].on_fail, OnFailAction::Spawn(name) if name == "recovery-script")
        );
    }

    #[test]
    fn parse_watch_spawn_target_must_exist() {
        let yaml = "\
jobs:
  web:
    run: echo hello
    watch:
      - name: recovery
        check:
          path: /tmp/healthy
        on_fail:
          spawn: nonexistent
";
        let path = write_yaml(yaml);
        let err = parse(&path, &HashMap::new()).unwrap_err();
        assert!(format!("{err}").contains("unknown spawn target"), "{err}");
    }

    #[test]
    fn parse_watch_spawn_target_must_be_autostart_false() {
        let yaml = "\
jobs:
  web:
    run: echo hello
    watch:
      - name: recovery
        check:
          path: /tmp/healthy
        on_fail:
          spawn: helper
  helper:
    run: echo helper
";
        let path = write_yaml(yaml);
        let err = parse(&path, &HashMap::new()).unwrap_err();
        assert!(format!("{err}").contains("autostart: false"), "{err}");
    }

    #[test]
    fn parse_watch_duplicate_names_rejected() {
        let yaml = "\
jobs:
  web:
    run: echo hello
    watch:
      - name: health
        check:
          path: /tmp/a
      - name: health
        check:
          path: /tmp/b
";
        let path = write_yaml(yaml);
        let err = parse(&path, &HashMap::new()).unwrap_err();
        assert!(format!("{err}").contains("duplicate watch name"), "{err}");
    }

    #[test]
    fn parse_watch_auto_name() {
        let yaml = "\
jobs:
  web:
    run: echo hello
    watch:
      - check:
          path: /tmp/a
      - check:
          path: /tmp/b
";
        let path = write_yaml(yaml);
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        assert_eq!(configs[0].watches[0].name, "web.watch-0");
        assert_eq!(configs[0].watches[1].name, "web.watch-1");
    }

    #[test]
    fn parse_no_watches_default() {
        let path = write_yaml("jobs:\n  web:\n    run: echo hello\n");
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        assert!(configs[0].watches.is_empty());
    }

    #[test]
    fn parse_watch_on_fail_debug() {
        let yaml = "\
jobs:
  web:
    run: echo hello
    watch:
      - name: health
        check:
          path: /tmp/a
        on_fail: debug
";
        let path = write_yaml(yaml);
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        assert!(matches!(configs[0].watches[0].on_fail, OnFailAction::Debug));
    }

    #[test]
    fn parse_watch_on_fail_log() {
        let yaml = "\
jobs:
  web:
    run: echo hello
    watch:
      - name: health
        check:
          path: /tmp/a
        on_fail: log
";
        let path = write_yaml(yaml);
        let (configs, _) = parse(&path, &HashMap::new()).unwrap();
        assert!(matches!(configs[0].watches[0].on_fail, OnFailAction::Log));
    }

    #[test]
    fn parse_with_config_section() {
        let yaml = "\
config:
  logs: ./my-logs
jobs:
  web:
    run: echo hello
";
        let path = write_yaml(yaml);
        let (configs, log_dir) = parse(&path, &HashMap::new()).unwrap();
        assert_eq!(configs.len(), 1);
        assert_eq!(log_dir.as_deref(), Some("./my-logs"));
    }

    #[test]
    fn parse_without_config_section() {
        let path = write_yaml("jobs:\n  web:\n    run: echo hello\n");
        let (_, log_dir) = parse(&path, &HashMap::new()).unwrap();
        assert!(log_dir.is_none());
    }

    #[test]
    fn parse_old_format_gives_helpful_error() {
        let path = write_yaml("web:\n  run: echo hello\n");
        let err = parse(&path, &HashMap::new()).unwrap_err();
        assert!(
            format!("{err}").contains("config format has changed"),
            "expected migration error, got: {err}"
        );
    }

    #[test]
    fn parse_missing_jobs_key_errors() {
        let path = write_yaml("config:\n  logs: ./logs\n");
        assert!(parse(&path, &HashMap::new()).is_err());
    }
}
