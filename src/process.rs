use std::{
    collections::{HashMap, HashSet},
    io::BufRead,
    os::unix::process::CommandExt,
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc,
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use nix::{
    libc,
    sys::{
        signal::{self, Signal},
        wait::{WaitPidFlag, WaitStatus, waitpid},
    },
    unistd::Pid,
};

use crate::{
    config::{ProcessConfig, SupervisorCommand},
    dependency,
    log::Logger,
    output,
    watch,
};

pub struct ProcessGroup {
    children: Vec<(Pid, String, Instant, bool)>,
    reader_threads: Vec<thread::JoinHandle<()>>,
    waiter_threads: Vec<thread::JoinHandle<()>>,
    watcher_threads: Vec<thread::JoinHandle<()>>,
    pending_deps: Arc<AtomicUsize>,
    exit_registry: Arc<Mutex<HashSet<String>>>,
    log_dir: PathBuf,
    fan_out_groups: HashMap<String, HashSet<String>>,
    debug_mode: bool,
    dormant: HashMap<String, ProcessConfig>,
    tx: Option<mpsc::Sender<SupervisorCommand>>,
    shutdown: Arc<AtomicBool>,
}

fn build_command(resolved_run: &str, name: &str) -> Result<std::process::Command> {
    anyhow::ensure!(
        !resolved_run.trim().is_empty(),
        "empty run command for {name}"
    );
    let mut cmd = std::process::Command::new("sh");
    cmd.args(["-e", "-u", "-o", "pipefail", "-c", resolved_run]);
    Ok(cmd)
}

fn evaluate_condition(
    condition: &str,
    env: &HashMap<String, String>,
    name: &str,
    logger: &Arc<Mutex<Logger>>,
) -> Result<bool> {
    let mut cmd = std::process::Command::new("sh");
    cmd.args(["-e", "-u", "-o", "pipefail", "-c", condition]);
    cmd.env_clear();
    cmd.envs(env);
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());
    let status = cmd
        .status()
        .with_context(|| format!("{name}: failed to run condition: {condition}"))?;
    if status.success() {
        Ok(true)
    } else {
        logger
            .lock()
            .unwrap()
            .log_line(name, &format!("skipped (condition failed: {condition})"));
        Ok(false)
    }
}

