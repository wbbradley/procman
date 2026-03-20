use std::{collections::HashMap, time::Duration};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct ForEachConfig {
    pub glob: String,
    pub variable: String,
}

#[derive(Clone, Debug)]
pub struct ProcessConfig {
    pub name: String,
    pub env: HashMap<String, String>,
    pub run: String,
    pub depends: Vec<Dependency>,
    pub once: bool,
    pub for_each: Option<ForEachConfig>,
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
    },
    TcpConnect {
        address: String,
        poll_interval: Option<Duration>,
        timeout: Option<Duration>,
    },
    FileContainsKey {
        path: String,
        format: FileFormat,
        key: serde_json_path::JsonPath,
        env: Option<String>,
        poll_interval: Option<Duration>,
        timeout: Option<Duration>,
    },
    FileExists {
        path: String,
    },
    ProcessExited {
        name: String,
    },
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
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum DependencyDef {
    HttpHealthCheck {
        url: String,
        code: u16,
        poll_interval: Option<f64>,
        timeout_seconds: Option<u64>,
    },
    TcpConnect {
        tcp: String,
        poll_interval: Option<f64>,
        timeout_seconds: Option<u64>,
    },
    FileContainsKey {
        file_contains: FileContainsDef,
    },
    FileExists {
        path: String,
    },
    ProcessExited {
        process_exited: String,
    },
}

fn expand_env_vars(s: &str, env: &HashMap<String, String>) -> Result<String> {
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
    pub fn into_dependency(self, env: &HashMap<String, String>) -> Result<Dependency> {
        Ok(match self {
            DependencyDef::HttpHealthCheck {
                url,
                code,
                poll_interval,
                timeout_seconds,
            } => Dependency::HttpHealthCheck {
                url: expand_env_vars(&url, env)?,
                code,
                poll_interval: poll_interval.map(Duration::from_secs_f64),
                timeout: timeout_seconds.map(Duration::from_secs),
            },
            DependencyDef::TcpConnect {
                tcp,
                poll_interval,
                timeout_seconds,
            } => Dependency::TcpConnect {
                address: expand_env_vars(&tcp, env)?,
                poll_interval: poll_interval.map(Duration::from_secs_f64),
                timeout: timeout_seconds.map(Duration::from_secs),
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
                    path: expand_env_vars(&file_contains.path, env)?,
                    format,
                    key,
                    env: file_contains.env,
                    poll_interval: file_contains.poll_interval.map(Duration::from_secs_f64),
                    timeout: file_contains.timeout_seconds.map(Duration::from_secs),
                }
            }
            DependencyDef::FileExists { path } => Dependency::FileExists {
                path: expand_env_vars(&path, env)?,
            },
            DependencyDef::ProcessExited { process_exited } => Dependency::ProcessExited {
                name: process_exited,
            },
        })
    }
}

pub enum SupervisorCommand {
    Spawn(ProcessConfig),
    Shutdown { message: String },
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
}
