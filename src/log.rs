use std::{
    collections::HashMap,
    fs::{self, File},
    io::Write,
};

use anyhow::{Context, Result};

pub struct Logger {
    max_name_len: usize,
    log_files: HashMap<String, File>,
}

impl Logger {
    pub fn new(names: &[String]) -> Result<Self> {
        fs::create_dir_all("logs").context("creating logs directory")?;
        let max_name_len = names.iter().map(|n| n.len()).max().unwrap_or(0);
        let mut log_files = HashMap::new();
        for name in names {
            let file =
                File::create(format!("logs/{name}.log")).context("creating log file for {name}")?;
            log_files.insert(name.clone(), file);
        }
        Ok(Self {
            max_name_len,
            log_files,
        })
    }

    pub fn add_process(&mut self, name: &str) -> Result<()> {
        self.max_name_len = self.max_name_len.max(name.len());
        let file = File::create(format!("logs/{name}.log"))
            .with_context(|| format!("creating log file for {name}"))?;
        self.log_files.insert(name.to_string(), file);
        Ok(())
    }

    pub fn log_line(&mut self, name: &str, line: &str) {
        let padded = format!("{:>width$}", name, width = self.max_name_len);
        println!("{padded} | {line}");
        if let Some(f) = self.log_files.get_mut(name) {
            let _ = writeln!(f, "{line}");
        }
    }
}
