use std::{
    collections::HashMap,
    fs::{self, File},
    io::{IsTerminal, Write},
    path::PathBuf,
    time::Instant,
};

use anyhow::{Context, Result};

pub struct Logger {
    max_name_len: usize,
    log_files: HashMap<String, File>,
    combined_log: Option<File>,
    log_dir: PathBuf,
    print_to_stdout: bool,
    colorize: bool,
    log_time: bool,
    start_time: Instant,
}

impl Logger {
    pub fn new(names: &[String], custom_log_dir: Option<&str>, log_time: bool) -> Result<Self> {
        let log_dir = PathBuf::from(custom_log_dir.unwrap_or("logs/procman"));
        let _ = fs::remove_dir_all(&log_dir);
        fs::create_dir_all(&log_dir).context("creating logs directory")?;
        let log_dir = fs::canonicalize(&log_dir).context("canonicalizing logs directory")?;
        eprintln!("procman: logs dir: {}", log_dir.display());
        let combined_log_path = log_dir.join("procman.log");
        eprintln!("procman: log file: {}", combined_log_path.display());
        let combined_log =
            File::create(&combined_log_path).context("creating combined log file")?;
        Self::with_options(names, log_dir, true, Some(combined_log), log_time)
    }

    fn with_options(
        names: &[String],
        log_dir: PathBuf,
        print_to_stdout: bool,
        combined_log: Option<File>,
        log_time: bool,
    ) -> Result<Self> {
        fs::create_dir_all(&log_dir).context("creating logs directory")?;
        let max_name_len = names.iter().map(|n| n.len()).max().unwrap_or(0);
        let mut log_files = HashMap::new();
        for name in names {
            if name == "procman" {
                continue;
            }
            let log_path = log_dir.join(format!("{name}.log"));
            if print_to_stdout {
                eprintln!("procman: log file: {}", log_path.display());
            }
            let file = File::create(&log_path).context("creating log file for {name}")?;
            log_files.insert(name.clone(), file);
        }
        let colorize = print_to_stdout
            && std::env::var_os("NO_COLOR").is_none()
            && std::io::stdout().is_terminal();
        Ok(Self {
            max_name_len,
            log_files,
            combined_log,
            log_dir,
            print_to_stdout,
            colorize,
            log_time,
            start_time: Instant::now(),
        })
    }

    #[cfg(test)]
    pub fn new_for_test(names: &[String], log_dir: PathBuf) -> Result<Self> {
        Self::with_options(names, log_dir, false, None, false)
    }

    pub fn log_dir(&self) -> &std::path::Path {
        &self.log_dir
    }

    pub fn add_process(&mut self, name: &str) -> Result<()> {
        if self.log_files.contains_key(name) {
            return Ok(());
        }
        self.max_name_len = self.max_name_len.max(name.len());
        let log_path = self.log_dir.join(format!("{name}.log"));
        if self.print_to_stdout {
            eprintln!("procman: log file: {}", log_path.display());
        }
        let file =
            File::create(&log_path).with_context(|| format!("creating log file for {name}"))?;
        self.log_files.insert(name.to_string(), file);
        Ok(())
    }

    pub fn log_line(&mut self, name: &str, line: &str) {
        let padded = format!("{:>width$}", name, width = self.max_name_len);
        let time_suffix = if self.log_time {
            let elapsed = self.start_time.elapsed().as_secs_f64();
            format!(" {elapsed:.1}s")
        } else {
            String::new()
        };
        let plain_prefix = format!("{padded}{time_suffix} |");
        if let Some(f) = &mut self.combined_log {
            let _ = writeln!(f, "{plain_prefix} {line}");
        }
        if self.print_to_stdout {
            if self.colorize {
                let (r, g, b) = color_for_name(name);
                println!("\x1b[38;2;{r};{g};{b}m{padded}\x1b[0m{time_suffix} | {line}");
            } else {
                println!("{plain_prefix} {line}");
            }
        }
        if let Some(f) = self.log_files.get_mut(name) {
            let _ = writeln!(f, "{line}");
        }
    }
}

// Keep prefixes readable on typical dark terminal backgrounds.
const LIGHTNESS_FLOOR: f32 = 0.55;

fn color_for_name(name: &str) -> (u8, u8, u8) {
    // FNV-1a.
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in name.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let r = (hash & 0xff) as u8;
    let g = ((hash >> 8) & 0xff) as u8;
    let b = ((hash >> 16) & 0xff) as u8;
    apply_lightness_floor(r, g, b, LIGHTNESS_FLOOR)
}

// Tint toward white to raise HSL lightness without changing hue.
fn apply_lightness_floor(r: u8, g: u8, b: u8, floor: f32) -> (u8, u8, u8) {
    let rf = r as f32 / 255.0;
    let gf = g as f32 / 255.0;
    let bf = b as f32 / 255.0;
    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let l = (max + min) / 2.0;
    if l >= floor {
        return (r, g, b);
    }
    let t = ((floor - l) / (1.0 - l)).clamp(0.0, 1.0);
    let nr = rf + (1.0 - rf) * t;
    let ng = gf + (1.0 - gf) * t;
    let nb = bf + (1.0 - bf) * t;
    (
        (nr * 255.0).round() as u8,
        (ng * 255.0).round() as u8,
        (nb * 255.0).round() as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lightness(r: u8, g: u8, b: u8) -> f32 {
        let rf = r as f32 / 255.0;
        let gf = g as f32 / 255.0;
        let bf = b as f32 / 255.0;
        (rf.max(gf).max(bf) + rf.min(gf).min(bf)) / 2.0
    }

    #[test]
    fn color_for_name_is_deterministic() {
        assert_eq!(color_for_name("web"), color_for_name("web"));
        assert_ne!(color_for_name("web"), color_for_name("db"));
    }

    #[test]
    fn color_for_name_meets_lightness_floor() {
        // Sample a bunch of plausible names, plus edge cases.
        let names = [
            "", "a", "procman", "web", "db", "worker", "worker-1", "cache", "api", "frontend",
            "backend", "redis", "postgres", "consumer", "migrator",
        ];
        for name in names {
            let (r, g, b) = color_for_name(name);
            let l = lightness(r, g, b);
            // Allow a tiny epsilon for f32 rounding through u8.
            assert!(
                l >= LIGHTNESS_FLOOR - 0.01,
                "name={name:?} rgb=({r},{g},{b}) lightness={l} < floor={LIGHTNESS_FLOOR}"
            );
        }
    }

    #[test]
    fn apply_lightness_floor_noop_when_already_bright() {
        assert_eq!(apply_lightness_floor(200, 200, 200, 0.5), (200, 200, 200));
    }

    #[test]
    fn apply_lightness_floor_brightens_black_to_gray() {
        let (r, g, b) = apply_lightness_floor(0, 0, 0, 0.6);
        assert!(lightness(r, g, b) >= 0.6 - 0.01);
    }
}
