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
//! **Wiring scope.** Every op `run()` entrypoint builds its live backend runner
//! through the single [`default_runner`] factory rather than a bare
//! [`ProcessRunner`], so all ops are gated by default (sympoies/nils-cli#1063).
//! Because the decorator no-ops on non-GraphQL and non-GitHub calls, routing
//! *every* op through it is safe: only GraphQL-backed calls (see
//! [`is_graphql_backed`]) actually preflight/back off. Centralizing the "which
//! runner" decision in one place means the classifier's breadth and the wiring
//! can no longer drift — a newly-added op cannot silently ship ungated. A guard
//! test (`ops_construct_runner_via_factory`) enforces the convention.
//!
//! The gate is enabled by default and tuned through the environment:
//!   - `FORGE_CLI_RATE_LIMIT_GATE=off|0|false|no` disables it entirely.
//!   - `FORGE_CLI_RATE_LIMIT_MIN_REMAINING` (default 50) — proceed immediately
//!     once `graphql.remaining` exceeds this.
//!   - `FORGE_CLI_RATE_LIMIT_MAX_WAIT_SECS` (default 120) — cap on total wait.
//!   - `FORGE_CLI_RATE_LIMIT_POLL_SECS` (default 15) — re-probe interval while
//!     throttled.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::backend::{
    BackendCall, BackendOutput, BackendProgram, BackendRunner, BackendSuccess, ProcessRunner,
};
use crate::error::ForgeError;
use crate::ops::pr_wait_checks::{Clock, SystemClock};

/// `error.kind` emitted by the backend when a call is throttled. The gate keys
/// its reactive retry off this discriminator.
pub const RATE_LIMITED_KIND: &str = "backend_rate_limited";

/// Hard timeout for the best-effort `gh api rate_limit` probe. A stalled probe
/// must never block real work, so it is bounded independently of the gated
/// call and a probe timeout is treated like an unreadable probe (proceed).
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Tunable gate policy resolved from the environment.
#[derive(Debug, Clone)]
pub struct GateConfig {
    /// When false the decorator is a transparent passthrough (no probe, no
    /// wait, no retry).
    pub enabled: bool,
    /// Proceed immediately when `graphql.remaining` is strictly greater than
    /// this. Keeps the healthy path to a single fast probe.
    pub min_remaining: u64,
    /// Upper bound on a single wait-for-recovery phase. Note that a gated call
    /// has two independent wait phases — the preflight and, after a
    /// `backend_rate_limited` failure, one reactive wait — so a single gated
    /// call can block up to `2 * max_wait`, and a multi-call op (`pr deliver`,
    /// `pr wait-checks`) bounds each of its calls independently rather than
    /// end-to-end.
    pub max_wait: Duration,
    /// Interval between re-probes while the budget is below `min_remaining`.
    /// Doubles as the freshness window for the cached probe reading and as the
    /// minimum backoff before a reactive retry.
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
///
/// Classification is **verb-coarse**: it treats an entire `gh` verb as
/// GraphQL-backed rather than distinguishing per-subcommand (e.g. the
/// REST-backed `gh release download` / `gh repo clone` would be classified
/// GraphQL here). Every op is now routed through [`RateLimitedRunner`] via
/// [`default_runner`], so this predicate is the sole thing that decides whether
/// a call is actually gated. It is safe today only because no op issues a
/// REST-backed subcommand of these verbs — the only `repo` call in the tree is
/// `gh repo view`, which is genuinely GraphQL-backed. Before adding an op that
/// shells a REST-backed subcommand of `pr`/`issue`/`search`/`repo`/`release`
/// (e.g. `gh release download`, `gh repo clone`), refine this matcher to key on
/// the subcommand, or that call would needlessly preflight and back off against
/// the GraphQL budget for a request that spends the core budget.
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
    /// Last probe reading `(taken_at, remaining)`, reused within
    /// `config.poll_interval` so a burst of *sequential* closely-spaced gated
    /// calls (e.g. the `pr deliver` chain, or the two `pr checks` calls per
    /// wait-checks poll) collapses to a single probe instead of one per call.
    ///
    /// A `Mutex` (not `RefCell`) so the runner stays `Sync`: ops that fan out
    /// across threads (`inbox`'s parallel provider queries) share one runner
    /// and require `R: Sync`. There is no single-flight coordination, so a
    /// *concurrent* cold-cache fan-out issues up to one probe per thread rather
    /// than one total — bounded, and against the free `rate_limit` endpoint, so
    /// it is a small request burst, not a latency or correctness risk. Critical
    /// sections only copy the small reading in or out, never spanning a backend
    /// call, so the lock is never held across I/O.
    probe_cache: Mutex<Option<(Instant, u64)>>,
}

