# forge-cli pr deliver preflight & DX — Source

| Field              | Value                                                                                                                                                                                                                                                                   |
| ------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Status             | Ready for plan generation                                                                                                                                                                                                                                               |
| Date               | 2026-05-30                                                                                                                                                                                                                                                              |
| Source             | Discussion 2026-05-30: delivering a docs PR via `forge-cli pr deliver` cost two avoidable round-trips — `--dry-run` reported `ok` for a body the real run rejected, then the branch had to be pushed manually after `head_not_pushed`. Root-caused in forge-cli source. |
| Intended next step | Generate a plan to make `pr deliver --dry-run` run the non-mutating local preflight rule-set and report verdicts additively, plus two DX cleanups (aggregate body-validation errors; align / cross-reference the pr-body scaffold).                                     |

## Purpose

`forge-cli pr deliver` is an "open draft -> CI green -> ready -> merge"
macro guarded by a set of local validation rules (branch, kind, body
sections, title, worktree-clean, head-pushed). Its `--dry-run` renders
the planned backend argv but executes none of those rules, so a dry-run
can report success for a delivery the real run will reject. In practice
this cost two extra round-trips (a body missing `## Summary` /
`## Test plan`, then an unpushed branch) that a faithful dry-run would
have surfaced together. This document captures the confirmed root cause
and the agreed improvements.

## Confirmed facts

- `pr deliver` short-circuits dry-run before any validation:
  `crates/forge-cli/src/macros/pr_deliver.rs:109-110` returns
  `emit_dry_run(...)` immediately when `global.dry_run` is set, and
  `emit_dry_run` (`pr_deliver.rs:330`) only enumerates `plan_steps[]` —
  it runs no validation.
- The local validation rule-set lives in
  `crates/forge-cli/src/validations.rs` and is entirely non-mutating
  (string inspection + local `git` queries):
  - Rule 1a `branch_name` (`:126`), Rule 1b `branch_kind_matches`
    (`:194`).
  - Rule 2a `body_summary` (`:256`, error code `body_missing_summary`),
    Rule 2b `body_test_plan` (`:273`, error code
    `body_missing_test_plan`).
  - Rule 3 `title_length` (`:225`, `<= 70` codepoints).
  - Rule 4 `worktree_clean` (`:327`), Rule 5 `head_pushed` (`:349`,
    error code `head_not_pushed`).
- Body rules are fail-fast: `body_summary` and `body_test_plan` are
  independent `Result<(), ForgeError>` functions, each returning on the
  first miss. A caller missing both sections sees `body_missing_summary`,
  fixes it, then sees `body_missing_test_plan` — two iterations.
  Observed directly in the 2026-05-30 delivery.
- Required body headings default to `## Summary` and `## Test plan` and
  are configurable (`crates/forge-cli/src/config.rs:178-191`,
  `resolve_summary_heading` / `resolve_test_plan_heading`).
- `forge-cli pr deliver --kind` accepts
  `feature|bug|chore|docs|ci|refactor`
  (`crates/forge-cli/src/cli.rs:180-187`).
- The body scaffold `agent-runtime pr-body render` accepts only
  `--kind feature|bug` and requires `--summary-file` /
  `--test-first-file` / `--test-plan-file`; it lives under the
  `agent-runtime` binary, not `forge-cli`. So a `docs` / `chore` / `ci`
  / `refactor` delivery has no scaffold that emits a matching valid
  body, and a forge-cli user is unlikely to discover the scaffold.
- `--dry-run` is documented as "Render the backend command that would
  run, without invoking it"; its contract is plan rendering, so adding
  local-validation verdicts must be additive and must not abort the
  dry-run.
- `head_not_pushed` is by design: `pr deliver` requires the head branch
  already pushed so the operator owns the push and its pre-push gate
  (in `agent-runtime-kit` that push runs `ci/all.sh`).

## Decisions (locked at this source doc)

1. Make `pr deliver --dry-run` execute the non-mutating local rule-set
   (Rules 1a-5) and report each rule's pass/fail verdict in the dry-run
   envelope, additively alongside `plan_steps[]`. Dry-run still never
   invokes provider-mutating backend steps (create / ready / merge) and
   never aborts on a failed local rule — it reports.
2. Aggregate body-section validation: when both `## Summary` and
   `## Test plan` are missing, report a single error enumerating all
   missing required sections (e.g. `details: ["## Summary",
   "## Test plan"]`) instead of failing on the first.
3. Improve scaffold parity and discoverability: align
   `agent-runtime pr-body render --kind` with the six `pr deliver`
   kinds (or make the Summary / Test-plan skeleton kind-agnostic), and
   reference the scaffold from the `body_missing_*` error `details`.
4. Do NOT change the push contract: `pr deliver` keeps requiring a
   pushed head (`head_not_pushed` stays). Decision 1 already surfaces an
   unpushed head during dry-run. An opt-in `--push` flag is out of scope
   for this plan (possible later) and is not a change to the default
   behavior.

## Scope

- `pr deliver` dry-run path: run and report the local rule-set verdicts.
- forge-cli body validation: aggregate missing-section errors.
- `agent-runtime pr-body` scaffold: kind parity + error cross-reference.

