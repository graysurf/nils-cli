//! Proactive GitHub GraphQL rate-limit gate for GraphQL-backed backend calls.
//!
//! Background: GitHub meters the GraphQL API on a budget that is separate from
//! the REST/core budget. A shared GraphQL budget can be drained by other
//! consumers while REST/core still has thousands of requests left. A
//! subsequent GraphQL-backed `gh pr …` / `gh api graphql` call then fails —
//! historically surfacing as a misleading "not available" / not-found error
//! rather than an explicit rate-limit signal (sympoies/nils-cli#1051).
//!
//! [`RateLimitedRunner`] decorates any [`BackendRunner`] so GraphQL-backed
//! calls:
//!   1. **preflight** the FREE `gh api rate_limit` endpoint (which does not
//!      consume quota) and wait, bounded by `max_wait`, for
//!      `resources.graphql.remaining` to recover before issuing the call, and
//!   2. on a `backend_rate_limited` failure (see
//!      [`crate::backend::is_rate_limit_stderr`]) wait for recovery and retry
//!      the call once.
//!
//! REST calls (`gh api repos/…`), non-GitHub backends, and the probe itself are
//! never gated. The gate is best-effort: an unreadable or failing probe never
//! blocks real work. Timing is driven through the [`Clock`] trait (shared with
//! `pr wait-checks`) so tests step time deterministically instead of sleeping.
//!
//! The gate is enabled by default and tuned through the environment:
//!   - `FORGE_CLI_RATE_LIMIT_GATE=off|0|false|no` disables it entirely.
//!   - `FORGE_CLI_RATE_LIMIT_MIN_REMAINING` (default 50) — proceed immediately
//!     once `graphql.remaining` exceeds this.
//!   - `FORGE_CLI_RATE_LIMIT_MAX_WAIT_SECS` (default 120) — cap on total wait.
//!   - `FORGE_CLI_RATE_LIMIT_POLL_SECS` (default 15) — re-probe interval while
//!     throttled.

use std::time::Duration;

use crate::backend::{
    BackendCall, BackendOutput, BackendProgram, BackendRunner, BackendSuccess, ProcessRunner,
};
use crate::error::ForgeError;
use crate::ops::pr_wait_checks::{Clock, SystemClock};

/// `error.kind` emitted by the backend when a call is throttled. The gate keys
/// its reactive retry off this discriminator.
pub const RATE_LIMITED_KIND: &str = "backend_rate_limited";

/// Tunable gate policy resolved from the environment.
#[derive(Debug, Clone)]
pub struct GateConfig {
    /// When false the decorator is a transparent passthrough (no probe, no
    /// wait, no retry).
    pub enabled: bool,
    /// Proceed immediately when `graphql.remaining` is strictly greater than
    /// this. Keeps the healthy path to a single fast probe.
    pub min_remaining: u64,
    /// Upper bound on the total time spent waiting for the budget to recover,
    /// per gated attempt.
    pub max_wait: Duration,
    /// Interval between re-probes while the budget is below `min_remaining`.
    pub poll_interval: Duration,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_remaining: 50,
            max_wait: Duration::from_secs(120),
            poll_interval: Duration::from_secs(15),
        }
    }
}

impl GateConfig {
    /// Resolve the gate policy from environment overrides, falling back to the
    /// [`Default`] values for anything unset or unparseable.
    pub fn from_env() -> Self {
        let defaults = Self::default();
        let enabled = !matches!(
            env_lower("FORGE_CLI_RATE_LIMIT_GATE").as_deref(),
            Some("off") | Some("0") | Some("false") | Some("no")
        );
        let min_remaining =
            env_u64("FORGE_CLI_RATE_LIMIT_MIN_REMAINING").unwrap_or(defaults.min_remaining);
        let max_wait = env_u64("FORGE_CLI_RATE_LIMIT_MAX_WAIT_SECS")
            .map(Duration::from_secs)
            .unwrap_or(defaults.max_wait);
        // A zero poll interval would busy-loop; clamp to at least one second.
        let poll_interval = env_u64("FORGE_CLI_RATE_LIMIT_POLL_SECS")
            .map(|s| Duration::from_secs(s.max(1)))
            .unwrap_or(defaults.poll_interval);
        Self {
            enabled,
            min_remaining,
            max_wait,
            poll_interval,
        }
    }
}

