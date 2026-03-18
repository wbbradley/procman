use std::{collections::HashMap, time::Duration};

pub struct ProcessConfig {
    pub name: String,
    pub env: HashMap<String, String>,
    pub program: String,
    pub args: Vec<String>,
    pub depends: Vec<Dependency>,
}

pub enum Dependency {
    HttpHealthCheck {
        url: String,
        code: u16,
        poll_interval: Option<Duration>,
        timeout: Option<Duration>,
    },
    FileExists {
        path: String,
    },
}
