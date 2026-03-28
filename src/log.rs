use std::{
    collections::HashMap,
    fs::{self, File},
    io::Write,
    path::PathBuf,
};

use anyhow::{Context, Result};

pub struct Logger {
    max_name_len: usize,
    log_files: HashMap<String, File>,
    combined_log: Option<File>,
    log_dir: PathBuf,
    print_to_stdout: bool,
}

impl Logger {
    pub fn new(names: &[String], custom_log_dir: Option<&str>) -> Result<Self> {
        let log_dir = PathBuf::from(custom_log_dir.unwrap_or("logs/procman"));
        let _ = fs::remove_dir_all(&log_dir);
        fs::create_dir_all(&log_dir).context("creating logs directory")?;
        let combined_log =
            File::create(log_dir.join("procman.log")).context("creating combined log file")?;
        Self::with_options(names, log_dir, true, Some(combined_log))
    }

    fn with_options(
        names: &[String],
        log_dir: PathBuf,
        print_to_stdout: bool,
        combined_log: Option<File>,
    ) -> Result<Self> {
        fs::create_dir_all(&log_dir).context("creating logs directory")?;
        let max_name_len = names.iter().map(|n| n.len()).max().unwrap_or(0);
        let mut log_files = HashMap::new();
        for name in names {
            if name == "procman" {
                continue;
            }
            let file = File::create(log_dir.join(format!("{name}.log")))
                .context("creating log file for {name}")?;
            log_files.insert(name.clone(), file);
        }
        Ok(Self {
            max_name_len,
            log_files,
            combined_log,
            log_dir,
            print_to_stdout,
        })
    }

    #[cfg(test)]
    pub fn new_for_test(names: &[String], log_dir: PathBuf) -> Result<Self> {
        Self::with_options(names, log_dir, false, None)
    }

    pub fn log_dir(&self) -> &std::path::Path {
        &self.log_dir
    }

    pub fn add_process(&mut self, name: &str) -> Result<()> {
        if self.log_files.contains_key(name) {
            return Ok(());
        }
        self.max_name_len = self.max_name_len.max(name.len());
        let file = File::create(self.log_dir.join(format!("{name}.log")))
            .with_context(|| format!("creating log file for {name}"))?;
        self.log_files.insert(name.to_string(), file);
        Ok(())
    }

    pub fn log_line(&mut self, name: &str, line: &str) {
        let padded = format!("{:>width$}", name, width = self.max_name_len);
        if let Some(f) = &mut self.combined_log {
            let _ = writeln!(f, "{padded} | {line}");
        }
        if self.print_to_stdout {
            println!("{padded} | {line}");
        }
        if let Some(f) = self.log_files.get_mut(name) {
            let _ = writeln!(f, "{line}");
        }
    }
}
