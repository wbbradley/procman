use std::{collections::HashMap, time::Duration};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct ForEachConfig {
    pub glob: String,
    pub variable: String,
}

#[derive(Clone, Debug)]
pub enum OnFailAction {
    Shutdown,
    Debug,
    Log,
    Spawn(String),
}

#[derive(Clone, Debug)]
pub struct Watch {
    pub name: String,
    pub check: Dependency,
    pub initial_delay: Duration,
    pub poll_interval: Duration,
    pub failure_threshold: u32,
    pub on_fail: OnFailAction,
}

#[derive(Clone, Debug)]
pub struct ProcessConfig {
    pub name: String,
    pub env: HashMap<String, String>,
    pub run: String,
    pub condition: Option<String>,
    pub depends: Vec<Dependency>,
    pub once: bool,
    pub for_each: Option<ForEachConfig>,
    pub autostart: bool,
    pub watches: Vec<Watch>,
}

#[derive(Clone, Debug)]
pub enum FileFormat {
    Json,
    Yaml,
}

#[derive(Clone, Debug)]
pub enum Dependency {
    HttpHealthCheck {
        url: String,
        code: u16,
        poll_interval: Option<Duration>,
        timeout: Option<Duration>,
        retry: bool,
    },
    TcpConnect {
        address: String,
        poll_interval: Option<Duration>,
        timeout: Option<Duration>,
        retry: bool,
    },
    FileContainsKey {
        path: String,
        format: FileFormat,
        key: serde_json_path::JsonPath,
        env: Option<String>,
        poll_interval: Option<Duration>,
        timeout: Option<Duration>,
        retry: bool,
    },
    FileExists {
        path: String,
        retry: bool,
    },
    ProcessExited {
        name: String,
        timeout: Option<Duration>,
        retry: bool,
    },
    TcpNotListening {
        address: String,
        poll_interval: Option<Duration>,
        timeout: Option<Duration>,
        retry: bool,
    },
    FileNotExists {
        path: String,
        retry: bool,
    },
    ProcessNotRunning {
        pattern: String,
        retry: bool,
    },
}

impl Dependency {
    pub(crate) fn map_string_field(
        &mut self,
        mut f: impl FnMut(&mut String) -> Result<()>,
    ) -> Result<()> {
        match self {
            Dependency::HttpHealthCheck { url, .. } => f(url),
            Dependency::TcpConnect { address, .. } => f(address),
            Dependency::FileContainsKey { path, .. } => f(path),
            Dependency::FileExists { path, .. } => f(path),
            Dependency::ProcessExited { name, .. } => f(name),
            Dependency::TcpNotListening { address, .. } => f(address),
            Dependency::FileNotExists { path, .. } => f(path),
            Dependency::ProcessNotRunning { pattern, .. } => f(pattern),
        }
    }

    pub fn substitute_var(&mut self, var: &str, value: &str) {
        let _ = self.map_string_field(|s| {
            *s = s
                .replace(&format!("${}", var), value)
                .replace(&format!("${{{}}}", var), value);
            Ok(())
        });
    }

