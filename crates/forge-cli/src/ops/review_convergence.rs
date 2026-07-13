//! Opt-in native-review convergence for the final PR merge window.

use std::time::{Duration, Instant};
use std::{cmp::Ordering, collections::BTreeMap};

use jiff::{SignedDuration, Timestamp};
use serde::Serialize;

use crate::backend::BackendRunner;
use crate::cli::BINARY;
use crate::config::{ReviewBotMode, ReviewConvergenceBot, ReviewConvergencePolicy};
use crate::error::ForgeError;
use crate::ops::pr_reviews::{NativeReviewSummary, PrReviewsPayload, compute_for_pr_with_timeout};
use crate::ops::pr_wait_checks::Clock;
use crate::provider::ProviderContext;

const POLL_INTERVAL: Duration = Duration::from_secs(10);

/// Final review snapshot carried by a successful merge payload. The existing
/// review-thread gate fills `unresolved_threads` immediately after this native
/// review convergence step and immediately before the provider merge.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReviewConvergenceSnapshot {
    pub required: bool,
    pub head_sha: String,
    pub observed_reviews: Vec<NativeReviewSummary>,
    pub stale_reviews: Vec<NativeReviewSummary>,
    pub unresolved_threads: usize,
    pub changes_requested_by: Vec<String>,
    pub missing_reviewers: Vec<String>,
    pub latest_activity_at: Option<String>,
    pub quiet_until: Option<String>,
    pub quiet_period_ms: u64,
    pub timeout_ms: u64,
    pub waited_ms: u64,
    pub bots: Vec<ReviewConvergenceBot>,
}

pub fn converge<R: BackendRunner, C: Clock>(
    runner: &R,
    clock: &C,
    ctx: &ProviderContext,
    number: u64,
    pr_url: &str,
    expected_head_sha: Option<&str>,
    policy: &ReviewConvergencePolicy,
) -> Result<ReviewConvergenceSnapshot, ForgeError> {
    let started = clock.now();
    let deadline = started.checked_add(policy.timeout).unwrap_or(started);
    let mut quiet_started: Option<Instant> = None;
    let mut quiet_until: Option<String> = None;
    let mut last_fingerprint: Option<Vec<String>> = None;

    loop {
        let remaining = deadline.saturating_duration_since(clock.now());
        if remaining.is_zero() {
            return Err(timeout_error(policy, expected_head_sha));
        }
        let reviews = compute_for_pr_with_timeout(runner, ctx, number, pr_url, Some(remaining))
            .map_err(|err| map_provider_timeout(err, policy, expected_head_sha))?;
        ensure_expected_head(expected_head_sha, &reviews)?;
        let mut snapshot = build_snapshot(policy, reviews, elapsed_ms(started, clock.now()));

        if clock.now() >= deadline {
            return Err(timeout_error(policy, Some(&snapshot.head_sha)));
        }

        if !snapshot.changes_requested_by.is_empty() {
            return Err(changes_requested_error(&snapshot));
        }

        // `observed` is intentionally absence-tolerant: no configured bot
        // review means no wait, no missing reviewer, and no timeout budget is
        // consumed merely to discover whether a bot might run later.
        if snapshot.observed_reviews.is_empty() || policy.quiet_period.is_zero() {
            return Ok(snapshot);
        }

        let fingerprint = activity_fingerprint(&snapshot.observed_reviews);
        let now = clock.now();
        if last_fingerprint.as_ref() != Some(&fingerprint) {
            quiet_started = Some(now);
            quiet_until = wall_clock_quiet_until(policy.quiet_period);
            last_fingerprint = Some(fingerprint);
        }
        snapshot.quiet_until = quiet_until.clone();

        let quiet_elapsed = quiet_started
            .map(|quiet| now.saturating_duration_since(quiet))
            .unwrap_or_default();
        if quiet_elapsed >= policy.quiet_period {
            snapshot.waited_ms = elapsed_ms(started, now);
            return Ok(snapshot);
        }
        if now >= deadline {
            return Err(timeout_error(policy, Some(&snapshot.head_sha)));
        }

        let quiet_remaining = policy.quiet_period.saturating_sub(quiet_elapsed);
        let timeout_remaining = deadline.saturating_duration_since(now);
        clock.sleep(POLL_INTERVAL.min(quiet_remaining).min(timeout_remaining));
    }
}

