use std::{collections::HashMap, time::Duration};

use anyhow::{Result, bail};

#[derive(Clone, Debug)]
pub enum ArgType {
    String,
    Bool,
}

#[derive(Clone, Debug)]
pub struct ArgDef {
    pub name: String,
    pub namespace: Option<String>,
    pub short: Option<String>,
    pub description: Option<String>,
    pub arg_type: ArgType,
    pub default: Option<String>,
    pub env: Option<String>,
}

pub struct ConfigHeader {
    pub log_dir: Option<String>,
    pub log_time: bool,
    pub arg_defs: Vec<ArgDef>,
}

#[derive(Clone, Debug)]
pub enum ForEachConfig {
    Glob {
        pattern: String,
        variable: String,
    },
    Array {
        values: Vec<String>,
        variable: String,
    },
    Range {
        start: i64,
        end: i64,
        inclusive: bool,
        variable: String,
    },
}

impl ForEachConfig {
    pub fn variable(&self) -> &str {
        match self {
            ForEachConfig::Glob { variable, .. }
            | ForEachConfig::Array { variable, .. }
            | ForEachConfig::Range { variable, .. } => variable,
        }
    }
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
    pub is_task: bool,
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
        poll_interval: Option<Duration>,
        timeout: Option<Duration>,
        retry: bool,
    },
    ProcessExited {
        name: String,
        poll_interval: Option<Duration>,
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
        poll_interval: Option<Duration>,
        timeout: Option<Duration>,
        retry: bool,
    },
    ProcessNotRunning {
        pattern: String,
        poll_interval: Option<Duration>,
        timeout: Option<Duration>,
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

pub enum SupervisorCommand {
    Spawn(Box<ProcessConfig>),
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
            poll_interval: None,
            timeout: None,
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