    pub(crate) fn resolve_env_vars(&mut self, env: &HashMap<String, String>) -> Result<()> {
        self.map_string_field(|s| {
            *s = expand_env_vars(s, env)?;
            Ok(())
        })
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct FileContainsDef {
    pub path: String,
    pub format: String,
    pub key: String,
    #[serde(default)]
    pub env: Option<String>,
    #[serde(default)]
    pub poll_interval: Option<f64>,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub retry: Option<bool>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ProcessExitedDef {
    pub name: String,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub retry: Option<bool>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum DependencyDef {
    HttpHealthCheck {
        url: String,
        code: u16,
        poll_interval: Option<f64>,
        timeout_seconds: Option<u64>,
        #[serde(default)]
        retry: Option<bool>,
    },
    TcpConnect {
        tcp: String,
        poll_interval: Option<f64>,
        timeout_seconds: Option<u64>,
        #[serde(default)]
        retry: Option<bool>,
    },
    FileContainsKey {
        file_contains: FileContainsDef,
    },
    FileExists {
        path: String,
        #[serde(default)]
        retry: Option<bool>,
    },
    ProcessExitedExpanded {
        process_exited: ProcessExitedDef,
    },
    ProcessExited {
        process_exited: String,
        #[serde(default)]
        retry: Option<bool>,
    },
    TcpNotListening {
        not_listening: String,
        poll_interval: Option<f64>,
        timeout_seconds: Option<u64>,
        #[serde(default)]
        retry: Option<bool>,
    },
    FileNotExists {
        not_exists: String,
        #[serde(default)]
        retry: Option<bool>,
    },
    ProcessNotRunning {
        not_running: String,
        #[serde(default)]
        retry: Option<bool>,
    },
}

pub(crate) fn expand_env_vars(s: &str, env: &HashMap<String, String>) -> Result<String> {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '$' {
            match chars.peek() {
                Some('$') => {
                    chars.next();
                    result.push('$');
                }
                Some('{') => {
                    chars.next();
                    let mut name = String::new();
                    while let Some(&c) = chars.peek() {
                        if c == '}' {
                            chars.next();
                            break;
                        }
                        name.push(c);
                        chars.next();
                    }
                    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                        bail!(
                            "invalid environment variable name '${{{}}}': names may only contain letters, digits, and underscores",
                            name
                        );
                    }
                    if let Some(val) = env.get(&name) {
                        result.push_str(val);
                    } else {
                        bail!("undefined environment variable: ${{{name}}}");
                    }
                }
                Some(&c) if c == '_' || c.is_ascii_alphabetic() => {
                    let mut name = String::new();
                    name.push(c);
                    chars.next();
                    while let Some(&c) = chars.peek() {
                        if c == '_' || c.is_ascii_alphanumeric() {
                            name.push(c);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    if let Some(val) = env.get(&name) {
                        result.push_str(val);
                    } else {
                        bail!("undefined environment variable: ${name}");
                    }
                }
                _ => {
                    result.push('$');
                }
            }
        } else {
            result.push(ch);
        }
    }
    Ok(result)
}

impl DependencyDef {
    pub fn into_dependency(self) -> Result<Dependency> {
        Ok(match self {
            DependencyDef::HttpHealthCheck {
                url,
                code,
                poll_interval,
                timeout_seconds,
                retry,
            } => Dependency::HttpHealthCheck {
                url,
                code,
                poll_interval: poll_interval.map(Duration::from_secs_f64),
                timeout: timeout_seconds.map(Duration::from_secs),
                retry: retry.unwrap_or(true),
            },
            DependencyDef::TcpConnect {
                tcp,
                poll_interval,
                timeout_seconds,
                retry,
            } => Dependency::TcpConnect {
                address: tcp,
                poll_interval: poll_interval.map(Duration::from_secs_f64),
                timeout: timeout_seconds.map(Duration::from_secs),
                retry: retry.unwrap_or(true),
            },
            DependencyDef::FileContainsKey { file_contains } => {
                let format = match file_contains.format.as_str() {
                    "json" => FileFormat::Json,
                    "yaml" => FileFormat::Yaml,
                    other => bail!(
                        "unsupported file_contains format: {other:?} (expected \"json\" or \"yaml\")"
                    ),
                };
                let key = serde_json_path::JsonPath::parse(&file_contains.key).map_err(|e| {
                    anyhow!(
                        "invalid JSONPath in file_contains.key {:?}: {e}",
                        file_contains.key
                    )
                })?;
                Dependency::FileContainsKey {
                    path: file_contains.path,
                    format,
                    key,
                    env: file_contains.env,
                    poll_interval: file_contains.poll_interval.map(Duration::from_secs_f64),
                    timeout: file_contains.timeout_seconds.map(Duration::from_secs),
                    retry: file_contains.retry.unwrap_or(true),
                }
            }
            DependencyDef::FileExists { path, retry } => Dependency::FileExists {
                path,
                retry: retry.unwrap_or(true),
            },
            DependencyDef::ProcessExitedExpanded { process_exited } => Dependency::ProcessExited {
                name: process_exited.name,
                timeout: process_exited.timeout_seconds.map(Duration::from_secs),
                retry: process_exited.retry.unwrap_or(true),
            },
            DependencyDef::ProcessExited {
                process_exited,
                retry,
            } => Dependency::ProcessExited {
                name: process_exited,
                timeout: Some(Duration::from_secs(60)),
                retry: retry.unwrap_or(true),
            },
            DependencyDef::TcpNotListening {
                not_listening,
                poll_interval,
                timeout_seconds,
                retry,
            } => Dependency::TcpNotListening {
                address: not_listening,
                poll_interval: poll_interval.map(Duration::from_secs_f64),
                timeout: timeout_seconds.map(Duration::from_secs),
                retry: retry.unwrap_or(true),
            },
            DependencyDef::FileNotExists { not_exists, retry } => Dependency::FileNotExists {
                path: not_exists,
                retry: retry.unwrap_or(true),
            },
            DependencyDef::ProcessNotRunning { not_running, retry } => {
                Dependency::ProcessNotRunning {
                    pattern: not_running,
                    retry: retry.unwrap_or(true),
                }
            }
        })
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum OnFailActionDef {
    Simple(String),
    Spawn { spawn: String },
}

#[derive(Clone, Deserialize, Serialize)]
pub struct WatchDef {
    #[serde(default)]
    pub name: Option<String>,
    pub check: DependencyDef,
    #[serde(default)]
    pub initial_delay: Option<f64>,
    #[serde(default)]
    pub poll_interval: Option<f64>,
    #[serde(default)]
    pub failure_threshold: Option<u32>,
    #[serde(default)]
    pub on_fail: Option<OnFailActionDef>,
}

impl WatchDef {
    pub fn into_watch(self, process_name: &str, index: usize) -> Result<Watch> {
        let name = self
            .name
            .unwrap_or_else(|| format!("{process_name}.watch-{index}"));
        let check = self.check.into_dependency()?;
        let on_fail = match self.on_fail {
            None => OnFailAction::Shutdown,
            Some(OnFailActionDef::Simple(s)) => match s.as_str() {
                "shutdown" => OnFailAction::Shutdown,
                "debug" => OnFailAction::Debug,
                "log" => OnFailAction::Log,
                other => bail!(
                    "unknown on_fail action: {other:?} (expected \"shutdown\", \"debug\", \"log\", or {{spawn: \"name\"}})"
                ),
            },
            Some(OnFailActionDef::Spawn { spawn }) => OnFailAction::Spawn(spawn),
        };
        Ok(Watch {
            name,
            check,
            initial_delay: Duration::from_secs_f64(self.initial_delay.unwrap_or(0.0)),
            poll_interval: Duration::from_secs_f64(self.poll_interval.unwrap_or(5.0)),
            failure_threshold: self.failure_threshold.unwrap_or(3),
            on_fail,
        })
    }
}

pub enum SupervisorCommand {
    Spawn(ProcessConfig),
    Shutdown { message: String },
    DebugPause { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn expand_empty_env() {
        let e = HashMap::new();
        assert_eq!(expand_env_vars("hello world", &e).unwrap(), "hello world");
    }

    #[test]
    fn expand_simple_var() {
        let e = env(&[("HOME", "/Users/me")]);
        assert_eq!(expand_env_vars("$HOME", &e).unwrap(), "/Users/me");
    }

    #[test]
    fn expand_braced_var() {
        let e = env(&[("HOME", "/Users/me")]);
        assert_eq!(expand_env_vars("${HOME}", &e).unwrap(), "/Users/me");
    }

    #[test]
    fn expand_unknown_var_errors() {
        let e = HashMap::new();
        assert!(expand_env_vars("$UNKNOWN", &e).is_err());
        assert!(expand_env_vars("${UNKNOWN}", &e).is_err());
    }

    #[test]
    fn expand_partial_unknown_errors() {
        let e = env(&[("KNOWN", "ok")]);
        assert!(expand_env_vars("$KNOWN/$UNKNOWN", &e).is_err());
    }

    #[test]
    fn expand_mixed_text() {
        let e = env(&[("DIR", "/data")]);
        assert_eq!(
            expand_env_vars("prefix/$DIR/suffix", &e).unwrap(),
            "prefix//data/suffix"
        );
    }

    #[test]
    fn expand_multiple_vars() {
        let e = env(&[("A", "1"), ("B", "2")]);
        assert_eq!(expand_env_vars("$A-$B", &e).unwrap(), "1-2");
    }

    #[test]
    fn expand_dollar_dollar_escape() {
        let e = env(&[("X", "val")]);
        assert_eq!(expand_env_vars("$$X", &e).unwrap(), "$X");
    }

    #[test]
    fn expand_var_adjacent_to_dot() {
        let e = env(&[("VAR", "value")]);
        assert_eq!(expand_env_vars("$VAR.txt", &e).unwrap(), "value.txt");
    }

    #[test]
    fn expand_trailing_dollar() {
        let e = HashMap::new();
        assert_eq!(expand_env_vars("cost is $", &e).unwrap(), "cost is $");
    }

    #[test]
    fn expand_underscore_var() {
        let e = env(&[("MY_DIR", "/tmp")]);
        assert_eq!(expand_env_vars("$MY_DIR/file", &e).unwrap(), "/tmp/file");
    }

    #[test]
    fn substitute_var_http_health_check() {
        let mut dep = Dependency::HttpHealthCheck {
            url: "http://${HOST}:8080/health".to_string(),
            code: 200,
            poll_interval: None,
            timeout: None,
            retry: true,
        };
        dep.substitute_var("HOST", "localhost");
        match &dep {
            Dependency::HttpHealthCheck { url, .. } => {
                assert_eq!(url, "http://localhost:8080/health");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn substitute_var_file_exists() {
        let mut dep = Dependency::FileExists {
            path: "/tmp/$NODE/healthy".to_string(),
            retry: true,
        };
        dep.substitute_var("NODE", "node-0");
        match &dep {
            Dependency::FileExists { path, .. } => {
                assert_eq!(path, "/tmp/node-0/healthy");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn expand_braced_var_rejects_invalid_chars() {
        let e = HashMap::new();
        let err = expand_env_vars("${VAR:-fallback}", &e).unwrap_err();
        assert!(
            format!("{err}").contains("invalid environment variable name"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn expand_braced_var_rejects_dots() {
        let e = HashMap::new();
        let err = expand_env_vars("${MY.VAR}", &e).unwrap_err();
        assert!(
            format!("{err}").contains("invalid environment variable name"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn expand_braced_var_rejects_hyphens() {
        let e = HashMap::new();
        let err = expand_env_vars("${my-var}", &e).unwrap_err();
        assert!(
            format!("{err}").contains("invalid environment variable name"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn substitute_var_tcp_connect() {
        let mut dep = Dependency::TcpConnect {
            address: "${HOST}:5432".to_string(),
            poll_interval: None,
            timeout: None,
            retry: true,
        };
        dep.substitute_var("HOST", "db.local");
        match &dep {
            Dependency::TcpConnect { address, .. } => {
                assert_eq!(address, "db.local:5432");
            }
            _ => panic!("wrong variant"),
        }
    }
}