fn env_lower(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty())
}

fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok()?.trim().parse::<u64>().ok()
}

/// Classify whether a backend call is GraphQL-backed and therefore subject to
/// the GraphQL budget. Only GitHub (`gh`) calls qualify; the free
/// `gh api rate_limit` probe and REST `gh api repos/…` calls are excluded so
/// the gate never gates itself or a call that draws on the core budget.
pub fn is_graphql_backed(call: &BackendCall) -> bool {
    if call.program != BackendProgram::Gh {
        return false;
    }
    let mut args = call.argv.iter().map(|a| a.to_string_lossy());
    match args.next().as_deref() {
        // `gh api graphql …` is GraphQL; every other `gh api …` (rate_limit,
        // repos/…, …) is REST/core.
        Some("api") => args.next().as_deref() == Some("graphql"),
        // `gh pr|issue|search|repo|release …` are GraphQL-backed.
        Some("pr") | Some("issue") | Some("search") | Some("repo") | Some("release") => true,
        _ => false,
    }
}

/// The free rate-limit probe call. `gh api rate_limit` returns the full REST
/// rate-limit document and does not consume any budget.
fn probe_call() -> BackendCall {
    BackendCall::new(BackendProgram::Gh, ["api", "rate_limit"])
}

/// Extract `resources.graphql.remaining` from a `gh api rate_limit` document.
/// Returns `None` when the payload is missing, malformed, or shaped
/// unexpectedly so callers can treat the probe as best-effort.
pub fn parse_graphql_remaining(json: &str) -> Option<u64> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    value
        .get("resources")?
        .get("graphql")?
        .get("remaining")?
        .as_u64()
}

/// A [`BackendRunner`] that preflights the GraphQL budget before GraphQL-backed
/// calls and retries once after a rate-limit failure.
pub struct RateLimitedRunner<R, C> {
    inner: R,
    clock: C,
    config: GateConfig,
}

impl RateLimitedRunner<ProcessRunner, SystemClock> {
    /// Production runner: wraps a real [`ProcessRunner`] with the system clock
    /// and the environment-resolved policy.
    pub fn production() -> Self {
        Self::new(ProcessRunner, SystemClock, GateConfig::from_env())
    }
}

impl<R: BackendRunner, C: Clock> RateLimitedRunner<R, C> {
    pub fn new(inner: R, clock: C, config: GateConfig) -> Self {
        Self {
            inner,
            clock,
            config,
        }
    }

    fn should_gate(&self, call: &BackendCall) -> bool {
        self.config.enabled && is_graphql_backed(call)
    }

    /// Best-effort read of the current GraphQL remaining budget. `None` when
    /// the probe fails or is unparseable.
    fn probe_remaining(&self) -> Option<u64> {
        let output = self.inner.run(&probe_call()).ok()?;
        parse_graphql_remaining(&output.stdout)
    }

    /// Poll `gh api rate_limit` until `graphql.remaining` exceeds the
    /// configured threshold or `max_wait` elapses. Best-effort: an unreadable
    /// probe returns immediately so a broken probe never blocks real work.
    fn wait_until_healthy(&self) {
        let start = self.clock.now();
        loop {
            match self.probe_remaining() {
                // Cannot read the budget — do not block; proceed and let the
                // real call (and its reactive retry) handle any throttling.
                None => return,
                Some(remaining) if remaining > self.config.min_remaining => return,
                Some(_) => {
                    let elapsed = self.clock.now().saturating_duration_since(start);
                    let budget_left = self.config.max_wait.saturating_sub(elapsed);
                    if budget_left.is_zero() {
                        return;
                    }
                    let nap = std::cmp::min(self.config.poll_interval, budget_left);
                    if nap.is_zero() {
                        return;
                    }
                    self.clock.sleep(nap);
                }
            }
        }
    }