/// The default backend runner for every production op `run()` entrypoint, and
/// the sole sanctioned way to build a live runner. It wraps a real
/// [`ProcessRunner`] with the system clock and the environment-resolved policy.
///
/// This is the one place the "which runner do live ops use" decision lives.
/// Every op routes its live (non-local) backend calls through the returned
/// gated runner instead of constructing a bare [`ProcessRunner`], so the
/// GraphQL rate-limit gate applies uniformly and no op can silently bypass it.
/// The gate is a transparent passthrough for non-GraphQL / non-GitHub calls
/// (and when disabled via `FORGE_CLI_RATE_LIMIT_GATE=off`), so wrapping every
/// op is safe. A guard test enforces that ops use this factory rather than a
/// bare runner.
pub fn default_runner() -> RateLimitedRunner<ProcessRunner, SystemClock> {
    RateLimitedRunner::new(ProcessRunner, SystemClock, GateConfig::from_env())
}

impl<R: BackendRunner, C: Clock> RateLimitedRunner<R, C> {
    pub fn new(inner: R, clock: C, config: GateConfig) -> Self {
        Self {
            inner,
            clock,
            config,
            probe_cache: Mutex::new(None),
        }
    }

    fn should_gate(&self, call: &BackendCall) -> bool {
        self.config.enabled && is_graphql_backed(call)
    }

    /// Best-effort read of the current GraphQL remaining budget. `None` when
    /// the probe fails, times out, or is unparseable. Bounded by
    /// [`PROBE_TIMEOUT`] so a stalled endpoint never blocks the gated call.
    fn probe_remaining(&self) -> Option<u64> {
        let output = self
            .inner
            .run_with_timeout(&probe_call(), Some(PROBE_TIMEOUT))
            .ok()?;
        parse_graphql_remaining(&output.stdout)
    }

    /// A GraphQL-remaining reading, reusing the cached probe when it is younger
    /// than `poll_interval` and otherwise re-probing (and refreshing the
    /// cache). An unreadable probe is not cached, so the next call re-probes.
    fn cached_remaining(&self) -> Option<u64> {
        // Copy the small reading out and release the lock before probing — the
        // probe issues a backend call and must never run under the lock.
        let cached = *self.probe_cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((taken_at, remaining)) = cached
            && self.clock.now().saturating_duration_since(taken_at) < self.config.poll_interval
        {
            return Some(remaining);
        }
        let remaining = self.probe_remaining()?;
        *self.probe_cache.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((self.clock.now(), remaining));
        Some(remaining)
    }

