//! `pr wait-checks` atom — blocking poll on top of `pr.checks` until every
//! required check reaches a terminal state.
//!
//! Spec / ops: shares schema `cli.forge-cli.pr.checks.v1` with the snapshot
//! atom (the `data.duration_ms` field is populated here; the snapshot leaves
//! it `None`). Exit-code matrix per spec §"pr wait-checks":
//!
//! - all required `success` → `SUCCESS 0`
//! - any required `failure` / `cancelled` / `timed_out` → `RUNTIME 1`,
//!   `error.kind = "checks_failed"`
//! - deadline reached → `UNAVAILABLE 69`, `error.kind = "checks_timeout"`
//!
//! Polling uses `std::thread::sleep`; the implementation owns the clock
//! through the [`Clock`] trait so tests can drive deterministic snapshot
//! sequences.

use std::time::{Duration, Instant};

use nils_common::cli_contract::{Envelope, EnvelopeError, OutputFormat, schema_version_for};
use serde_json::json;

use crate::backend::{BackendRunner, DryRunPayload, ProcessRunner};
use crate::cli::{BINARY, GlobalFlags, PrChecksArgs, PrWaitChecksArgs};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::pr_checks::{self, PrChecksPayload, SCHEMA, SCHEMA_VERSION};
use crate::provider::{ProviderContext, detect, git_remote_url};

/// Trait abstracting `now()` and `sleep` so tests can step time without
/// `std::thread::sleep`.
pub trait Clock {
    fn now(&self) -> Instant;
    fn sleep(&self, dur: Duration);
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn sleep(&self, dur: Duration) {
        std::thread::sleep(dur);
    }
}

