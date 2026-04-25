use std::{
    collections::HashMap,
    sync::{
        Arc,
        Condvar,
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use crate::log::Logger;

/// Outcome of an [`OutputMatcher`] firing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchOutcome {
    /// The pattern was observed on the upstream's output stream.
    Matched,
    /// The upstream exited (EOF on its output) without ever emitting the pattern.
    UpstreamExited,
}

/// One subscription to an upstream's output stream.
///
/// Created at lower time, registered into [`OutputMatchRegistry`] before any
/// upstream is spawned, then read by both:
///   - the upstream's reader thread (which calls [`Matcher::fire`]), and
///   - the downstream's waiter thread (which calls [`wait_for_output_match`]).
#[derive(Debug)]
pub struct Matcher {
    /// Literal substring pattern. Match is case-sensitive, per-line, against
    /// the ANSI-stripped form of each captured output line.
    pub pattern: String,
    /// Set once the matcher has resolved (matched or upstream-exited).
    pub fired: AtomicBool,
    /// The outcome paired with a condvar so waiters can sleep.
    pub state: Mutex<Option<MatchOutcome>>,
    pub cv: Condvar,
    /// Human-readable label for log lines, e.g. `output_matches @migrate "ready"`.
    pub label: String,
}

impl Matcher {
    pub fn new(pattern: String, label: String) -> Arc<Self> {
        Arc::new(Self {
            pattern,
            fired: AtomicBool::new(false),
            state: Mutex::new(None),
            cv: Condvar::new(),
            label,
        })
    }

    /// Resolve this matcher with `outcome`. Idempotent — the first call wins;
    /// subsequent calls are no-ops. Logs the resolution against `upstream_name`.
    pub fn fire(&self, outcome: MatchOutcome, logger: &Mutex<Logger>, upstream_name: &str) {
        if self.fired.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut guard = self.state.lock().unwrap();
        *guard = Some(outcome);
        self.cv.notify_all();
        drop(guard);
        let msg = match outcome {
            MatchOutcome::Matched => {
                format!("matched downstream waiter: {}", self.label)
            }
            MatchOutcome::UpstreamExited => {
                format!(
                    "upstream exited without emitting pattern for: {}",
                    self.label
                )
            }
        };
        logger.lock().unwrap().log_line(upstream_name, &msg);
    }
}

/// Pre-spawn registry of all output matchers, keyed by resolved upstream name.
///
/// One key may have many matchers (e.g. multiple downstreams subscribing to
/// the same upstream, possibly with different patterns). Per-key vectors are
/// cloned by the upstream's reader thread once at startup; per-line work is
/// then lock-free.
pub struct OutputMatchRegistry {
    matchers: Mutex<HashMap<String, Vec<Arc<Matcher>>>>,
}

impl OutputMatchRegistry {
    pub fn new() -> Self {
        Self {
            matchers: Mutex::new(HashMap::new()),
        }
    }

    /// Register a matcher under `upstream`. May be called many times for the
    /// same upstream — each call appends.
    pub fn register(&self, upstream: &str, matcher: Arc<Matcher>) {
        self.matchers
            .lock()
            .unwrap()
            .entry(upstream.to_string())
            .or_default()
            .push(matcher);
    }

    /// Snapshot all matchers registered under `upstream`. Safe to call before
    /// the upstream is spawned (returns an empty vec); the reader thread calls
    /// it once at startup, after pre-spawn registration is complete.
    pub fn for_upstream(&self, upstream: &str) -> Vec<Arc<Matcher>> {
        self.matchers
            .lock()
            .unwrap()
            .get(upstream)
            .cloned()
            .unwrap_or_default()
    }

    /// Copy all matchers from `from` to each name in `to`. Used when a fan-out
    /// upstream's template name (e.g. `nodes`) materializes into per-instance
    /// names (`nodes-0`, `nodes-1`, ...). Any one instance matching satisfies
    /// the matcher (first-wins via the existing `fired` AtomicBool).
    pub fn copy_template_to_instances(&self, from: &str, to: &[String]) {
        let mut guard = self.matchers.lock().unwrap();
        let template = match guard.get(from).cloned() {
            Some(v) => v,
            None => return,
        };
        for name in to {
            guard
                .entry(name.clone())
                .or_default()
                .extend(template.iter().cloned());
        }
    }
}

/// Wait for `matcher` to fire, or for `timeout` to elapse, or for shutdown.
///
/// Returns `true` if the matcher fired with [`MatchOutcome::Matched`]. Returns
/// `false` on `UpstreamExited`, on timeout, or on shutdown — the caller is
/// expected to log and trigger shutdown when this returns false (other than
/// via the shutdown flag itself).
pub fn wait_for_output_match(
    matcher: &Arc<Matcher>,
    timeout: Option<Duration>,
    shutdown: &AtomicBool,
    logger: &Mutex<Logger>,
    waiter_name: &str,
) -> bool {
    let start = Instant::now();
    let mut guard = matcher.state.lock().unwrap();
    loop {
        if let Some(outcome) = *guard {
            return outcome == MatchOutcome::Matched;
        }
        if shutdown.load(Ordering::Relaxed) {
            return false;
        }
        if let Some(t) = timeout
            && start.elapsed() >= t
        {
            logger.lock().unwrap().log_line(
                waiter_name,
                &format!("dependency timed out: {}", matcher.label),
            );
            return false;
        }
        // Cap each wait at ~100ms so we observe shutdown promptly even when
        // there's no per-condition timeout.
        let wait_chunk = match timeout {
            Some(t) => {
                let remaining = t.saturating_sub(start.elapsed());
                remaining.min(Duration::from_millis(100))
            }
            None => Duration::from_millis(100),
        };
        let (g, _) = matcher.cv.wait_timeout(guard, wait_chunk).unwrap();
        guard = g;
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::atomic::AtomicUsize, thread, time::Instant};

    use super::*;

    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn make_logger() -> Arc<Mutex<Logger>> {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let log_dir =
            std::env::temp_dir().join(format!("procman_match_test_{}_{id}", std::process::id()));
        Arc::new(Mutex::new(
            Logger::new_for_test(&["upstream".to_string(), "waiter".to_string()], log_dir).unwrap(),
        ))
    }

    #[test]
    fn matched_outcome_returns_true() {
        let m = Matcher::new("ready".to_string(), "test".to_string());
        let m_clone = Arc::clone(&m);
        let logger = make_logger();
        let logger_for_thread = Arc::clone(&logger);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            m_clone.fire(MatchOutcome::Matched, &logger_for_thread, "upstream");
        });
        let shutdown = AtomicBool::new(false);
        assert!(wait_for_output_match(
            &m, None, &shutdown, &logger, "waiter"
        ));
    }

    #[test]
    fn upstream_exited_outcome_returns_false() {
        let m = Matcher::new("ready".to_string(), "test".to_string());
        let m_clone = Arc::clone(&m);
        let logger = make_logger();
        let logger_for_thread = Arc::clone(&logger);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            m_clone.fire(MatchOutcome::UpstreamExited, &logger_for_thread, "upstream");
        });
        let shutdown = AtomicBool::new(false);
        assert!(!wait_for_output_match(
            &m, None, &shutdown, &logger, "waiter"
        ));
    }

    #[test]
    fn timeout_returns_false() {
        let m = Matcher::new("never".to_string(), "test".to_string());
        let logger = make_logger();
        let shutdown = AtomicBool::new(false);
        let start = Instant::now();
        assert!(!wait_for_output_match(
            &m,
            Some(Duration::from_millis(150)),
            &shutdown,
            &logger,
            "waiter"
        ));
        assert!(start.elapsed() < Duration::from_millis(500));
        assert!(start.elapsed() >= Duration::from_millis(140));
    }

    #[test]
    fn shutdown_returns_false_promptly() {
        let m = Matcher::new("never".to_string(), "test".to_string());
        let logger = make_logger();
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = Arc::clone(&shutdown);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            shutdown_clone.store(true, Ordering::Relaxed);
        });
        let start = Instant::now();
        assert!(!wait_for_output_match(
            &m, None, &shutdown, &logger, "waiter"
        ));
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "should observe shutdown promptly, took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn fire_is_idempotent() {
        let m = Matcher::new("ready".to_string(), "test".to_string());
        let logger = make_logger();
        m.fire(MatchOutcome::Matched, &logger, "upstream");
        // Second fire is a no-op; the first outcome wins.
        m.fire(MatchOutcome::UpstreamExited, &logger, "upstream");
        let state = *m.state.lock().unwrap();
        assert_eq!(state, Some(MatchOutcome::Matched));
    }

    #[test]
    fn retroactive_match_wakes_immediately() {
        // Matcher fires before the waiter starts waiting — wait should return
        // instantly without blocking.
        let m = Matcher::new("ready".to_string(), "test".to_string());
        let logger = make_logger();
        m.fire(MatchOutcome::Matched, &logger, "upstream");
        let shutdown = AtomicBool::new(false);
        let start = Instant::now();
        assert!(wait_for_output_match(
            &m, None, &shutdown, &logger, "waiter"
        ));
        assert!(
            start.elapsed() < Duration::from_millis(50),
            "retroactive wait should be near-instant, took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn registry_for_upstream_returns_empty_for_unknown() {
        let r = OutputMatchRegistry::new();
        assert!(r.for_upstream("ghost").is_empty());
    }

    #[test]
    fn registry_register_and_for_upstream() {
        let r = OutputMatchRegistry::new();
        let m = Matcher::new("ready".to_string(), "test".to_string());
        r.register("upstream", Arc::clone(&m));
        let snapshot = r.for_upstream("upstream");
        assert_eq!(snapshot.len(), 1);
        assert!(Arc::ptr_eq(&snapshot[0], &m));
    }

    #[test]
    fn registry_multiple_subscribers_per_upstream() {
        let r = OutputMatchRegistry::new();
        let m1 = Matcher::new("a".to_string(), "1".to_string());
        let m2 = Matcher::new("b".to_string(), "2".to_string());
        r.register("upstream", Arc::clone(&m1));
        r.register("upstream", Arc::clone(&m2));
        let snapshot = r.for_upstream("upstream");
        assert_eq!(snapshot.len(), 2);
    }

    #[test]
    fn registry_copy_template_to_instances() {
        let r = OutputMatchRegistry::new();
        let m = Matcher::new("ready".to_string(), "test".to_string());
        r.register("nodes", Arc::clone(&m));
        r.copy_template_to_instances(
            "nodes",
            &[
                "nodes-0".to_string(),
                "nodes-1".to_string(),
                "nodes-2".to_string(),
            ],
        );
        for name in &["nodes-0", "nodes-1", "nodes-2"] {
            let snapshot = r.for_upstream(name);
            assert_eq!(snapshot.len(), 1, "missing matcher under {name}");
            assert!(Arc::ptr_eq(&snapshot[0], &m));
        }
        // Original template name still has the matcher too.
        assert_eq!(r.for_upstream("nodes").len(), 1);
    }

    #[test]
    fn registry_copy_no_op_for_unknown_template() {
        let r = OutputMatchRegistry::new();
        r.copy_template_to_instances("ghost", &["nodes-0".to_string()]);
        assert!(r.for_upstream("nodes-0").is_empty());
    }

    /// Simulate the reader-thread tap: matched line releases the waiter.
    #[test]
    fn reader_tap_simulation_matches_and_releases_waiter() {
        let r = Arc::new(OutputMatchRegistry::new());
        let m = Matcher::new("ready".to_string(), "test".to_string());
        r.register("upstream", Arc::clone(&m));
        let logger = make_logger();

        // Reader thread: snapshot matchers once, then iterate "lines".
        let r_clone = Arc::clone(&r);
        let logger_for_reader = Arc::clone(&logger);
        let reader = thread::spawn(move || {
            let matchers = r_clone.for_upstream("upstream");
            let lines = [
                "starting up",
                "loading config",
                "ready to serve",
                "request handled",
            ];
            for line in lines {
                if !matchers.is_empty() {
                    let any_unfired = matchers.iter().any(|m| !m.fired.load(Ordering::Relaxed));
                    if any_unfired {
                        let stripped = strip_ansi_escapes::strip_str(line);
                        for m in &matchers {
                            if !m.fired.load(Ordering::Relaxed) && stripped.contains(&m.pattern) {
                                m.fire(MatchOutcome::Matched, &logger_for_reader, "upstream");
                            }
                        }
                    }
                }
            }
        });

        let shutdown = AtomicBool::new(false);
        assert!(wait_for_output_match(
            &m,
            Some(Duration::from_millis(500)),
            &shutdown,
            &logger,
            "waiter"
        ));
        reader.join().unwrap();
    }

    /// ANSI-colored output: pattern matches against stripped text.
    #[test]
    fn reader_tap_simulation_strips_ansi() {
        let r = Arc::new(OutputMatchRegistry::new());
        let m = Matcher::new("ready".to_string(), "test".to_string());
        r.register("upstream", Arc::clone(&m));
        let logger = make_logger();

        let line = "\x1b[32mready\x1b[0m";
        let matchers = r.for_upstream("upstream");
        let stripped = strip_ansi_escapes::strip_str(line);
        for m in &matchers {
            if stripped.contains(&m.pattern) {
                m.fire(MatchOutcome::Matched, &logger, "upstream");
            }
        }
        assert!(m.fired.load(Ordering::Relaxed));
    }

    /// EOF without match fires UpstreamExited.
    #[test]
    fn reader_tap_simulation_eof_fires_upstream_exited() {
        let r = Arc::new(OutputMatchRegistry::new());
        let m = Matcher::new("ready".to_string(), "test".to_string());
        r.register("upstream", Arc::clone(&m));
        let logger = make_logger();

        let matchers = r.for_upstream("upstream");
        // Simulate reader: no lines match, then EOF reached.
        for line in &["something_else", "nothing_relevant"] {
            let stripped = strip_ansi_escapes::strip_str(line);
            for m in &matchers {
                if !m.fired.load(Ordering::Relaxed) && stripped.contains(&m.pattern) {
                    m.fire(MatchOutcome::Matched, &logger, "upstream");
                }
            }
        }
        // EOF: notify any unfired matcher.
        for m in &matchers {
            if !m.fired.load(Ordering::Relaxed) {
                m.fire(MatchOutcome::UpstreamExited, &logger, "upstream");
            }
        }

        let shutdown = AtomicBool::new(false);
        assert!(!wait_for_output_match(
            &m, None, &shutdown, &logger, "waiter"
        ));
    }

    /// Substring match (not anchored) and case-sensitive.
    #[test]
    fn pattern_is_substring_and_case_sensitive() {
        let m = Matcher::new("ready".to_string(), "test".to_string());
        let logger = make_logger();

        // Substring match within a longer line.
        let stripped = strip_ansi_escapes::strip_str("server is ready to go");
        if !m.fired.load(Ordering::Relaxed) && stripped.contains(&m.pattern) {
            m.fire(MatchOutcome::Matched, &logger, "upstream");
        }
        assert!(m.fired.load(Ordering::Relaxed));

        // New matcher: case-sensitive — uppercase pattern shouldn't match
        // lowercase text.
        let m2 = Matcher::new("READY".to_string(), "test2".to_string());
        let stripped2 = strip_ansi_escapes::strip_str("server is ready to go");
        if !m2.fired.load(Ordering::Relaxed) && stripped2.contains(&m2.pattern) {
            m2.fire(MatchOutcome::Matched, &logger, "upstream");
        }
        assert!(!m2.fired.load(Ordering::Relaxed));
    }

    /// Multiple subscribers (different patterns) on the same upstream fire
    /// independently when each pattern is observed.
    #[test]
    fn multiple_subscribers_fire_independently() {
        let r = Arc::new(OutputMatchRegistry::new());
        let m_ready = Matcher::new("ready".to_string(), "ready-watcher".to_string());
        let m_loaded = Matcher::new("loaded".to_string(), "loaded-watcher".to_string());
        r.register("upstream", Arc::clone(&m_ready));
        r.register("upstream", Arc::clone(&m_loaded));
        let logger = make_logger();

        let matchers = r.for_upstream("upstream");
        for line in &["configs loaded", "starting up", "now ready"] {
            let any_unfired = matchers.iter().any(|m| !m.fired.load(Ordering::Relaxed));
            if !any_unfired {
                break;
            }
            let stripped = strip_ansi_escapes::strip_str(line);
            for m in &matchers {
                if !m.fired.load(Ordering::Relaxed) && stripped.contains(&m.pattern) {
                    m.fire(MatchOutcome::Matched, &logger, "upstream");
                }
            }
        }
        assert!(m_ready.fired.load(Ordering::Relaxed));
        assert!(m_loaded.fired.load(Ordering::Relaxed));
    }
}