/// Re-read native reviews after slower thread/task gates and immediately
/// before provider merge. A changed observed-review fingerprint forces the
/// caller to retry convergence; a late native CHANGES_REQUESTED remains
/// mechanically blocking.
pub fn recheck_before_merge<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    number: u64,
    pr_url: &str,
    expected_head_sha: Option<&str>,
    policy: &ReviewConvergencePolicy,
    previous: &ReviewConvergenceSnapshot,
) -> Result<ReviewConvergenceSnapshot, ForgeError> {
    let reviews = compute_for_pr_with_timeout(runner, ctx, number, pr_url, Some(policy.timeout))
        .map_err(|err| map_provider_timeout(err, policy, expected_head_sha))?;
    ensure_expected_head(expected_head_sha, &reviews)?;
    let mut snapshot = build_snapshot(policy, reviews, previous.waited_ms);
    if !snapshot.changes_requested_by.is_empty() {
        return Err(changes_requested_error(&snapshot));
    }
    if activity_fingerprint(&snapshot.observed_reviews)
        != activity_fingerprint(&previous.observed_reviews)
    {
        return Err(ForgeError::validation(
            schema_err(),
            "review_convergence_activity_changed",
            "native review activity changed after convergence and before merge",
            Some(format!("head_sha={}", snapshot.head_sha)),
        ));
    }
    snapshot.quiet_until = previous.quiet_until.clone();
    snapshot.latest_activity_at = previous.latest_activity_at.clone();
    Ok(snapshot)
}

fn build_snapshot(
    policy: &ReviewConvergencePolicy,
    reviews: PrReviewsPayload,
    waited_ms: u64,
) -> ReviewConvergenceSnapshot {
    let observed_reviews = reviews
        .current_head_reviews
        .iter()
        .filter(|review| is_observed_login(&review.author, &policy.bots))
        .cloned()
        .collect::<Vec<_>>();
    let stale_reviews = reviews
        .stale_reviews
        .into_iter()
        .filter(|review| is_observed_login(&review.author, &policy.bots))
        .collect::<Vec<_>>();
    let changes_requested_by = effective_changes_requested_by(&reviews.current_head_reviews);
    let latest_activity_at = observed_reviews
        .iter()
        .map(|review| review.submitted_at.as_str())
        .filter(|submitted| !submitted.is_empty())
        .max()
        .map(str::to_string);

    ReviewConvergenceSnapshot {
        required: policy.require,
        head_sha: reviews.head_sha,
        observed_reviews,
        stale_reviews,
        unresolved_threads: 0,
        changes_requested_by,
        missing_reviewers: Vec::new(),
        latest_activity_at,
        quiet_until: None,
        quiet_period_ms: duration_ms(policy.quiet_period),
        timeout_ms: duration_ms(policy.timeout),
        waited_ms,
        bots: policy.bots.clone(),
    }
}

/// Compute each reviewer's current opinionated state on this head. COMMENTED
/// summaries remain observable evidence but do not clear an earlier native
/// request for changes; a later APPROVED or DISMISSED state does. This mirrors
/// GitHub's "latest opinionated review per user" semantics without hiding the
/// complete bounded review evidence from the standalone read surface.
fn effective_changes_requested_by(reviews: &[NativeReviewSummary]) -> Vec<String> {
    let mut latest = BTreeMap::<String, &NativeReviewSummary>::new();
    for review in reviews {
        if !matches!(
            review.state.as_str(),
            "CHANGES_REQUESTED" | "APPROVED" | "DISMISSED"
        ) {
            continue;
        }
        let identity = review_identity(review);
        let replace = latest
            .get(&identity)
            .is_none_or(|current| review_order(review, current) == Ordering::Greater);
        if replace {
            latest.insert(identity, review);
        }
    }
    latest
        .into_values()
        .filter(|review| review.state.eq_ignore_ascii_case("CHANGES_REQUESTED"))
        .map(review_display_identity)
        .collect()
}

fn review_identity(review: &NativeReviewSummary) -> String {
    let login = normalized_login(&review.author);
    if login.is_empty() {
        format!("review:{}", review.id)
    } else {
        login
    }
}

fn review_display_identity(review: &NativeReviewSummary) -> String {
    if review.author.trim().is_empty() {
        format!("review:{}", review.id)
    } else {
        review.author.clone()
    }
}

