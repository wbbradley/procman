use std::{collections::HashMap, time::Duration};

use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct ProcessConfig {
    pub name: String,
    pub env: HashMap<String, String>,
    pub program: String,
    pub args: Vec<String>,
    pub depends: Vec<Dependency>,
    pub once: bool,
}

#[derive(Clone)]
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
    FileExists {
        path: String,
    },
    ProcessExited {
        name: String,
    },
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
    FileExists {
        path: String,
    },
    ProcessExited {
        process_exited: String,
    },
}

impl DependencyDef {
    pub fn into_dependency(self) -> Dependency {
        match self {
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
            DependencyDef::FileExists { path } => Dependency::FileExists { path },
            DependencyDef::ProcessExited { process_exited } => Dependency::ProcessExited {
                name: process_exited,
            },
        }
    }
}

pub enum SupervisorCommand {
    Spawn(ProcessConfig),
    Shutdown { message: String },
}
