use std::{
    io::BufRead,
    os::unix::process::CommandExt,
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

use crate::{config::ProcessConfig, dependency, log::Logger};

pub struct ProcessGroup {
    pgid: Option<Pid>,
    children: Vec<(Pid, String)>,
    reader_threads: Vec<thread::JoinHandle<()>>,
    waiter_threads: Vec<thread::JoinHandle<()>>,
    pending_deps: Arc<AtomicUsize>,
}

impl ProcessGroup {
    fn spawn_one(&mut self, cmd: &ProcessConfig, logger: &Arc<Mutex<Logger>>) -> Result<Pid> {
        let mut child_cmd = std::process::Command::new(&cmd.program);
        child_cmd.args(&cmd.args);
        child_cmd.env_clear();
        child_cmd.envs(&cmd.env);
        child_cmd.stdout(Stdio::piped());
        child_cmd.stderr(Stdio::piped());

        let pgid_val = self.pgid;
        unsafe {
            child_cmd.pre_exec(move || {
                // Merge stderr into stdout
                let r = libc::dup2(1, 2);
                if r == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                // Set process group
                match pgid_val {
                    Some(pg) => {
                        let r = libc::setpgid(0, pg.as_raw());
                        if r == -1 {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
                    None => {
                        let r = libc::setpgid(0, 0);
                        if r == -1 {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
                }
                Ok(())
            });
        }

        let mut child = child_cmd
            .spawn()
            .with_context(|| format!("spawning {}", cmd.name))?;

        let pid = Pid::from_raw(child.id() as i32);
        if self.pgid.is_none() {
            self.pgid = Some(pid);
        }

        let name = cmd.name.clone();
        self.children.push((pid, name.clone()));

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

    pub fn spawn(
        commands: &[ProcessConfig],
        tx: mpsc::Sender<ProcessConfig>,
        shutdown: Arc<AtomicBool>,
        logger: Arc<Mutex<Logger>>,
    ) -> Result<Self> {
        let mut group = Self {
            pgid: None,
            children: Vec::new(),
            reader_threads: Vec::new(),
            waiter_threads: Vec::new(),
            pending_deps: Arc::new(AtomicUsize::new(0)),
        };

        for cmd in commands {
            if cmd.depends.is_empty() {
                group.spawn_one(cmd, &logger)?;
            } else {
                logger.lock().unwrap().add_process(&cmd.name).ok();
                group.pending_deps.fetch_add(1, Ordering::Relaxed);
                group.waiter_threads.push(dependency::spawn_waiter(
                    cmd.clone(),
                    tx.clone(),
                    Arc::clone(&shutdown),
                    Arc::clone(&logger),
                    Arc::clone(&group.pending_deps),
                ));
            }
        }

        drop(tx);
        Ok(group)
    }

    fn try_accept_new(&mut self, rx: &mpsc::Receiver<ProcessConfig>, logger: &Arc<Mutex<Logger>>) {
        while let Ok(cmd) = rx.try_recv() {
            logger.lock().unwrap().add_process(&cmd.name).ok();
            if let Err(e) = self.spawn_one(&cmd, logger) {
                eprintln!("error spawning {}: {e}", cmd.name);
            }
        }
    }

    pub fn wait_and_shutdown(
        mut self,
        shutdown: Arc<AtomicBool>,
        rx: mpsc::Receiver<ProcessConfig>,
        logger: Arc<Mutex<Logger>>,
    ) -> i32 {
        let mut first_exit_code: Option<i32> = None;
        let mut remaining: Vec<Pid> = self.children.iter().map(|(pid, _)| *pid).collect();

        loop {
            if shutdown.load(Ordering::Relaxed) || first_exit_code.is_some() {
                break;
            }

            match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::Exited(pid, code)) => {
                    remaining.retain(|p| *p != pid);
                    if first_exit_code.is_none() {
                        first_exit_code = Some(code);
                    }
                }
                Ok(WaitStatus::Signaled(pid, _sig, _)) => {
                    remaining.retain(|p| *p != pid);
                    if first_exit_code.is_none() {
                        first_exit_code = Some(1);
                    }
                }
                Ok(WaitStatus::StillAlive) => {
                    self.try_accept_new(&rx, &logger);
                    for (pid, _) in &self.children {
                        if !remaining.contains(pid) {
                            remaining.push(*pid);
                        }
                    }
                    thread::sleep(Duration::from_millis(50));
                    continue;
                }
                Err(nix::errno::Errno::ECHILD) => {
                    self.try_accept_new(&rx, &logger);
                    if !self.children.is_empty() {
                        remaining = self.children.iter().map(|(pid, _)| *pid).collect();
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

        // SIGTERM the group
        if let Some(pgid) = self.pgid {
            let _ = signal::killpg(pgid, Signal::SIGTERM);

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

            // SIGKILL if any remain
            if !remaining.is_empty() {
                let _ = signal::killpg(pgid, Signal::SIGKILL);
                for pid in &remaining {
                    let _ = waitpid(*pid, None);
                }
            }
        }

        // Join reader threads
        for handle in self.reader_threads {
            let _ = handle.join();
        }

        // Join waiter threads
        for handle in self.waiter_threads {
            let _ = handle.join();
        }

        first_exit_code.unwrap_or(1)
    }
}