    /// Drop the cached probe reading. Called after a `backend_rate_limited`
    /// failure so the reactive wait re-probes fresh rather than trusting a
    /// reading that predates the throttling.
    fn invalidate_cache(&self) {
        *self.probe_cache.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Poll `gh api rate_limit` until `graphql.remaining` exceeds the
    /// configured threshold or `max_wait` elapses. Best-effort: an unreadable
    /// probe returns immediately so a broken probe never blocks real work.
    fn wait_until_healthy(&self) {
        let start = self.clock.now();
        loop {
            match self.cached_remaining() {
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
    /// healthy budget, then on a `backend_rate_limited` failure back off and
    /// retry exactly once.
    ///
    /// The single retry replays the individual failing backend call. For the
    /// mutating lifecycle ops (`pr merge`/`pr ready`) that is safe: a
    /// rate-limited call did not execute server-side, and these ops are
    /// effectively idempotent. Before retrying we drop the stale cached reading
    /// and always sleep at least one `poll_interval` — a floor that gives
    /// secondary/abuse rate limits (which do not deplete `graphql.remaining`,
    /// so `wait_until_healthy` would return immediately) a real backoff instead
    /// of an instant hammer.
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
                self.invalidate_cache();
                self.clock.sleep(self.config.poll_interval);
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

    /// Scripted runner that serves queued `graphql.remaining` probe values and
    /// records every call's argv. The first `fail_remaining` non-probe calls
    /// fail with `fail_kind`; the rest succeed. This lets tests drive the
    /// preflight, reactive-retry, and passthrough paths without a subprocess.
    struct FakeRunner {
        probe_remaining: RefCell<Vec<u64>>,
        fail_remaining: RefCell<usize>,
        fail_kind: &'static str,
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl FakeRunner {
        fn new(probe_remaining: Vec<u64>) -> Self {
            Self {
                probe_remaining: RefCell::new(probe_remaining),
                fail_remaining: RefCell::new(0),
                fail_kind: RATE_LIMITED_KIND,
                calls: RefCell::new(Vec::new()),
            }
        }

        /// The first non-probe call fails with `backend_rate_limited`.
        fn with_fail_once(probe_remaining: Vec<u64>) -> Self {
            Self::with_fails(probe_remaining, 1, RATE_LIMITED_KIND)
        }

        /// The first `count` non-probe calls fail with `kind`.
        fn with_fails(probe_remaining: Vec<u64>, count: usize, kind: &'static str) -> Self {
            let r = Self::new(probe_remaining);
            *r.fail_remaining.borrow_mut() = count;
            Self {
                fail_kind: kind,
                ..r
            }
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
            let mut fails = self.fail_remaining.borrow_mut();
            if *fails > 0 {
                *fails -= 1;
                return Err(ForgeError::unavailable(
                    "cli.forge-cli.error.v1",
                    self.fail_kind,
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
    fn rate_limited_failure_backs_off_waits_and_retries_once() {
        // Healthy preflight; the real call fails once with backend_rate_limited;
        // the reactive path drops the stale reading, sleeps the floor, then
        // waits for the (initially still-throttled) budget to recover before a
        // single successful retry. Non-vacuous: removing the reactive
        // wait_until_healthy would change the sleep/probe counts asserted here.
        let runner = FakeRunner::with_fail_once(vec![4821, 0, 500]);
        let clock = FakeClock::new();
        let gate = RateLimitedRunner::new(&runner, &clock, cfg());

        let out = gate.run(&pr_ready_call()).expect("retry succeeds");
        assert_eq!(out.stdout, "ok");
        assert_eq!(runner.real_count(), 2, "one failure + exactly one retry");
        // floor backoff (15s) + one recovery poll (15s).
        assert_eq!(clock.sleep_count(), 2);
        assert_eq!(clock.total_slept(), Duration::from_secs(30));
        // preflight + two reactive probes (throttled, then recovered).
        assert_eq!(runner.probe_count(), 3);
    }

    #[test]
    fn persistent_throttle_retries_once_then_surfaces_rate_limit_error() {
        // The real call fails on every attempt. The gate must retry exactly
        // once (not loop) and then surface the backend_rate_limited error
        // rather than swallow it — the core #1051 signal.
        let runner = FakeRunner::with_fails(vec![4821, 4821], 5, RATE_LIMITED_KIND);
        let clock = FakeClock::new();
        let gate = RateLimitedRunner::new(&runner, &clock, cfg());

        let err = gate.run(&pr_ready_call()).expect_err("persistent throttle");
        assert_eq!(
            err.kind(),
            RATE_LIMITED_KIND,
            "error is surfaced, not swallowed"
        );
        assert_eq!(runner.real_count(), 2, "bounded to a single retry");
    }

    #[test]
    fn non_rate_limit_error_passes_through_without_retry_or_extra_probe() {
        // A generic (non-throttle) failure must be returned as-is: no reactive
        // wait, no second probe, no retry.
        let runner = FakeRunner::with_fails(vec![4821], 1, "backend_error");
        let clock = FakeClock::new();
        let gate = RateLimitedRunner::new(&runner, &clock, cfg());

        let err = gate.run(&pr_ready_call()).expect_err("generic failure");
        assert_eq!(err.kind(), "backend_error");
        assert_eq!(runner.real_count(), 1, "not retried");
        assert_eq!(runner.probe_count(), 1, "only the preflight probe");
        assert_eq!(clock.sleep_count(), 0);
    }

    #[test]
    fn run_raw_is_gated_like_run() {
        // The run_raw path (used by pr checks) must preflight identically.
        let runner = FakeRunner::new(vec![0, 500]);
        let clock = FakeClock::new();
        let gate = RateLimitedRunner::new(&runner, &clock, cfg());

        gate.run_raw(&pr_ready_call()).expect("run_raw");
        assert_eq!(runner.probe_count(), 2, "re-probed after sleeping");
        assert_eq!(clock.sleep_count(), 1);
        assert_eq!(runner.real_count(), 1);
    }

    #[test]
    fn run_with_timeout_is_gated_like_run() {
        // The run_with_timeout path (used by inbox's threaded fan-out) must
        // preflight identically to run/run_raw — its outer wrapper is otherwise
        // exercised by no other test.
        let runner = FakeRunner::new(vec![0, 500]);
        let clock = FakeClock::new();
        let gate = RateLimitedRunner::new(&runner, &clock, cfg());

        gate.run_with_timeout(&pr_ready_call(), Some(Duration::from_secs(5)))
            .expect("run_with_timeout");
        assert_eq!(runner.probe_count(), 2, "re-probed after sleeping");
        assert_eq!(clock.sleep_count(), 1);
        assert_eq!(runner.real_count(), 1);
    }

    #[test]
    fn min_remaining_boundary_is_strict_greater_than() {
        // remaining == min_remaining is throttled; min_remaining + 1 proceeds.
        let at_threshold = FakeRunner::new(vec![50, 500]);
        let clock = FakeClock::new();
        RateLimitedRunner::new(&at_threshold, &clock, cfg())
            .run(&pr_ready_call())
            .expect("run");
        assert_eq!(clock.sleep_count(), 1, "remaining == min_remaining waits");

        let above = FakeRunner::new(vec![51]);
        let clock2 = FakeClock::new();
        RateLimitedRunner::new(&above, &clock2, cfg())
            .run(&pr_ready_call())
            .expect("run");
        assert_eq!(
            clock2.sleep_count(),
            0,
            "remaining > min_remaining proceeds"
        );
    }

    #[test]
    fn partial_final_nap_never_overshoots_max_wait() {
        // max_wait (100s) is not a multiple of poll_interval (15s): the final
        // nap must be clamped to the remaining budget so total == max_wait.
        let runner = FakeRunner::new(vec![0; 32]);
        let clock = FakeClock::new();
        let config = GateConfig {
            max_wait: Duration::from_secs(100),
            poll_interval: Duration::from_secs(15),
            ..cfg()
        };
        RateLimitedRunner::new(&runner, &clock, config)
            .run(&pr_ready_call())
            .expect("run");
        assert_eq!(clock.total_slept(), Duration::from_secs(100));
    }

    #[test]
    fn cached_probe_reused_within_poll_interval() {
        // Two back-to-back gated calls within the freshness window issue exactly
        // one probe, not one per call. Only one probe value is queued, so a
        // second probe would be provable via probe_count.
        let runner = FakeRunner::new(vec![4821]);
        let clock = FakeClock::new();
        let gate = RateLimitedRunner::new(&runner, &clock, cfg());

        gate.run(&pr_ready_call()).expect("first");
        gate.run(&pr_ready_call()).expect("second");
        assert_eq!(
            runner.probe_count(),
            1,
            "second call reused the cached probe"
        );
        assert_eq!(runner.real_count(), 2);
        assert_eq!(clock.sleep_count(), 0);
    }

    #[test]
    fn from_env_parses_disable_tokens_fallbacks_and_clamp() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let keys = [
            "FORGE_CLI_RATE_LIMIT_GATE",
            "FORGE_CLI_RATE_LIMIT_MIN_REMAINING",
            "FORGE_CLI_RATE_LIMIT_MAX_WAIT_SECS",
            "FORGE_CLI_RATE_LIMIT_POLL_SECS",
        ];
        let clear = || unsafe {
            for k in keys {
                std::env::remove_var(k);
            }
        };

        clear();
        for token in ["off", "0", "false", "no", "OFF"] {
            unsafe { std::env::set_var("FORGE_CLI_RATE_LIMIT_GATE", token) };
            assert!(!GateConfig::from_env().enabled, "token {token} disables");
        }
        unsafe { std::env::set_var("FORGE_CLI_RATE_LIMIT_GATE", "on") };
        assert!(
            GateConfig::from_env().enabled,
            "any other value stays enabled"
        );

        // Garbage numeric values fall back to the defaults.
        unsafe {
            std::env::set_var("FORGE_CLI_RATE_LIMIT_MIN_REMAINING", "not-a-number");
            std::env::set_var("FORGE_CLI_RATE_LIMIT_MAX_WAIT_SECS", "");
        }
        let cfg = GateConfig::from_env();
        assert_eq!(cfg.min_remaining, 50);
        assert_eq!(cfg.max_wait, Duration::from_secs(120));

        // A zero poll interval is clamped to 1s to avoid a busy-loop.
        unsafe { std::env::set_var("FORGE_CLI_RATE_LIMIT_POLL_SECS", "0") };
        assert_eq!(GateConfig::from_env().poll_interval, Duration::from_secs(1));

        clear();
    }

    /// Serializes the env-mutating `from_env` test against any future
    /// env-mutating unit test in this module.
    static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

    #[test]
    fn shared_runner_is_sync_across_threads() {
        // The RefCell->Mutex change exists so one gated runner can be shared
        // across threads: `inbox` fans its provider queries out via
        // `thread::scope` and requires `R: BackendRunner + Sync`. Prove the
        // shared runner gates correctly under a concurrent fan-out — every call
        // succeeds, and the cold-cache burst issues at most one probe per thread
        // (bounded, not runaway; there is no single-flight coordination).
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct SyncFake {
            probes: AtomicUsize,
            reals: AtomicUsize,
        }
        impl BackendRunner for SyncFake {
            fn run(&self, call: &BackendCall) -> Result<BackendSuccess, ForgeError> {
                let argv: Vec<String> = call
                    .argv
                    .iter()
                    .map(|a| a.to_string_lossy().into())
                    .collect();
                if argv == ["api", "rate_limit"] {
                    self.probes.fetch_add(1, Ordering::SeqCst);
                    return Ok(BackendSuccess {
                        stdout:
                            r#"{"resources":{"graphql":{"limit":5000,"remaining":5000,"reset":1}}}"#
                                .into(),
                        stderr: String::new(),
                    });
                }
                self.reals.fetch_add(1, Ordering::SeqCst);
                Ok(BackendSuccess {
                    stdout: "ok".into(),
                    stderr: String::new(),
                })
            }
        }

        const THREADS: usize = 8;
        let fake = SyncFake {
            probes: AtomicUsize::new(0),
            reals: AtomicUsize::new(0),
        };
        // SystemClock never sleeps here: the probed budget is always healthy.
        let gate = RateLimitedRunner::new(&fake, SystemClock, cfg());

        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                scope.spawn(|| {
                    gate.run(&pr_ready_call()).expect("gated call in thread");
                });
            }
        });

        assert_eq!(fake.reals.load(Ordering::SeqCst), THREADS, "every call ran");
        let probes = fake.probes.load(Ordering::SeqCst);
        assert!(
            (1..=THREADS).contains(&probes),
            "cold-cache fan-out issues 1..=THREADS probes, got {probes}"
        );
    }
}

/// Structural guard: dispatch/op entrypoints must build their live runner via
/// [`default_runner`], never a bare [`ProcessRunner`]. This is what keeps the
/// GraphQL rate-limit gate wired to *every* op — the classifier's breadth and
/// the wiring cannot drift, because there is only one place a runner is built
/// (sympoies/nils-cli#1063).
///
/// This is a textual guard, not an AST check: it verifies the *absence* of the
/// `ProcessRunner` token (a necessary condition for routing through the
/// factory), not the *presence* of a `default_runner()` call. It cannot catch
/// every conceivable bypass (e.g. a hand-rolled `std::process::Command` or a
/// disabled-config runner), but it does catch the realistic regression — an op
/// reaching for the bare runner the way every op used to.
#[cfg(test)]
mod wiring_guard {
    use std::fs;
    use std::path::Path;

    /// Directories that hold live-dispatching `run()` entrypoints. A bare
    /// `ProcessRunner` here would silently bypass the rate-limit gate. Extend
    /// this list (or `GATED_FILES`) whenever live dispatch grows a new home.
    const GATED_DIRS: &[&str] = &["src/ops", "src/macros"];

    /// Individual files (outside the gated dirs) that also dispatch live calls
    /// — chiefly the CLI dispatcher, which routes some paths directly.
    const GATED_FILES: &[&str] = &["src/cli.rs"];

    fn rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                rs_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// Drop line and trailing `//` comments so a doc-comment or inline mention
    /// of `ProcessRunner` does not trip the guard — only code text is scanned.
    fn strip_comments(body: &str) -> String {
        body.lines()
            .map(|line| line.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn ops_construct_runner_via_factory() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        for rel in GATED_DIRS {
            rs_files(&root.join(rel), &mut files);
        }
        files.extend(GATED_FILES.iter().map(|rel| root.join(rel)));

        let mut offenders = Vec::new();
        for file in files {
            let body = fs::read_to_string(&file).expect("read dispatch/op source");
            if strip_comments(&body).contains("ProcessRunner") {
                offenders.push(
                    file.strip_prefix(root)
                        .unwrap_or(&file)
                        .display()
                        .to_string(),
                );
            }
        }
        offenders.sort();
        assert!(
            offenders.is_empty(),
            "dispatch/op entrypoints must build the live runner via \
             `crate::rate_limit::default_runner()`, not a bare `ProcessRunner`, \
             so the GraphQL rate-limit gate stays wired to every op. (Comments \
             are ignored; a match is a real construction in code, including in a \
             `#[cfg(test)]` module — route it through the factory or move the \
             test off the bare runner.) Offending files: {offenders:?}"
        );
    }
}