    /// Run `call` through `run`, gating GraphQL-backed calls: preflight for a
    /// healthy budget, then on a `backend_rate_limited` failure wait for
    /// recovery and retry exactly once.
    fn gated<T>(
        &self,
        call: &BackendCall,
        run: impl Fn(&BackendCall) -> Result<T, ForgeError>,
    ) -> Result<T, ForgeError> {
        if !self.should_gate(call) {
            return run(call);
        }
        self.wait_until_healthy();
        match run(call) {
            Err(err) if err.kind() == RATE_LIMITED_KIND => {
                self.wait_until_healthy();
                run(call)
            }
            other => other,
        }
    }
}

impl<R: BackendRunner, C: Clock> BackendRunner for RateLimitedRunner<R, C> {
    fn run(&self, call: &BackendCall) -> Result<BackendSuccess, ForgeError> {
        self.gated(call, |c| self.inner.run(c))
    }

    fn run_with_timeout(
        &self,
        call: &BackendCall,
        timeout: Option<Duration>,
    ) -> Result<BackendSuccess, ForgeError> {
        self.gated(call, |c| self.inner.run_with_timeout(c, timeout))
    }

    fn run_raw(&self, call: &BackendCall) -> Result<BackendOutput, ForgeError> {
        self.gated(call, |c| self.inner.run_raw(c))
    }