fn review_order(left: &NativeReviewSummary, right: &NativeReviewSummary) -> Ordering {
    left.submitted_at
        .cmp(&right.submitted_at)
        .then_with(|| left.database_id.cmp(&right.database_id))
        .then_with(|| left.id.cmp(&right.id))
}

fn is_observed_login(author: &str, bots: &[ReviewConvergenceBot]) -> bool {
    let author = normalized_login(author);
    bots.iter()
        .any(|bot| bot.mode == ReviewBotMode::Observed && normalized_login(&bot.login) == author)
}

fn normalized_login(login: &str) -> String {
    login
        .trim()
        .strip_suffix("[bot]")
        .unwrap_or(login.trim())
        .to_ascii_lowercase()
}

fn activity_fingerprint(reviews: &[NativeReviewSummary]) -> Vec<String> {
    let mut fingerprint = reviews
        .iter()
        .map(|review| {
            format!(
                "{}:{}:{}:{}:{}:{}",
                review.id,
                review.state,
                review.commit_sha.as_deref().unwrap_or(""),
                review.submitted_at,
                review.summary_truncated,
                review.summary,
            )
        })
        .collect::<Vec<_>>();
    fingerprint.sort();
    fingerprint
}

fn changes_requested_error(snapshot: &ReviewConvergenceSnapshot) -> ForgeError {
    ForgeError::validation(
        schema_err(),
        "review_changes_requested",
        "one or more current-head native reviews request changes",
        Some(format!(
            "changes_requested_by={}",
            snapshot.changes_requested_by.join(",")
        )),
    )
}

fn timeout_error(policy: &ReviewConvergencePolicy, expected_head_sha: Option<&str>) -> ForgeError {
    ForgeError::unavailable(
        schema_err(),
        "review_convergence_timeout",
        "native review convergence exceeded the configured timeout",
        Some(format!(
            "head_sha={}; quiet_period_ms={}; timeout_ms={}",
            expected_head_sha.unwrap_or("<unknown>"),
            duration_ms(policy.quiet_period),
            duration_ms(policy.timeout)
        )),
    )
}

fn map_provider_timeout(
    err: ForgeError,
    policy: &ReviewConvergencePolicy,
    expected_head_sha: Option<&str>,
) -> ForgeError {
    if err.kind() == "backend_timeout" {
        timeout_error(policy, expected_head_sha)
    } else {
        err
    }
}

fn ensure_expected_head(
    expected_head_sha: Option<&str>,
    reviews: &PrReviewsPayload,
) -> Result<(), ForgeError> {
    if expected_head_sha.is_none_or(|expected| expected == reviews.head_sha) {
        return Ok(());
    }
    Err(ForgeError::validation(
        schema_err(),
        "review_convergence_head_changed",
        "the provider PR head changed during review convergence",
        Some(format!(
            "expected_head={} provider_head={}",
            expected_head_sha.unwrap_or("<missing>"),
            reviews.head_sha
        )),
    ))
}