## Non-scope

- Changing the default push contract or auto-pushing the head branch.
- Any provider-mutating behavior in dry-run.
- Reworking the `wait_checks` / merge logic.
- Changing the required heading defaults (`## Summary` / `## Test plan`).

## Implementation boundaries

- Dry-run must remain provider-read-only: it may run local `git` /
  string validations but must not call `gh pr create` / merge backends.
- Preserve existing error codes (`body_missing_summary`,
  `body_missing_test_plan`, `head_not_pushed`). The aggregated body
  error must keep per-section codes discoverable (combined code plus
  `details`, or first code plus `details`) so existing callers / tests
  that match on codes do not silently break.
- No new third-party dependency (preserve `third-party-artifacts` and
  `Cargo.lock` locked-build gates).

## Requirements

- `forge-cli pr deliver --dry-run` reports, for the current invocation,
  the pass / fail of branch, kind-vs-branch, title length, body
  `## Summary`, body `## Test plan`, worktree-clean, and head-pushed —
  without mutating the provider.
- A body missing both required sections yields one error enumerating
  both.
- `agent-runtime pr-body render` can scaffold a valid body for every
  kind `pr deliver` accepts, and the `body_missing_*` error points the
  caller at the scaffold.

## Acceptance criteria

- `pr deliver --dry-run --format json` includes a local-preflight
  verdict block; for a body lacking both sections AND an unpushed
  branch, the single dry-run reports both as failed.
- A real `pr deliver` with a body missing both sections returns one
  aggregated error listing `## Summary` and `## Test plan`.
- `agent-runtime pr-body render --kind docs|chore|ci|refactor` succeeds
  and emits a forge-cli-valid body.
- DEVELOPMENT.md required checks plus the four CI gates pass; existing
  forge-cli validation tests stay green (error codes preserved).

## Validation plan

- `cargo test -p forge-cli` (validation rules, deliver-macro dry-run,
  body aggregation, dry-run-issues-no-backend-call regression).
- `cargo test -p agent-runtime-cli` (pr-body render kinds).
- Manual: `pr deliver --dry-run` on a branch with a bad body and an
  unpushed head -> confirm both surface in one run; then exercise a real
  deliver path.
- Full DEVELOPMENT.md required checks (fmt, clippy,
  `completion-asset-audit`, `third-party-artifacts`, `Cargo.lock`
  locked-build) before PR.

## Findings

| Priority | Issue | Evidence | Fix location | Acceptance |
| --- | --- | --- | --- | --- |
| HIGH | `--dry-run` runs no validation, so it reports `ok` for a delivery the real run rejects | `pr_deliver.rs:109-110` early-returns `emit_dry_run`; `validations.rs:126-364` rules run only in the real path | `macros/pr_deliver.rs` (dry-run path) + reuse `validations.rs` | dry-run reports Rule 1a-5 verdicts; no backend call |
| MED | Body validation is fail-fast across two sections -> two round-trips | `validations.rs:256` / `:273` independent `Err`-on-first | `validations.rs` (aggregate wrapper) + create-atom call site | one error lists all missing sections |
| MED | Scaffold kind subset + wrong binary: `pr-body` is feature/bug-only and under `agent-runtime` | `cli.rs:180-187` (6 deliver kinds) vs `agent-runtime pr-body render --kind feature|bug` | `agent-runtime-cli` pr-body + `body_missing_*` error `details` | scaffold covers all deliver kinds; error points to it |

## Risks and guardrails

- Backward-compat of error shape (Decision 2 / boundary): aggregating
  may change the first-returned code. Mitigate by keeping per-section
  codes in `details` and pinning with a regression test.
- Dry-run scope creep: keep it strictly local / read-only; guard with a
  test asserting dry-run issues no provider backend call.
- Scaffold kind expansion (Decision 3) must not regress the existing
  feature / bug templates.

## Execution

- Recommended plan: docs/plans/forge-cli-pr-deliver-preflight-dx/forge-cli-pr-deliver-preflight-dx-plan.md
- Recommended execution state: docs/plans/forge-cli-pr-deliver-preflight-dx/forge-cli-pr-deliver-preflight-dx-execution-state.md
- Status: ready for plan generation; to be tracked by a plan-tracking issue.
- Next-task source: this document.

## Retention intent

- Plan-scoped. Clean up `docs/plans/forge-cli-pr-deliver-preflight-dx/`
  after execution lands and the PR merges, unless promoted into a
  forge-cli runbook.

## Read-first references

- `crates/forge-cli/src/macros/pr_deliver.rs` (dry-run early return;
  deliver step sequence; `emit_dry_run`).
- `crates/forge-cli/src/validations.rs` (Rules 1a-5; body / worktree /
  head checks).
- `crates/forge-cli/src/config.rs:178-191` (configurable headings).
- `crates/forge-cli/src/cli.rs:180-187` (deliver kinds).
- `DEVELOPMENT.md` (required checks).

## Recommended next artifact

- A plan (`*-plan.md`) sequencing: dry-run preflight verdicts -> body
  error aggregation -> pr-body kind parity + error cross-reference ->
  validation -> PR, tracked via `create-plan-tracking-issue`.
