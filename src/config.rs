use std::{collections::HashMap, time::Duration};

use anyhow::{Result, bail};
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
        key: String,
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

impl DependencyDef {
    pub fn into_dependency(self) -> Result<Dependency> {
        Ok(match self {
            DependencyDef::HttpHealthCheck {
                url,
                code,
                poll_interval,
                timeout_seconds,
            } => Dependency::HttpHealthCheck {
                url,
                code,
                poll_interval: poll_interval.map(Duration::from_secs_f64),
                timeout: timeout_seconds.map(Duration::from_secs),
            },
            DependencyDef::TcpConnect {
                tcp,
                poll_interval,
                timeout_seconds,
            } => Dependency::TcpConnect {
                address: tcp,
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
                Dependency::FileContainsKey {
                    path: file_contains.path,
                    format,
                    key: file_contains.key,
                    env: file_contains.env,
                    poll_interval: file_contains.poll_interval.map(Duration::from_secs_f64),
                    timeout: file_contains.timeout_seconds.map(Duration::from_secs),
                }
            }
            DependencyDef::FileExists { path } => Dependency::FileExists { path },
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