pub fn run(
    global: &GlobalFlags,
    args: PrWaitChecksArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = ProcessRunner;
    let clock = SystemClock;
    run_with(&runner, &clock, global, &args, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, C: Clock, F: Fn(&str) -> Option<String>>(
    runner: &R,
    clock: &C,
    global: &GlobalFlags,
    args: &PrWaitChecksArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect(global.provider_hint(), &global.remote, remote_url_lookup)?;
    let snapshot_args = PrChecksArgs {
        id: args.id.clone(),
        required_only: args.required_only,
    };
    if global.dry_run {
        return Ok(emit_dry_run(&ctx, args, &snapshot_args, format));
    }

    let timeout = args.timeout;
    let interval = args.interval;
    let start = clock.now();
    let deadline = start + timeout;

    loop {
        let snapshot = pr_checks::snapshot(runner, global, &ctx, &snapshot_args)?;
        let snapshot = with_duration(snapshot, ms_between(start, clock.now()));
        if is_terminal(&snapshot) {
            return finalise(snapshot, format);
        }
        let now = clock.now();
        if now >= deadline {
            let timed_out = with_duration(snapshot, ms_between(start, now));
            return Ok(emit_timeout(timed_out, format));
        }
        let remaining = deadline.saturating_duration_since(now);
        let sleep_for = std::cmp::min(interval, remaining);
        if sleep_for.is_zero() {
            continue;
        }
        clock.sleep(sleep_for);
    }
}

fn ms_between(start: Instant, end: Instant) -> u64 {
    end.duration_since(start).as_millis() as u64
}

fn with_duration(mut snapshot: PrChecksPayload, duration_ms: u64) -> PrChecksPayload {
    snapshot.duration_ms = Some(duration_ms);
    snapshot
}

fn is_terminal(snapshot: &PrChecksPayload) -> bool {
    // Terminal iff no entries are pending in the gating set.
    snapshot.pending.is_empty()
}

fn finalise(snapshot: PrChecksPayload, format: OutputFormat) -> Result<i32, ForgeError> {
    // Distinguish success from failure based on the aggregated state.
    let state = snapshot.state;
    let is_success = state == "success";
    if is_success {
        Ok(emit_success(
            schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
            snapshot,
            format,
            render_text,
        ))
    } else {
        Ok(emit_failure(
            snapshot,
            "checks_failed",
            "required checks did not reach success",
            nils_common::cli_contract::exit::RUNTIME,
            format,
        ))
    }
}

fn emit_timeout(snapshot: PrChecksPayload, format: OutputFormat) -> i32 {
    emit_failure(
        snapshot,
        "checks_timeout",
        "deadline reached before required checks became terminal",
        nils_common::cli_contract::exit::UNAVAILABLE,
        format,
    )
}

/// Emit a failure envelope that *still carries* the snapshot payload so
/// callers can inspect failed/pending lists even though `ok = false`.
fn emit_failure(
    snapshot: PrChecksPayload,
    kind: &'static str,
    message: &str,
    exit_code: i32,
    format: OutputFormat,
) -> i32 {
    let schema = schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION);
    let envelope = Envelope {
        schema_version: schema,
        ok: false,
        data: Some(snapshot.clone()),
        warnings: Vec::new(),
        error: Some(EnvelopeError {
            code: kind.to_string(),
            message: message.to_string(),
            hint: None,
            details: Some(json!({
                "state": snapshot.state,
                "required_count": snapshot.required_count,
                "success_count": snapshot.success_count,
                "duration_ms": snapshot.duration_ms,
            })),
        }),
    };
    match format {
        OutputFormat::Json => {
            let serialized =
                serde_json::to_string(&envelope).unwrap_or_else(|_| String::from("{\"ok\":false}"));
            println!("{serialized}");
        }
        OutputFormat::Text => {
            eprintln!("error: {kind}: {message}");
            render_text(&snapshot);
        }
    }
    exit_code
}

fn emit_dry_run(
    ctx: &ProviderContext,
    wait_args: &PrWaitChecksArgs,
    snapshot_args: &PrChecksArgs,
    format: OutputFormat,
) -> i32 {
    let call = pr_checks::build_dry_run_call(ctx, snapshot_args);
    let payload = DryRunPlan {
        inner: DryRunPayload::new(ctx.provider, &call),
        timeout_ms: wait_args.timeout.as_millis() as u64,
        interval_ms: wait_args.interval.as_millis() as u64,
        required_only: snapshot_args.required_only,
    };
    emit_success(
        schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
        payload,
        format,
        |p| {
            println!(
                "would run: {plan} (interval={interval}ms, timeout={timeout}ms, required_only={required})",
                plan = p.inner.plan.join(" "),
                interval = p.interval_ms,
                timeout = p.timeout_ms,
                required = p.required_only,
            );
        },
    )
}

#[derive(serde::Serialize)]
struct DryRunPlan {
    #[serde(flatten)]
    inner: DryRunPayload,
    timeout_ms: u64,
    interval_ms: u64,
    required_only: bool,
}

#[cfg(test)]
fn timed_out_placeholder(ctx: &ProviderContext) -> PrChecksPayload {
    PrChecksPayload {
        provider: ctx.provider.as_str(),
        state: "pending",
        required_count: 0,
        success_count: 0,
        failed: Vec::new(),
        pending: Vec::new(),
        checks: Vec::new(),
        duration_ms: None,
    }
}

fn render_text(payload: &PrChecksPayload) {
    println!(
        "{state} [{provider}] required={required} success={success} failed={fcount} pending={pcount} elapsed_ms={elapsed}",
        state = payload.state,
        provider = payload.provider,
        required = payload.required_count,
        success = payload.success_count,
        fcount = payload.failed.len(),
        pcount = payload.pending.len(),
        elapsed = payload.duration_ms.unwrap_or(0),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendCall, BackendSuccess};
    use crate::provider::{DetectionSource, Provider};
    use std::cell::RefCell;
    use std::time::Duration;

    fn make_ctx(p: Provider) -> ProviderContext {
        ProviderContext {
            provider: p,
            host: "example.com".into(),
            source: DetectionSource::Flag,
        }
    }

    fn make_global() -> GlobalFlags {
        GlobalFlags {
            format: None,
            remote: "origin".into(),
            provider: Some(crate::cli::ProviderFlag::Github),
            repo: None,
            dry_run: false,
        }
    }

    fn make_args(id: &str) -> PrWaitChecksArgs {
        PrWaitChecksArgs {
            id: id.into(),
            timeout: Duration::from_secs(5),
            interval: Duration::from_millis(1),
            required_only: true,
        }
    }

    struct StubRunner {
        outputs: RefCell<Vec<String>>,
    }

    impl BackendRunner for StubRunner {
        fn run(&self, _call: &BackendCall) -> Result<BackendSuccess, ForgeError> {
            let mut outs = self.outputs.borrow_mut();
            let next = if outs.is_empty() {
                String::new()
            } else {
                outs.remove(0)
            };
            Ok(BackendSuccess {
                stdout: next,
                stderr: String::new(),
            })
        }
    }

    /// Test clock with a manually-advanced now() and a sleep() that advances
    /// the clock instead of actually sleeping.
    struct StepClock {
        now: RefCell<Instant>,
    }

    impl StepClock {
        fn new() -> Self {
            Self {
                now: RefCell::new(Instant::now()),
            }
        }
    }

    impl Clock for StepClock {
        fn now(&self) -> Instant {
            *self.now.borrow()
        }
        fn sleep(&self, dur: Duration) {
            let mut cell = self.now.borrow_mut();
            *cell += dur;
        }
    }

    #[test]
    fn succeeds_when_first_snapshot_is_terminal_success() {
        let runner = StubRunner {
            outputs: RefCell::new(vec![
                r#"[{"name":"build","bucket":"pass","conclusion":"success","isRequired":true}]"#
                    .into(),
            ]),
        };
        let clock = StepClock::new();
        let ctx = make_ctx(Provider::GitHub);
        let global = make_global();
        let args = make_args("1");
        // We can't easily call run_with without re-implementing the provider
        // detection plumb-through; exercise the pieces directly.
        let snapshot_args = PrChecksArgs {
            id: args.id.clone(),
            required_only: args.required_only,
        };
        let snapshot = pr_checks::snapshot(&runner, &global, &ctx, &snapshot_args).unwrap();
        assert_eq!(snapshot.state, "success");
        let snapshot = with_duration(snapshot, ms_between(clock.now(), clock.now()));
        assert!(is_terminal(&snapshot));
    }

    #[test]
    fn pending_then_success_succeeds_on_second_poll() {
        let runner = StubRunner {
            outputs: RefCell::new(vec![
                r#"[{"name":"build","bucket":"pending","isRequired":true}]"#.into(),
                r#"[{"name":"build","bucket":"pass","conclusion":"success","isRequired":true}]"#
                    .into(),
            ]),
        };
        let ctx = make_ctx(Provider::GitHub);
        let global = make_global();
        let args = PrChecksArgs {
            id: "1".into(),
            required_only: true,
        };
        let s1 = pr_checks::snapshot(&runner, &global, &ctx, &args).unwrap();
        assert_eq!(s1.state, "pending");
        assert!(!is_terminal(&s1));
        let s2 = pr_checks::snapshot(&runner, &global, &ctx, &args).unwrap();
        assert_eq!(s2.state, "success");
        assert!(is_terminal(&s2));
    }

    #[test]
    fn step_clock_advances_on_sleep() {
        let clock = StepClock::new();
        let before = clock.now();
        clock.sleep(Duration::from_secs(2));
        let after = clock.now();
        assert_eq!(ms_between(before, after), 2000);
    }

    #[test]
    fn failure_state_marks_terminal_and_finalise_returns_runtime_code() {
        let snapshot = PrChecksPayload {
            provider: "github",
            state: "failure",
            required_count: 1,
            success_count: 0,
            failed: vec![pr_checks::FailedCheck {
                name: "test".into(),
                url: None,
                conclusion: Some("failure".into()),
            }],
            pending: Vec::new(),
            checks: Vec::new(),
            duration_ms: Some(123),
        };
        assert!(is_terminal(&snapshot));
        let code = finalise(snapshot, OutputFormat::Json).unwrap();
        assert_eq!(code, nils_common::cli_contract::exit::RUNTIME);
    }

    #[test]
    fn timed_out_placeholder_has_pending_state() {
        let p = timed_out_placeholder(&make_ctx(Provider::GitHub));
        assert_eq!(p.state, "pending");
    }
}