    fn run_raw_with_timeout(
        &self,
        call: &BackendCall,
        timeout: Option<Duration>,
    ) -> Result<BackendOutput, ForgeError> {
        self.gated(call, |c| self.inner.run_raw_with_timeout(c, timeout))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::time::Instant;

    use pretty_assertions::assert_eq;

    /// Scripted runner that returns queued rate-limit-probe responses and
    /// records every call's argv. Non-probe calls return a fixed success (or a
    /// scripted rate-limit error, once) so the decorator's gating path can be
    /// observed without a real subprocess.
    struct FakeRunner {
        /// Queue of `graphql.remaining` values served by successive probes.
        probe_remaining: RefCell<Vec<u64>>,
        /// When true, the first non-probe call fails with `backend_rate_limited`.
        fail_once: RefCell<bool>,
        /// Recorded argv of every call (probe and real).
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl FakeRunner {
        fn new(probe_remaining: Vec<u64>) -> Self {
            Self {
                probe_remaining: RefCell::new(probe_remaining),
                fail_once: RefCell::new(false),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn with_fail_once(probe_remaining: Vec<u64>) -> Self {
            let r = Self::new(probe_remaining);
            *r.fail_once.borrow_mut() = true;
            r
        }

        fn is_probe(call: &BackendCall) -> bool {
            let argv: Vec<String> = call
                .argv
                .iter()
                .map(|a| a.to_string_lossy().into())
                .collect();
            argv == ["api", "rate_limit"]
        }

        fn probe_count(&self) -> usize {
            self.calls
                .borrow()
                .iter()
                .filter(|c| c.as_slice() == ["api", "rate_limit"])
                .count()
        }

        fn real_count(&self) -> usize {
            self.calls.borrow().len() - self.probe_count()
        }
    }

    impl BackendRunner for FakeRunner {
        fn run(&self, call: &BackendCall) -> Result<BackendSuccess, ForgeError> {
            self.calls.borrow_mut().push(
                call.argv
                    .iter()
                    .map(|a| a.to_string_lossy().into())
                    .collect(),
            );
            if Self::is_probe(call) {
                let mut queue = self.probe_remaining.borrow_mut();
                let remaining = if queue.is_empty() {
                    9999
                } else {
                    queue.remove(0)
                };
                return Ok(BackendSuccess {
                    stdout: format!(
                        r#"{{"resources":{{"graphql":{{"limit":5000,"remaining":{remaining},"reset":1700000000}}}}}}"#
                    ),
                    stderr: String::new(),
                });
            }
            if *self.fail_once.borrow() {
                *self.fail_once.borrow_mut() = false;
                return Err(ForgeError::unavailable(
                    "cli.forge-cli.error.v1",
                    RATE_LIMITED_KIND,
                    "throttled",
                    None,
                ));
            }
            Ok(BackendSuccess {
                stdout: "ok".into(),
                stderr: String::new(),
            })
        }
    }

    /// Deterministic clock: `now()` advances by a fixed step each `sleep`, and
    /// every sleep duration is recorded.
    struct FakeClock {
        now: RefCell<Instant>,
        slept: RefCell<Vec<Duration>>,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                now: RefCell::new(Instant::now()),
                slept: RefCell::new(Vec::new()),
            }
        }

        fn total_slept(&self) -> Duration {
            self.slept.borrow().iter().copied().sum()
        }

        fn sleep_count(&self) -> usize {
            self.slept.borrow().len()
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            *self.now.borrow()
        }
        fn sleep(&self, dur: Duration) {
            self.slept.borrow_mut().push(dur);
            *self.now.borrow_mut() += dur;
        }
    }

    fn cfg() -> GateConfig {
        GateConfig {
            enabled: true,
            min_remaining: 50,
            max_wait: Duration::from_secs(120),
            poll_interval: Duration::from_secs(15),
        }
    }

    fn pr_ready_call() -> BackendCall {
        BackendCall::new(BackendProgram::Gh, ["pr", "ready", "42"])
    }

    #[test]
    fn is_graphql_backed_matches_pr_and_graphql_api_only() {
        assert!(is_graphql_backed(&BackendCall::new(
            BackendProgram::Gh,
            ["pr", "view", "1"]
        )));
        assert!(is_graphql_backed(&BackendCall::new(
            BackendProgram::Gh,
            ["api", "graphql", "-f", "query=x"]
        )));
        assert!(is_graphql_backed(&BackendCall::new(
            BackendProgram::Gh,
            ["release", "view", "v1"]
        )));
        // REST + the probe itself are not gated.
        assert!(!is_graphql_backed(&BackendCall::new(
            BackendProgram::Gh,
            ["api", "rate_limit"]
        )));
        assert!(!is_graphql_backed(&BackendCall::new(
            BackendProgram::Gh,
            ["api", "repos/o/r/releases/tags/v1"]
        )));
        assert!(!is_graphql_backed(&BackendCall::new(
            BackendProgram::Gh,
            ["auth", "status"]
        )));
        // Non-GitHub backends are never gated.
        assert!(!is_graphql_backed(&BackendCall::new(
            BackendProgram::Glab,
            ["mr", "view", "1"]
        )));
    }

    #[test]
    fn parse_graphql_remaining_reads_nested_field() {
        let json = r#"{"resources":{"core":{"remaining":4821},"graphql":{"limit":5000,"remaining":0,"reset":1700000000}}}"#;
        assert_eq!(parse_graphql_remaining(json), Some(0));
        assert_eq!(parse_graphql_remaining("not json"), None);
        assert_eq!(parse_graphql_remaining(r#"{"resources":{}}"#), None);
    }

    #[test]
    fn healthy_budget_probes_once_and_runs_without_sleeping() {
        let runner = FakeRunner::new(vec![4821]);
        let clock = FakeClock::new();
        let gate = RateLimitedRunner::new(&runner, &clock, cfg());

        let out = gate.run(&pr_ready_call()).expect("run");
        assert_eq!(out.stdout, "ok");
        assert_eq!(runner.probe_count(), 1, "one preflight probe");
        assert_eq!(runner.real_count(), 1, "the real call ran once");
        assert_eq!(clock.sleep_count(), 0, "healthy budget never sleeps");
    }

    #[test]
    fn throttled_budget_waits_then_proceeds_when_recovered() {
        // First probe: throttled (0); after one sleep the budget recovers.
        let runner = FakeRunner::new(vec![0, 500]);
        let clock = FakeClock::new();
        let gate = RateLimitedRunner::new(&runner, &clock, cfg());

        gate.run(&pr_ready_call()).expect("run");
        assert_eq!(runner.probe_count(), 2, "re-probed after sleeping");
        assert_eq!(clock.sleep_count(), 1);
        assert_eq!(clock.total_slept(), Duration::from_secs(15));
        assert_eq!(runner.real_count(), 1);
    }

    #[test]
    fn throttled_budget_gives_up_after_max_wait_and_proceeds() {
        // Budget never recovers; the gate must stop waiting at max_wait
        // (120s / 15s poll = 8 sleeps) and still run the call.
        let runner = FakeRunner::new(vec![0; 32]);
        let clock = FakeClock::new();
        let gate = RateLimitedRunner::new(&runner, &clock, cfg());

        gate.run(&pr_ready_call()).expect("run");
        assert!(
            clock.total_slept() <= Duration::from_secs(120),
            "bounded by max_wait"
        );
        assert_eq!(clock.total_slept(), Duration::from_secs(120));
        assert_eq!(clock.sleep_count(), 8);
        assert_eq!(
            runner.real_count(),
            1,
            "still attempts the call after giving up"
        );
    }

    #[test]
    fn rate_limited_failure_waits_and_retries_once() {
        // Healthy preflight, the real call fails once with backend_rate_limited,
        // the gate re-checks (healthy) and retries, which succeeds.
        let runner = FakeRunner::with_fail_once(vec![4821, 4821]);
        let clock = FakeClock::new();
        let gate = RateLimitedRunner::new(&runner, &clock, cfg());

        let out = gate.run(&pr_ready_call()).expect("retry succeeds");
        assert_eq!(out.stdout, "ok");
        assert_eq!(runner.real_count(), 2, "one failure + one retry");
    }

    #[test]
    fn non_graphql_call_is_not_gated() {
        let runner = FakeRunner::new(vec![0]);
        let clock = FakeClock::new();
        let gate = RateLimitedRunner::new(&runner, &clock, cfg());

        gate.run(&BackendCall::new(BackendProgram::Gh, ["api", "rate_limit"]))
            .expect("run");
        // The rate_limit call itself is a probe-shaped REST call: not gated, so
        // no *extra* preflight probe is inserted ahead of it.
        assert_eq!(
            runner.probe_count(),
            1,
            "only the call itself, no gate probe"
        );
        assert_eq!(clock.sleep_count(), 0);
    }

    #[test]
    fn disabled_gate_is_transparent_passthrough() {
        let runner = FakeRunner::new(vec![0]);
        let clock = FakeClock::new();
        let mut config = cfg();
        config.enabled = false;
        let gate = RateLimitedRunner::new(&runner, &clock, config);

        gate.run(&pr_ready_call()).expect("run");
        assert_eq!(runner.probe_count(), 0, "no probe when disabled");
        assert_eq!(runner.real_count(), 1);
        assert_eq!(clock.sleep_count(), 0);
    }

    #[test]
    fn unreadable_probe_does_not_block() {
        // A probe that returns an unparseable body yields None → proceed.
        struct BadProbe;
        impl BackendRunner for BadProbe {
            fn run(&self, call: &BackendCall) -> Result<BackendSuccess, ForgeError> {
                if call
                    .argv
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    == ["api", "rate_limit"]
                {
                    return Ok(BackendSuccess {
                        stdout: "garbage".into(),
                        stderr: String::new(),
                    });
                }
                Ok(BackendSuccess {
                    stdout: "ok".into(),
                    stderr: String::new(),
                })
            }
        }
        let clock = FakeClock::new();
        let gate = RateLimitedRunner::new(&BadProbe, &clock, cfg());
        let out = gate.run(&pr_ready_call()).expect("run");
        assert_eq!(out.stdout, "ok");
        assert_eq!(clock.sleep_count(), 0, "unreadable probe never sleeps");
    }
}