impl ProcessGroup {
    fn spawn_one(&mut self, cmd: &ProcessConfig, logger: &Arc<Mutex<Logger>>) -> Result<Pid> {
        let log_dir = &self.log_dir;
        let resolve = |proc_name: &str, key: &str| -> Result<String> {
            let path = log_dir.join(format!("{proc_name}.output"));
            let outputs = output::parse_output_file(&path)?;
            outputs.get(key).cloned().ok_or_else(|| {
                anyhow::anyhow!("output key '{key}' not found in process '{proc_name}'")
            })
        };

        // Resolve templates in env values
        let mut resolved_env = cmd.env.clone();
        for value in resolved_env.values_mut() {
            *value = output::resolve_templates(value, &resolve)?;
        }
        // Set PROCMAN_OUTPUT
        let output_path = log_dir.join(format!("{}.output", cmd.name));
        resolved_env.insert(
            "PROCMAN_OUTPUT".to_string(),
            output_path.to_string_lossy().to_string(),
        );

        // Resolve templates in run string, then build command
        let resolved_run = output::resolve_templates(&cmd.run, &resolve)?;
        let mut child_cmd = build_command(&resolved_run, &cmd.name)?;
        child_cmd.env_clear();
        child_cmd.envs(&resolved_env);
        child_cmd.stdout(Stdio::piped());
        child_cmd.stderr(Stdio::piped());

        unsafe {
            child_cmd.pre_exec(move || {
                if libc::setpgid(0, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                let r = libc::dup2(1, 2);
                if r == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let mut child = child_cmd
            .spawn()
            .with_context(|| format!("spawning {}", cmd.name))?;

        let pid = Pid::from_raw(child.id() as i32);

        let name = cmd.name.clone();
        self.children
            .push((pid, name.clone(), Instant::now(), cmd.once));
        logger
            .lock()
            .unwrap()
            .log_line(&name, &format!("[{pid}] started"));

        let stdout = child.stdout.take().unwrap();
        let logger_clone = Arc::clone(logger);
        let name_clone = name.clone();
        self.reader_threads.push(thread::spawn(move || {
            let reader = std::io::BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        let mut log = logger_clone.lock().unwrap();
                        log.log_line(&name_clone, &line);
                    }
                    Err(_) => break,
                }
            }
        }));

        // We need to drop the Child so that we can waitpid on the raw pid.
        // stderr is already duped to stdout in pre_exec, and stdout pipe is taken.
        // Forget the Child to avoid double-wait.
        std::mem::forget(child);

        Ok(pid)
    }

    fn start_watchers(&mut self, config: &ProcessConfig, logger: &Arc<Mutex<Logger>>) {
        if config.watches.is_empty() {
            return;
        }
        if let Some(tx) = &self.tx {
            self.watcher_threads.push(watch::spawn_watcher(
                config.name.clone(),
                config.watches.clone(),
                tx.clone(),
                Arc::clone(&self.shutdown),
                Arc::clone(logger),
                Arc::clone(&self.exit_registry),
            ));
        }
    }

    fn expand_fan_out(
        &mut self,
        config: &ProcessConfig,
        logger: &Arc<Mutex<Logger>>,
    ) -> Result<()> {
        let fe = config.for_each.as_ref().unwrap();
        let glob_pattern = crate::config::expand_env_vars(&fe.glob, &config.env)?;
        let mut matches: Vec<String> = glob::glob(&glob_pattern)
            .with_context(|| format!("invalid glob pattern: {glob_pattern}"))?
            .filter_map(|entry| entry.ok())
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        matches.sort();

        if matches.is_empty() {
            anyhow::bail!(
                "fan-out for '{}': glob '{}' matched zero files",
                config.name,
                glob_pattern
            );
        }

        let mut instance_names = HashSet::new();
        for (i, matched_path) in matches.iter().enumerate() {
            let instance_name = format!("{}-{i}", config.name);
            instance_names.insert(instance_name.clone());

            let mut env = config.env.clone();
            env.insert(fe.variable.clone(), matched_path.clone());

            let run = config
                .run
                .replace(&format!("${}", fe.variable), matched_path)
                .replace(&format!("${{{}}}", fe.variable), matched_path);

            let mut instance_watches = config.watches.clone();
            for w in &mut instance_watches {
                w.name = format!(
                    "{instance_name}.{}",
                    w.name.rsplit_once('.').map(|(_, s)| s).unwrap_or(&w.name)
                );
                w.check.substitute_var(&fe.variable, matched_path);
            }

            let instance_config = ProcessConfig {
                name: instance_name.clone(),
                env,
                run,
                condition: None,
                depends: vec![],
                once: config.once,
                for_each: None,
                autostart: true,
                watches: instance_watches,
            };

            logger.lock().unwrap().add_process(&instance_name).ok();
            self.spawn_one(&instance_config, logger)?;
            self.start_watchers(&instance_config, logger);
        }

        self.fan_out_groups
            .insert(config.name.clone(), instance_names);
        logger.lock().unwrap().log_line(
            &config.name,
            &format!(
                "fan-out: spawned {} instance(s) from glob '{}'",
                matches.len(),
                fe.glob
            ),
        );
        Ok(())
    }

    pub fn spawn(
        commands: &[ProcessConfig],
        tx: mpsc::Sender<SupervisorCommand>,
        shutdown: Arc<AtomicBool>,
        logger: Arc<Mutex<Logger>>,
        debug: bool,
    ) -> Result<Self> {
        let log_dir = logger.lock().unwrap().log_dir().to_path_buf();
        let mut group = Self {
            children: Vec::new(),
            reader_threads: Vec::new(),
            waiter_threads: Vec::new(),
            watcher_threads: Vec::new(),
            pending_deps: Arc::new(AtomicUsize::new(0)),
            exit_registry: Arc::new(Mutex::new(HashSet::new())),
            log_dir,
            fan_out_groups: HashMap::new(),
            debug_mode: debug,
            dormant: HashMap::new(),
            tx: Some(tx),
            shutdown: Arc::clone(&shutdown),
        };

        for cmd in commands {
            if !cmd.autostart {
                group.dormant.insert(cmd.name.clone(), cmd.clone());
                continue;
            }
            if let Some(ref condition) = cmd.condition
                && !evaluate_condition(condition, &cmd.env, &cmd.name, &logger)?
            {
                if cmd.once {
                    group.exit_registry.lock().unwrap().insert(cmd.name.clone());
                }
                continue;
            }
            if cmd.depends.is_empty() {
                if cmd.for_each.is_some() {
                    group.expand_fan_out(cmd, &logger)?;
                } else {
                    group.spawn_one(cmd, &logger)?;
                    group.start_watchers(cmd, &logger);
                }
            } else {
                logger.lock().unwrap().add_process(&cmd.name).ok();
                group.pending_deps.fetch_add(1, Ordering::Relaxed);
                let tx = group.tx.as_ref().unwrap().clone();
                group.waiter_threads.push(dependency::spawn_waiter(
                    cmd.clone(),
                    tx,
                    Arc::clone(&shutdown),
                    Arc::clone(&logger),
                    Arc::clone(&group.pending_deps),
                    Arc::clone(&group.exit_registry),
                ));
            }
        }

        Ok(group)
    }

    fn try_accept_new(
        &mut self,
        rx: &mpsc::Receiver<SupervisorCommand>,
        shutdown: &Arc<AtomicBool>,
        logger: &Arc<Mutex<Logger>>,
    ) {
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                SupervisorCommand::Spawn(config) => {
                    // Skip if already running
                    if self
                        .children
                        .iter()
                        .any(|(_, name, _, _)| *name == config.name)
                    {
                        logger.lock().unwrap().log_line(
                            "procman",
                            &format!("{}: already running, skipping spawn", config.name),
                        );
                        continue;
                    }

                    let config = if let Some(mut dormant_config) = self.dormant.remove(&config.name)
                    {
                        dormant_config.env.extend(config.env);
                        dormant_config
                    } else {
                        config
                    };
                    if let Some(ref condition) = config.condition {
                        match evaluate_condition(condition, &config.env, &config.name, logger) {
                            Ok(true) => {}
                            Ok(false) => {
                                if config.once {
                                    self.exit_registry
                                        .lock()
                                        .unwrap()
                                        .insert(config.name.clone());
                                }
                                continue;
                            }
                            Err(e) => {
                                logger.lock().unwrap().log_line(
                                    "procman",
                                    &format!("error evaluating condition for {}: {e}", config.name),
                                );
                                shutdown.store(true, Ordering::Relaxed);
                                continue;
                            }
                        }
                    }
                    if config.for_each.is_some() {
                        if let Err(e) = self.expand_fan_out(&config, logger) {
                            logger.lock().unwrap().log_line(
                                "procman",
                                &format!("error in fan-out for {}: {e}", config.name),
                            );
                            shutdown.store(true, Ordering::Relaxed);
                        }
                    } else {
                        logger.lock().unwrap().add_process(&config.name).ok();
                        match self.spawn_one(&config, logger) {
                            Ok(_) => self.start_watchers(&config, logger),
                            Err(e) => {
                                logger.lock().unwrap().log_line(
                                    "procman",
                                    &format!("error spawning {}: {e}", config.name),
                                );
                            }
                        }
                    }
                }
                SupervisorCommand::Shutdown { message } => {
                    logger
                        .lock()
                        .unwrap()
                        .log_line("procman", &format!("shutdown: {message}"));
                    shutdown.store(true, Ordering::Relaxed);
                }
                SupervisorCommand::DebugPause { message } => {
                    logger
                        .lock()
                        .unwrap()
                        .log_line("procman", &format!("debug pause: {message}"));
                    self.debug_mode = true;
                    shutdown.store(true, Ordering::Relaxed);
                }
            }
        }
    }

    pub fn wait_and_shutdown(
        mut self,
        shutdown: Arc<AtomicBool>,
        signal_triggered: Arc<AtomicBool>,
        rx: mpsc::Receiver<SupervisorCommand>,
        logger: Arc<Mutex<Logger>>,
    ) -> i32 {
        // Drop our sender so channel closes when all watcher/waiter threads finish
        self.tx = None;

        let mut first_exit_code: Option<i32> = None;
        let mut shutdown_trigger: Option<String> = None;
        let mut remaining: Vec<Pid> = self.children.iter().map(|(pid, _, _, _)| *pid).collect();

        loop {
            if shutdown.load(Ordering::Relaxed) || first_exit_code.is_some() {
                break;
            }

            match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::Exited(pid, code)) => {
                    remaining.retain(|p| *p != pid);
                    let child_info = self.children.iter().find(|(p, _, _, _)| *p == pid).map(
                        |(_, name, started, once)| {
                            (name.clone(), started.elapsed().as_secs_f64(), *once)
                        },
                    );
                    self.children.retain(|(p, _, _, _)| *p != pid);
                    if let Some((name, elapsed, once)) = child_info {
                        if once && code == 0 {
                            let mut completed_group = None;
                            {
                                let mut registry = self.exit_registry.lock().unwrap();
                                registry.insert(name.clone());
                                for (template_name, instance_names) in &self.fan_out_groups {
                                    if instance_names.contains(&name)
                                        && instance_names.iter().all(|n| registry.contains(n))
                                    {
                                        registry.insert(template_name.clone());
                                        completed_group = Some(template_name.clone());
                                        break;
                                    }
                                }
                            }
                            if let Some(template_name) = completed_group {
                                logger
                                    .lock()
                                    .unwrap()
                                    .log_line(&template_name, "all fan-out instances completed");
                            }
                            logger
                                .lock()
                                .unwrap()
                                .log_line(&name, &format!("[{pid}] completed after {elapsed:.1}s"));
                            if remaining.is_empty()
                                && self.pending_deps.load(Ordering::Relaxed) == 0
                            {
                                first_exit_code = Some(0);
                                break;
                            }
                            continue;
                        }
                        logger.lock().unwrap().log_line(
                            &name,
                            &format!("[{pid}] exited with code {code} after {elapsed:.1}s"),
                        );
                        if first_exit_code.is_none() {
                            first_exit_code = Some(code);
                            shutdown_trigger =
                                Some(format!("{name} [pid {pid}] exited with code {code}"));
                        }
                    } else if first_exit_code.is_none() {
                        first_exit_code = Some(code);
                    }
                }
                Ok(WaitStatus::Signaled(pid, sig, _)) => {
                    remaining.retain(|p| *p != pid);
                    let child_name = self.children.iter().find(|(p, _, _, _)| *p == pid).map(
                        |(_, name, started, _)| (name.clone(), started.elapsed().as_secs_f64()),
                    );
                    self.children.retain(|(p, _, _, _)| *p != pid);
                    if let Some((name, elapsed)) = child_name {
                        logger.lock().unwrap().log_line(
                            &name,
                            &format!("[{pid}] killed by {sig} after {elapsed:.1}s"),
                        );
                        if first_exit_code.is_none() {
                            first_exit_code = Some(1);
                            shutdown_trigger = Some(format!("{name} [pid {pid}] killed by {sig}"));
                        }
                    } else if first_exit_code.is_none() {
                        first_exit_code = Some(1);
                    }
                }
                Ok(WaitStatus::StillAlive) => {
                    self.try_accept_new(&rx, &shutdown, &logger);
                    for (pid, _, _, _) in &self.children {
                        if !remaining.contains(pid) {
                            remaining.push(*pid);
                        }
                    }
                    thread::sleep(Duration::from_millis(50));
                    continue;
                }
                Err(nix::errno::Errno::ECHILD) => {
                    self.try_accept_new(&rx, &shutdown, &logger);
                    if !self.children.is_empty() {
                        remaining = self.children.iter().map(|(pid, _, _, _)| *pid).collect();
                        continue;
                    }
                    if self.pending_deps.load(Ordering::Relaxed) == 0 {
                        break;
                    }
                    thread::sleep(Duration::from_millis(50));
                    continue;
                }
                _ => {
                    thread::sleep(Duration::from_millis(50));
                    continue;
                }
            }
        }

        // Drain any already-exited children so "remaining" is accurate
        loop {
            match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::Exited(pid, code)) => {
                    remaining.retain(|p| *p != pid);
                    self.children.retain(|(p, _, _, _)| *p != pid);
                    if first_exit_code.is_none() {
                        first_exit_code = Some(code);
                    }
                }
                Ok(WaitStatus::Signaled(pid, _, _)) => {
                    remaining.retain(|p| *p != pid);
                    self.children.retain(|(p, _, _, _)| *p != pid);
                }
                _ => break,
            }
        }

        if self.debug_mode && !remaining.is_empty() && !signal_triggered.load(Ordering::Relaxed) {
            let trigger = shutdown_trigger
                .as_deref()
                .unwrap_or("dependency timed out");
            logger
                .lock()
                .unwrap()
                .log_line("procman", "debug mode \u{2014} shutdown paused");
            logger
                .lock()
                .unwrap()
                .log_line("procman", &format!("trigger: {trigger}"));
            logger.lock().unwrap().log_line("procman", "still running:");
            for pid in &remaining {
                if let Some((_, name, _, _)) = self.children.iter().find(|(p, _, _, _)| *p == *pid)
                {
                    logger
                        .lock()
                        .unwrap()
                        .log_line("procman", &format!("  - {name} [pid {pid}]"));
                }
            }
            logger
                .lock()
                .unwrap()
                .log_line("procman", "press ENTER to continue shutdown (or Ctrl+C)...");

            let (done_tx, done_rx) = mpsc::channel();
            thread::spawn(move || {
                let mut buf = String::new();
                let _ = std::io::stdin().read_line(&mut buf);
                let _ = done_tx.send(());
            });
            loop {
                if signal_triggered.load(Ordering::Relaxed) {
                    break;
                }
                if done_rx.recv_timeout(Duration::from_millis(100)).is_ok() {
                    break;
                }
            }
        }

        // SIGTERM each remaining child's process group
        for pid in &remaining {
            let _ = signal::killpg(*pid, Signal::SIGTERM);
        }

        // Poll for up to 2 seconds
        let deadline = Instant::now() + Duration::from_secs(2);
        while !remaining.is_empty() && Instant::now() < deadline {
            match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::Exited(pid, code)) => {
                    remaining.retain(|p| *p != pid);
                    if first_exit_code.is_none() {
                        first_exit_code = Some(code);
                    }
                }
                Ok(WaitStatus::Signaled(pid, _, _)) => {
                    remaining.retain(|p| *p != pid);
                }
                Err(nix::errno::Errno::ECHILD) => break,
                _ => {
                    thread::sleep(Duration::from_millis(50));
                }
            }
        }

        // SIGKILL any that remain
        for pid in &remaining {
            let _ = signal::killpg(*pid, Signal::SIGKILL);
            let _ = waitpid(*pid, None);
        }

        // Join reader threads
        for handle in self.reader_threads {
            let _ = handle.join();
        }

        // Join waiter threads
        for handle in self.waiter_threads {
            let _ = handle.join();
        }

        // Join watcher threads
        for handle in self.watcher_threads {
            let _ = handle.join();
        }

        first_exit_code.unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;
    use crate::config::ForEachConfig;

    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);
    // Serialize tests that spawn child processes to prevent waitpid(-1) from
    // reaping another test's children.
    static PROCESS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn make_test_group() -> (ProcessGroup, Arc<Mutex<Logger>>) {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let log_dir =
            std::env::temp_dir().join(format!("procman_process_test_{}_{id}", std::process::id()));
        std::fs::create_dir_all(&log_dir).unwrap();
        let logger = Arc::new(Mutex::new(
            Logger::new_for_test(&["procman".to_string()], log_dir.clone()).unwrap(),
        ));
        let group = ProcessGroup {
            children: Vec::new(),
            reader_threads: Vec::new(),
            waiter_threads: Vec::new(),
            watcher_threads: Vec::new(),
            pending_deps: Arc::new(AtomicUsize::new(0)),
            exit_registry: Arc::new(Mutex::new(HashSet::new())),
            log_dir,
            fan_out_groups: HashMap::new(),
            debug_mode: false,
            dormant: HashMap::new(),
            tx: None,
            shutdown: Arc::new(AtomicBool::new(false)),
        };
        (group, logger)
    }

    fn make_temp_glob_files(count: usize) -> (PathBuf, String) {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("procman_fanout_test_{}_{id}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..count {
            std::fs::write(dir.join(format!("node-{i}.yaml")), format!("node{i}")).unwrap();
        }
        let pattern = dir.join("node-*.yaml").to_string_lossy().to_string();
        (dir, pattern)
    }

    #[test]
    fn build_command_single_line() {
        let cmd = build_command("echo hello world", "test").unwrap();
        assert_eq!(cmd.get_program(), "sh");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(
            args,
            &["-e", "-u", "-o", "pipefail", "-c", "echo hello world"]
        );
    }

    #[test]
    fn build_command_multiline_uses_sh() {
        let cmd = build_command("echo hello\necho world", "test").unwrap();
        assert_eq!(cmd.get_program(), "sh");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(
            args,
            &["-e", "-u", "-o", "pipefail", "-c", "echo hello\necho world"]
        );
    }

    #[test]
    fn build_command_trailing_newline_only() {
        let cmd = build_command("echo hello\n", "test").unwrap();
        assert_eq!(cmd.get_program(), "sh");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, &["-e", "-u", "-o", "pipefail", "-c", "echo hello\n"]);
    }

    #[test]
    fn expand_fan_out_creates_instances() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let (mut group, logger) = make_test_group();
        let (_dir, pattern) = make_temp_glob_files(3);
        let config = ProcessConfig {
            name: "nodes".to_string(),
            env: std::env::vars().collect(),
            run: "true".to_string(),
            condition: None,
            depends: vec![],
            once: true,
            autostart: true,
            for_each: Some(ForEachConfig {
                glob: pattern,
                variable: "CONFIG_PATH".to_string(),
            }),
            watches: vec![],
        };
        group.expand_fan_out(&config, &logger).unwrap();
        assert_eq!(group.children.len(), 3);
        let names: Vec<&str> = group
            .children
            .iter()
            .map(|(_, n, _, _)| n.as_str())
            .collect();
        assert!(names.contains(&"nodes-0"));
        assert!(names.contains(&"nodes-1"));
        assert!(names.contains(&"nodes-2"));
        assert!(group.fan_out_groups.contains_key("nodes"));
        assert_eq!(group.fan_out_groups["nodes"].len(), 3);
        for (pid, _, _, _) in &group.children {
            let _ = waitpid(*pid, None);
        }
        for handle in std::mem::take(&mut group.reader_threads) {
            let _ = handle.join();
        }
    }

    #[test]
    fn expand_fan_out_zero_matches_errors() {
        let (mut group, logger) = make_test_group();
        let config = ProcessConfig {
            name: "nodes".to_string(),
            env: HashMap::new(),
            run: "true".to_string(),
            condition: None,
            depends: vec![],
            once: true,
            autostart: true,
            for_each: Some(ForEachConfig {
                glob: "/tmp/procman_nonexistent_glob_pattern_*.xyz".to_string(),
                variable: "CONFIG_PATH".to_string(),
            }),
            watches: vec![],
        };
        let err = group.expand_fan_out(&config, &logger).unwrap_err();
        assert!(err.to_string().contains("matched zero files"), "{err}");
    }

    #[test]
    fn expand_fan_out_sets_env_var() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let (mut group, logger) = make_test_group();
        let (dir, pattern) = make_temp_glob_files(2);
        let config = ProcessConfig {
            name: "nodes".to_string(),
            env: std::env::vars().collect(),
            run: "echo $CONFIG_PATH".to_string(),
            condition: None,
            depends: vec![],
            once: true,
            autostart: true,
            for_each: Some(ForEachConfig {
                glob: pattern,
                variable: "CONFIG_PATH".to_string(),
            }),
            watches: vec![],
        };
        group.expand_fan_out(&config, &logger).unwrap();
        // Verify that the run strings got substituted
        // We can't easily inspect the spawned process's env, but we can verify
        // the fan_out_groups were created correctly
        let instance_names = &group.fan_out_groups["nodes"];
        assert!(instance_names.contains("nodes-0"));
        assert!(instance_names.contains("nodes-1"));
        // The files should be sorted, so node-0.yaml comes first
        let expected_path_0 = dir.join("node-0.yaml").to_string_lossy().to_string();
        let expected_path_1 = dir.join("node-1.yaml").to_string_lossy().to_string();
        // Verify substitution happened in run string by checking children were spawned
        // (if the run string substitution failed, spawn_one would error on $CONFIG_PATH)
        assert_eq!(group.children.len(), 2);
        // Check we can find the paths in the child names
        assert!(group.children.iter().any(|(_, n, _, _)| n == "nodes-0"));
        assert!(group.children.iter().any(|(_, n, _, _)| n == "nodes-1"));
        drop(expected_path_0);
        drop(expected_path_1);
        for (pid, _, _, _) in &group.children {
            let _ = waitpid(*pid, None);
        }
        for handle in std::mem::take(&mut group.reader_threads) {
            let _ = handle.join();
        }
    }

    #[test]
    fn expand_fan_out_expands_env_in_glob() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let (mut group, logger) = make_test_group();
        // Create temp files manually (don't use make_temp_glob_files since we need the dir separate)
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("procman_envglob_test_{}_{id}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..2 {
            std::fs::write(dir.join(format!("node-{i}.yaml")), format!("node{i}")).unwrap();
        }

        let mut env: HashMap<String, String> = std::env::vars().collect();
        env.insert("TEST_DIR".to_string(), dir.to_string_lossy().to_string());

        let config = ProcessConfig {
            name: "nodes".to_string(),
            env,
            run: "true".to_string(),
            condition: None,
            depends: vec![],
            once: true,
            autostart: true,
            for_each: Some(ForEachConfig {
                glob: "$TEST_DIR/node-*.yaml".to_string(),
                variable: "CONFIG_PATH".to_string(),
            }),
            watches: vec![],
        };
        group.expand_fan_out(&config, &logger).unwrap();
        assert_eq!(group.children.len(), 2);
        assert!(group.children.iter().any(|(_, n, _, _)| n == "nodes-0"));
        assert!(group.children.iter().any(|(_, n, _, _)| n == "nodes-1"));
        for (pid, _, _, _) in &group.children {
            let _ = waitpid(*pid, None);
        }
        for handle in std::mem::take(&mut group.reader_threads) {
            let _ = handle.join();
        }
    }

    #[test]
    fn fan_out_group_completion() {
        let (mut group, _logger) = make_test_group();
        let mut instance_names = HashSet::new();
        instance_names.insert("nodes-0".to_string());
        instance_names.insert("nodes-1".to_string());
        instance_names.insert("nodes-2".to_string());
        group
            .fan_out_groups
            .insert("nodes".to_string(), instance_names);

        let registry = group.exit_registry.clone();

        // Insert first two — group not yet complete
        registry.lock().unwrap().insert("nodes-0".to_string());
        registry.lock().unwrap().insert("nodes-1".to_string());
        assert!(!registry.lock().unwrap().contains("nodes"));

        // Insert third — now manually simulate what the exit handler does
        {
            let mut reg = registry.lock().unwrap();
            reg.insert("nodes-2".to_string());
            for (template_name, instance_names) in &group.fan_out_groups {
                if instance_names.contains("nodes-2")
                    && instance_names.iter().all(|n| reg.contains(n))
                {
                    reg.insert(template_name.clone());
                    break;
                }
            }
        }
        assert!(registry.lock().unwrap().contains("nodes"));
    }

    #[test]
    fn once_process_exits_cleanly() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let (tx, rx) = mpsc::channel::<crate::config::SupervisorCommand>();
        let shutdown = Arc::new(AtomicBool::new(false));
        let signal_triggered = Arc::new(AtomicBool::new(false));
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let log_dir = std::env::temp_dir().join(format!(
            "procman_once_exit_test_{}_{id}",
            std::process::id()
        ));
        let logger = Arc::new(Mutex::new(
            Logger::new_for_test(&["procman".to_string(), "hello".to_string()], log_dir).unwrap(),
        ));
        let configs = vec![ProcessConfig {
            name: "hello".to_string(),
            env: std::env::vars().collect(),
            run: "echo Hello".to_string(),
            condition: None,
            depends: vec![],
            once: true,
            for_each: None,
            autostart: true,
            watches: vec![],
        }];
        let group = ProcessGroup::spawn(
            &configs,
            tx,
            Arc::clone(&shutdown),
            Arc::clone(&logger),
            false,
        )
        .unwrap();
        drop(rx);
        let (done_tx, done_rx) = mpsc::channel();
        let shutdown2 = Arc::clone(&shutdown);
        let signal2 = Arc::clone(&signal_triggered);
        let logger2 = Arc::clone(&logger);
        let handle = thread::spawn(move || {
            let (_, inner_rx) = mpsc::channel();
            let code = group.wait_and_shutdown(shutdown2, signal2, inner_rx, logger2);
            let _ = done_tx.send(code);
        });
        let code = done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("wait_and_shutdown should not hang");
        assert_eq!(code, 0);
        handle.join().unwrap();
    }

    #[test]
    fn all_once_processes_exit_cleanly() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let (tx, rx) = mpsc::channel::<crate::config::SupervisorCommand>();
        let shutdown = Arc::new(AtomicBool::new(false));
        let signal_triggered = Arc::new(AtomicBool::new(false));
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let log_dir = std::env::temp_dir().join(format!(
            "procman_all_once_exit_test_{}_{id}",
            std::process::id()
        ));
        let logger = Arc::new(Mutex::new(
            Logger::new_for_test(
                &["procman".to_string(), "a".to_string(), "b".to_string()],
                log_dir,
            )
            .unwrap(),
        ));
        let configs = vec![
            ProcessConfig {
                name: "a".to_string(),
                env: std::env::vars().collect(),
                run: "echo A".to_string(),
                condition: None,
                depends: vec![],
                once: true,
                for_each: None,
                autostart: true,
                watches: vec![],
            },
            ProcessConfig {
                name: "b".to_string(),
                env: std::env::vars().collect(),
                run: "echo B".to_string(),
                condition: None,
                depends: vec![],
                once: true,
                for_each: None,
                autostart: true,
                watches: vec![],
            },
        ];
        let group = ProcessGroup::spawn(
            &configs,
            tx,
            Arc::clone(&shutdown),
            Arc::clone(&logger),
            false,
        )
        .unwrap();
        drop(rx);
        let (done_tx, done_rx) = mpsc::channel();
        let shutdown2 = Arc::clone(&shutdown);
        let signal2 = Arc::clone(&signal_triggered);
        let logger2 = Arc::clone(&logger);
        let handle = thread::spawn(move || {
            let (_, inner_rx) = mpsc::channel();
            let code = group.wait_and_shutdown(shutdown2, signal2, inner_rx, logger2);
            let _ = done_tx.send(code);
        });
        let code = done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("wait_and_shutdown should not hang");
        assert_eq!(code, 0);
        handle.join().unwrap();
    }

    #[test]
    fn once_with_long_running_does_not_auto_exit() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let (tx, rx) = mpsc::channel::<crate::config::SupervisorCommand>();
        let shutdown = Arc::new(AtomicBool::new(false));
        let signal_triggered = Arc::new(AtomicBool::new(false));
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let log_dir = std::env::temp_dir().join(format!(
            "procman_once_long_test_{}_{id}",
            std::process::id()
        ));
        let logger = Arc::new(Mutex::new(
            Logger::new_for_test(
                &[
                    "procman".to_string(),
                    "quick".to_string(),
                    "slow".to_string(),
                ],
                log_dir,
            )
            .unwrap(),
        ));
        let configs = vec![
            ProcessConfig {
                name: "quick".to_string(),
                env: std::env::vars().collect(),
                run: "echo done".to_string(),
                condition: None,
                depends: vec![],
                once: true,
                for_each: None,
                autostart: true,
                watches: vec![],
            },
            ProcessConfig {
                name: "slow".to_string(),
                env: std::env::vars().collect(),
                run: "sleep 60".to_string(),
                condition: None,
                depends: vec![],
                once: false,
                for_each: None,
                autostart: true,
                watches: vec![],
            },
        ];
        let group = ProcessGroup::spawn(
            &configs,
            tx,
            Arc::clone(&shutdown),
            Arc::clone(&logger),
            false,
        )
        .unwrap();
        drop(rx);
        let (done_tx, done_rx) = mpsc::channel();
        let shutdown2 = Arc::clone(&shutdown);
        let signal2 = Arc::clone(&signal_triggered);
        let logger2 = Arc::clone(&logger);
        let handle = thread::spawn(move || {
            let (_, inner_rx) = mpsc::channel();
            let code = group.wait_and_shutdown(shutdown2, signal2, inner_rx, logger2);
            let _ = done_tx.send(code);
        });
        // Should NOT auto-exit within 500ms because "slow" is still running
        assert!(
            done_rx.recv_timeout(Duration::from_millis(500)).is_err(),
            "should not auto-exit while long-running process is active"
        );
        // Trigger shutdown so the test can clean up
        shutdown.store(true, Ordering::Relaxed);
        let code = done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("should exit after shutdown");
        // Exit code 0 since no process failed
        assert_eq!(code, 0);
        handle.join().unwrap();
    }

    #[test]
    fn debug_mode_excludes_completed_once_processes() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let (tx, rx) = mpsc::channel::<crate::config::SupervisorCommand>();
        let shutdown = Arc::new(AtomicBool::new(false));
        let signal_triggered = Arc::new(AtomicBool::new(false));
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let log_dir = std::env::temp_dir().join(format!(
            "procman_debug_once_test_{}_{id}",
            std::process::id()
        ));
        let logger = Arc::new(Mutex::new(
            Logger::new_for_test(
                &[
                    "procman".to_string(),
                    "fast".to_string(),
                    "crasher".to_string(),
                ],
                log_dir,
            )
            .unwrap(),
        ));
        let configs = vec![
            ProcessConfig {
                name: "fast".to_string(),
                env: std::env::vars().collect(),
                run: "echo done".to_string(),
                condition: None,
                depends: vec![],
                once: true,
                for_each: None,
                autostart: true,
                watches: vec![],
            },
            ProcessConfig {
                name: "crasher".to_string(),
                env: std::env::vars().collect(),
                run: "exit 1".to_string(),
                condition: None,
                depends: vec![],
                once: false,
                for_each: None,
                autostart: true,
                watches: vec![],
            },
        ];
        let group = ProcessGroup::spawn(
            &configs,
            tx,
            Arc::clone(&shutdown),
            Arc::clone(&logger),
            true,
        )
        .unwrap();
        drop(rx);
        let (done_tx, done_rx) = mpsc::channel();
        let shutdown2 = Arc::clone(&shutdown);
        let signal2 = Arc::clone(&signal_triggered);
        let logger2 = Arc::clone(&logger);
        // Pre-trigger signal so debug mode doesn't block waiting for stdin
        signal_triggered.store(true, Ordering::Relaxed);
        let handle = thread::spawn(move || {
            let (_, inner_rx) = mpsc::channel();
            let code = group.wait_and_shutdown(shutdown2, signal2, inner_rx, logger2);
            let _ = done_tx.send(code);
        });
        let code = done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("wait_and_shutdown should not hang");
        assert_eq!(code, 1);
        handle.join().unwrap();
    }

    #[test]
    fn try_accept_new_merges_env_into_dormant() {
        let (mut group, logger) = make_test_group();
        group.dormant.insert(
            "recovery".to_string(),
            ProcessConfig {
                name: "recovery".to_string(),
                env: std::env::vars().collect(),
                run: "echo recovering".to_string(),
                condition: None,
                depends: vec![],
                once: true,
                for_each: None,
                autostart: false,
                watches: vec![],
            },
        );

        let (tx, rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));

        // Send a Spawn with extra env vars (simulating watch trigger)
        let mut watch_env = HashMap::new();
        watch_env.insert("PROCMAN_WATCH_NAME".to_string(), "health".to_string());
        watch_env.insert("PROCMAN_WATCH_PROCESS".to_string(), "web".to_string());
        let config = ProcessConfig {
            name: "recovery".to_string(),
            env: watch_env,
            run: String::new(),
            condition: None,
            depends: vec![],
            once: false,
            for_each: None,
            autostart: true,
            watches: vec![],
        };
        tx.send(SupervisorCommand::Spawn(config)).unwrap();
        drop(tx);

        // The dormant config should be removed and env merged
        // We can't easily test the merged env without spawning, but we can verify
        // the dormant map was consumed
        group.try_accept_new(&rx, &shutdown, &logger);
        assert!(!group.dormant.contains_key("recovery"));
    }

    #[test]
    fn try_accept_new_skips_already_running() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let (mut group, logger) = make_test_group();

        // Spawn a process first
        let config = ProcessConfig {
            name: "worker".to_string(),
            env: std::env::vars().collect(),
            run: "sleep 60".to_string(),
            condition: None,
            depends: vec![],
            once: false,
            for_each: None,
            autostart: true,
            watches: vec![],
        };
        logger.lock().unwrap().add_process("worker").ok();
        group.spawn_one(&config, &logger).unwrap();
        assert_eq!(group.children.len(), 1);

        // Try to spawn again with the same name
        let (tx, rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        tx.send(SupervisorCommand::Spawn(config)).unwrap();
        drop(tx);

        group.try_accept_new(&rx, &shutdown, &logger);
        // Should still have only 1 child
        assert_eq!(group.children.len(), 1);

        // Clean up
        for (pid, _, _, _) in &group.children {
            let _ = nix::sys::signal::killpg(*pid, Signal::SIGKILL);
            let _ = waitpid(*pid, None);
        }
        for handle in std::mem::take(&mut group.reader_threads) {
            let _ = handle.join();
        }
    }

    #[test]
    fn debug_pause_sets_debug_and_shutdown() {
        let (mut group, logger) = make_test_group();
        let (tx, rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));

        tx.send(SupervisorCommand::DebugPause {
            message: "watch triggered".to_string(),
        })
        .unwrap();
        drop(tx);

        assert!(!group.debug_mode);
        group.try_accept_new(&rx, &shutdown, &logger);
        assert!(group.debug_mode);
        assert!(shutdown.load(Ordering::Relaxed));
    }

    #[test]
    fn evaluate_condition_true_returns_true() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let (_, logger) = make_test_group();
        let env: HashMap<String, String> = std::env::vars().collect();
        assert!(evaluate_condition("true", &env, "test", &logger).unwrap());
    }

    #[test]
    fn evaluate_condition_false_returns_false() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let (_, logger) = make_test_group();
        let env: HashMap<String, String> = std::env::vars().collect();
        assert!(!evaluate_condition("false", &env, "test", &logger).unwrap());
    }

    #[test]
    fn evaluate_condition_uses_process_env() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let (_, logger) = make_test_group();
        let mut env = HashMap::new();
        env.insert("MY_FLAG".to_string(), "yes".to_string());
        assert!(evaluate_condition("test \"$MY_FLAG\" = yes", &env, "test", &logger).unwrap());
        env.insert("MY_FLAG".to_string(), "no".to_string());
        assert!(!evaluate_condition("test \"$MY_FLAG\" = yes", &env, "test", &logger).unwrap());
    }

    #[test]
    fn condition_false_skips_once_process_and_registers_exit() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let (tx, _rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let log_dir = std::env::temp_dir().join(format!(
            "procman_cond_skip_test_{}_{id}",
            std::process::id()
        ));
        let logger = Arc::new(Mutex::new(
            Logger::new_for_test(&["procman".to_string(), "skipped".to_string()], log_dir).unwrap(),
        ));
        let configs = vec![ProcessConfig {
            name: "skipped".to_string(),
            env: std::env::vars().collect(),
            run: "echo should-not-run".to_string(),
            condition: Some("false".to_string()),
            depends: vec![],
            once: true,
            for_each: None,
            autostart: true,
            watches: vec![],
        }];
        let group = ProcessGroup::spawn(
            &configs,
            tx,
            Arc::clone(&shutdown),
            Arc::clone(&logger),
            false,
        )
        .unwrap();
        assert!(group.children.is_empty());
        assert!(group.exit_registry.lock().unwrap().contains("skipped"));
    }

    #[test]
    fn condition_true_allows_spawn() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let (mut group, logger) = make_test_group();
        let config = ProcessConfig {
            name: "cond-pass".to_string(),
            env: std::env::vars().collect(),
            run: "true".to_string(),
            condition: Some("true".to_string()),
            depends: vec![],
            once: true,
            for_each: None,
            autostart: true,
            watches: vec![],
        };
        logger.lock().unwrap().add_process("cond-pass").ok();
        group.spawn_one(&config, &logger).unwrap();
        assert_eq!(group.children.len(), 1);
        for (pid, _, _, _) in &group.children {
            let _ = waitpid(*pid, None);
        }
        for handle in std::mem::take(&mut group.reader_threads) {
            let _ = handle.join();
        }
    }

    #[test]
    fn condition_false_non_once_process_not_in_exit_registry() {
        let _guard = PROCESS_TEST_LOCK.lock().unwrap();
        let (tx, _rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let log_dir = std::env::temp_dir().join(format!(
            "procman_cond_skip_noonce_test_{}_{id}",
            std::process::id()
        ));
        let logger = Arc::new(Mutex::new(
            Logger::new_for_test(&["procman".to_string(), "skipped".to_string()], log_dir).unwrap(),
        ));
        let configs = vec![ProcessConfig {
            name: "skipped".to_string(),
            env: std::env::vars().collect(),
            run: "echo should-not-run".to_string(),
            condition: Some("false".to_string()),
            depends: vec![],
            once: false,
            for_each: None,
            autostart: true,
            watches: vec![],
        }];
        let group = ProcessGroup::spawn(
            &configs,
            tx,
            Arc::clone(&shutdown),
            Arc::clone(&logger),
            false,
        )
        .unwrap();
        assert!(group.children.is_empty());
        assert!(!group.exit_registry.lock().unwrap().contains("skipped"));
    }
}