fn wall_clock_quiet_until(duration: Duration) -> Option<String> {
    let millis = i64::try_from(duration.as_millis()).ok()?;
    Timestamp::now()
        .checked_add(SignedDuration::from_millis(millis))
        .ok()
        .map(|timestamp| timestamp.to_string())
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn elapsed_ms(start: Instant, end: Instant) -> u64 {
    duration_ms(end.saturating_duration_since(start))
}

fn schema_err() -> String {
    nils_common::cli_contract::schema_version_for(BINARY, "error", 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendCall, BackendSuccess};
    use crate::config::ReviewConvergenceBot;
    use crate::provider::{DetectionSource, Provider};
    use pretty_assertions::assert_eq;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    struct QueueRunner {
        outputs: RefCell<VecDeque<String>>,
        calls: Cell<usize>,
    }

    impl QueueRunner {
        fn new(outputs: impl IntoIterator<Item = String>) -> Self {
            Self {
                outputs: RefCell::new(outputs.into_iter().collect()),
                calls: Cell::new(0),
            }
        }
    }

    impl BackendRunner for QueueRunner {
        fn run(&self, _call: &BackendCall) -> Result<BackendSuccess, ForgeError> {
            self.calls.set(self.calls.get() + 1);
            let mut outputs = self.outputs.borrow_mut();
            let value = if outputs.len() > 1 {
                outputs.pop_front().expect("queued review output")
            } else {
                outputs.front().cloned().expect("queued review output")
            };
            Ok(BackendSuccess {
                stdout: value,
                stderr: String::new(),
            })
        }
    }

    struct StepClock {
        start: Instant,
        elapsed: Cell<Duration>,
    }

    impl StepClock {
        fn new() -> Self {
            Self {
                start: Instant::now(),
                elapsed: Cell::new(Duration::ZERO),
            }
        }
    }

    impl Clock for StepClock {
        fn now(&self) -> Instant {
            self.start + self.elapsed.get()
        }

        fn sleep(&self, duration: Duration) {
            self.elapsed.set(self.elapsed.get() + duration);
        }
    }

    struct SlowRunner<'a> {
        clock: &'a StepClock,
        delay: Duration,
        output: String,
        plain_calls: Cell<usize>,
        timeout_calls: Cell<usize>,
    }

    impl BackendRunner for SlowRunner<'_> {
        fn run(&self, _call: &BackendCall) -> Result<BackendSuccess, ForgeError> {
            self.plain_calls.set(self.plain_calls.get() + 1);
            self.clock.sleep(self.delay);
            Ok(BackendSuccess {
                stdout: self.output.clone(),
                stderr: String::new(),
            })
        }

        fn run_with_timeout(
            &self,
            _call: &BackendCall,
            timeout: Option<Duration>,
        ) -> Result<BackendSuccess, ForgeError> {
            self.timeout_calls.set(self.timeout_calls.get() + 1);
            let budget = timeout.unwrap_or(self.delay);
            self.clock.sleep(self.delay.min(budget));
            if self.delay > budget {
                return Err(ForgeError::unavailable(
                    schema_err(),
                    "backend_timeout",
                    "simulated provider timeout",
                    None,
                ));
            }
            Ok(BackendSuccess {
                stdout: self.output.clone(),
                stderr: String::new(),
            })
        }
    }

    fn ctx() -> ProviderContext {
        ProviderContext {
            provider: Provider::GitHub,
            host: "github.com".into(),
            source: DetectionSource::Flag,
            repo: Some("acme/widgets".into()),
        }
    }

    fn policy(quiet_secs: u64, timeout_secs: u64) -> ReviewConvergencePolicy {
        ReviewConvergencePolicy {
            require: true,
            quiet_period: Duration::from_secs(quiet_secs),
            timeout: Duration::from_secs(timeout_secs),
            bots: vec![ReviewConvergenceBot {
                login: "example-review-bot".into(),
                mode: ReviewBotMode::Observed,
            }],
        }
    }

    fn reviews_json(head: &str, reviews: &str) -> String {
        format!(
            r#"{{"data":{{"repository":{{"pullRequest":{{"headRefOid":"{head}","reviews":{{"nodes":[{reviews}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}"#
        )
    }

    fn review(id: &str, state: &str, submitted_at: &str) -> String {
        format!(
            r#"{{"id":"{id}","databaseId":1,"url":"https://github.com/acme/widgets/pull/7#pullrequestreview-1","author":{{"login":"example-review-bot[bot]"}},"state":"{state}","commit":{{"oid":"head"}},"submittedAt":"{submitted_at}","body":"summary"}}"#
        )
    }

    #[test]
    fn app_bot_suffix_matches_plain_config_login() {
        let bots = vec![ReviewConvergenceBot {
            login: "example-review-bot".into(),
            mode: ReviewBotMode::Observed,
        }];
        assert!(is_observed_login("example-review-bot[bot]", &bots));
        assert!(is_observed_login("EXAMPLE-REVIEW-BOT", &bots));
        assert!(!is_observed_login("other-bot[bot]", &bots));
    }

    #[test]
    fn fingerprint_is_order_independent() {
        let review = |id: &str| NativeReviewSummary {
            id: id.into(),
            database_id: None,
            url: String::new(),
            author: "example-review-bot[bot]".into(),
            state: "COMMENTED".into(),
            commit_sha: Some("head".into()),
            submitted_at: "2026-07-14T04:00:00Z".into(),
            summary: String::new(),
            summary_truncated: false,
        };
        assert_eq!(
            activity_fingerprint(&[review("b"), review("a")]),
            activity_fingerprint(&[review("a"), review("b")])
        );
    }

    #[test]
    fn latest_opinionated_state_supersedes_changes_requested_but_comment_does_not() {
        let review = |id: &str, state: &str, submitted_at: &str| NativeReviewSummary {
            id: id.into(),
            database_id: None,
            url: String::new(),
            author: "reviewer".into(),
            state: state.into(),
            commit_sha: Some("head".into()),
            submitted_at: submitted_at.into(),
            summary: String::new(),
            summary_truncated: false,
        };
        let requested = review("a", "CHANGES_REQUESTED", "2026-07-14T04:00:00Z");
        let commented = review("b", "COMMENTED", "2026-07-14T04:01:00Z");
        assert_eq!(
            effective_changes_requested_by(&[requested.clone(), commented]),
            vec!["reviewer"]
        );
        let approved = review("c", "APPROVED", "2026-07-14T04:02:00Z");
        assert!(effective_changes_requested_by(&[requested, approved]).is_empty());
    }

    #[test]
    fn absent_observed_bot_returns_immediately_without_consuming_timeout() {
        let runner = QueueRunner::new([reviews_json("head", "")]);
        let clock = StepClock::new();
        let snapshot = converge(
            &runner,
            &clock,
            &ctx(),
            7,
            "https://github.com/acme/widgets/pull/7",
            Some("head"),
            &policy(120, 1200),
        )
        .expect("absence is not blocking");
        assert!(snapshot.observed_reviews.is_empty());
        assert_eq!(snapshot.waited_ms, 0);
        assert_eq!(runner.calls.get(), 1);
    }

    #[test]
    fn observed_review_waits_for_the_quiet_period() {
        let current = review("PRR_1", "COMMENTED", "2026-07-14T04:00:00Z");
        let runner = QueueRunner::new([reviews_json("head", &current)]);
        let clock = StepClock::new();
        let snapshot = converge(
            &runner,
            &clock,
            &ctx(),
            7,
            "https://github.com/acme/widgets/pull/7",
            Some("head"),
            &policy(2, 20),
        )
        .expect("quiet convergence");
        assert_eq!(snapshot.waited_ms, 2000);
        assert_eq!(runner.calls.get(), 2);
        assert!(snapshot.quiet_until.is_some());
    }

    #[test]
    fn default_quiet_window_bounds_provider_poll_count() {
        let current = review("PRR_1", "COMMENTED", "2026-07-14T04:00:00Z");
        let runner = QueueRunner::new([reviews_json("head", &current)]);
        let snapshot = converge(
            &runner,
            &StepClock::new(),
            &ctx(),
            7,
            "https://github.com/acme/widgets/pull/7",
            Some("head"),
            &policy(120, 1200),
        )
        .expect("quiet convergence");
        assert_eq!(snapshot.waited_ms, 120_000);
        assert!(runner.calls.get() <= 13, "calls={}", runner.calls.get());
    }

    #[test]
    fn new_observed_activity_restarts_the_quiet_period() {
        let first = review("PRR_1", "COMMENTED", "2026-07-14T04:00:00Z");
        let second = review("PRR_2", "COMMENTED", "2026-07-14T04:00:01Z");
        let both = format!("{first},{second}");
        let runner = QueueRunner::new([
            reviews_json("head", &first),
            reviews_json("head", &both),
            reviews_json("head", &both),
            reviews_json("head", &both),
        ]);
        let clock = StepClock::new();
        let snapshot = converge(
            &runner,
            &clock,
            &ctx(),
            7,
            "https://github.com/acme/widgets/pull/7",
            Some("head"),
            &policy(2, 20),
        )
        .expect("activity settles");
        assert_eq!(snapshot.waited_ms, 4000);
        assert_eq!(snapshot.observed_reviews.len(), 2);
    }

    #[test]
    fn active_observed_wait_times_out_deterministically() {
        let current = review("PRR_1", "COMMENTED", "2026-07-14T04:00:00Z");
        let runner = QueueRunner::new([reviews_json("head", &current)]);
        let clock = StepClock::new();
        let err = converge(
            &runner,
            &clock,
            &ctx(),
            7,
            "https://github.com/acme/widgets/pull/7",
            Some("head"),
            &policy(3, 2),
        )
        .expect_err("must time out");
        assert_eq!(err.kind(), "review_convergence_timeout");
        assert_eq!(err.exit_code(), 69);
    }

    #[test]
    fn convergence_timeout_is_forwarded_to_each_provider_call() {
        let clock = StepClock::new();
        let runner = SlowRunner {
            clock: &clock,
            delay: Duration::from_secs(30),
            output: reviews_json("head", ""),
            plain_calls: Cell::new(0),
            timeout_calls: Cell::new(0),
        };
        let err = converge(
            &runner,
            &clock,
            &ctx(),
            7,
            "https://github.com/acme/widgets/pull/7",
            Some("head"),
            &policy(2, 20),
        )
        .expect_err("outer timeout must bound the provider call");
        assert_eq!(err.kind(), "review_convergence_timeout");
        assert_eq!(runner.plain_calls.get(), 0);
        assert_eq!(runner.timeout_calls.get(), 1);
    }

    #[test]
    fn current_head_changes_requested_blocks_without_parsing_summary() {
        let current = review("PRR_1", "CHANGES_REQUESTED", "2026-07-14T04:00:00Z");
        let runner = QueueRunner::new([reviews_json("head", &current)]);
        let err = converge(
            &runner,
            &StepClock::new(),
            &ctx(),
            7,
            "https://github.com/acme/widgets/pull/7",
            Some("head"),
            &policy(2, 20),
        )
        .expect_err("native state blocks");
        assert_eq!(err.kind(), "review_changes_requested");
        assert_eq!(err.exit_code(), 65);
    }

    #[test]
    fn unknown_review_state_cannot_clear_an_earlier_change_request() {
        let requested = review("PRR_1", "CHANGES_REQUESTED", "2026-07-14T04:00:00Z");
        let unknown = review("PRR_2", "FUTURE_STATE", "2026-07-14T04:01:00Z");
        let runner = QueueRunner::new([reviews_json("head", &format!("{requested},{unknown}"))]);
        let err = converge(
            &runner,
            &StepClock::new(),
            &ctx(),
            7,
            "https://github.com/acme/widgets/pull/7",
            Some("head"),
            &policy(0, 20),
        )
        .expect_err("unknown provider state must fail closed");
        assert_eq!(err.kind(), "review_snapshot_incomplete");
    }

    #[test]
    fn graphql_partial_errors_fail_the_review_snapshot_closed() {
        let output = serde_json::json!({
            "errors": [{"message": "partial provider response"}],
            "data": {
                "repository": {
                    "pullRequest": {
                        "headRefOid": "head",
                        "reviews": {
                            "nodes": [],
                            "pageInfo": {"hasNextPage": false, "endCursor": null}
                        }
                    }
                }
            }
        })
        .to_string();
        let err = converge(
            &QueueRunner::new([output]),
            &StepClock::new(),
            &ctx(),
            7,
            "https://github.com/acme/widgets/pull/7",
            Some("head"),
            &policy(0, 20),
        )
        .expect_err("partial GraphQL data must not authorize merge");
        assert_eq!(err.kind(), "review_snapshot_incomplete");
    }

    #[test]
    fn authorless_current_head_changes_requested_still_blocks() {
        let current = r#"{"id":"PRR_deleted","databaseId":2,"url":"https://github.com/acme/widgets/pull/7#pullrequestreview-2","author":null,"state":"CHANGES_REQUESTED","commit":{"oid":"head"},"submittedAt":"2026-07-14T04:00:00Z","body":"summary"}"#;
        let runner = QueueRunner::new([reviews_json("head", current)]);
        let err = converge(
            &runner,
            &StepClock::new(),
            &ctx(),
            7,
            "https://github.com/acme/widgets/pull/7",
            Some("head"),
            &policy(2, 20),
        )
        .expect_err("native state blocks even when the author was deleted");
        assert_eq!(err.kind(), "review_changes_requested");
        assert!(
            err.detail()
                .is_some_and(|detail| detail.contains("review:PRR_deleted"))
        );
    }

    #[test]
    fn head_change_during_convergence_fails_closed() {
        let current = review("PRR_1", "COMMENTED", "2026-07-14T04:00:00Z")
            .replace(r#""oid":"head""#, r#""oid":"old-head""#);
        let runner = QueueRunner::new([
            reviews_json("old-head", &current),
            reviews_json("new-head", &current),
        ]);
        let err = converge(
            &runner,
            &StepClock::new(),
            &ctx(),
            7,
            "https://github.com/acme/widgets/pull/7",
            Some("old-head"),
            &policy(2, 20),
        )
        .expect_err("head changed");
        assert_eq!(err.kind(), "review_convergence_head_changed");
        assert_eq!(runner.calls.get(), 2);
    }
}
