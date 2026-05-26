# plan-issue closeout `Required` column rendering fix — Source

| Field              | Value                                                                                                                  |
| ------------------ | ---------------------------------------------------------------------------------------------------------------------- |
| Status             | Ready for implementation                                                                                               |
| Date               | 2026-05-26                                                                                                             |
| Source             | sympoies/nils-cli#541 closeout comment review: every linked PR's `Required` column rendered `unknown` despite green CI |
| Intended next step | Open a sympoies/nils-cli issue, then implement the fix in a single small PR                                            |

## Purpose

`plan-issue record close` posts a closeout comment whose linked-PR table
includes a `Required` column. On sympoies/nils-cli — and on any other
repository whose default branch lacks a branch-protection rule with
`required_status_checks` — every PR row renders `Required: unknown` even
when the PR's overall CI is green and the repo has no required checks
defined at all. Observed concretely in
<https://github.com/sympoies/nils-cli/issues/541#issuecomment-4543937296>:
PRs #542–#553 each show `Checks: pass / Required: unknown / Non-required
failures: none`, and PR #554 shows `Checks: none / Required: unknown /
Non-required failures: none`.

This is two bugs stacked, not the cosmetic "missing branch protection"
non-issue the surface presentation suggests:

1. The GitHub adapter's required-check probe issues a `gh` call with an
   invalid `--json` field, so the probe always errors out and the live
   closeout path can never report `required_state` even when the
   provider would have reported one.
2. The closeout-comment renderer collapses every `required_state == None`
   reason into the single label `unknown`, hiding the difference between
   "no required-check rule exists for this branch", "no required checks
   selected in the rule", "probe failed", and "fixture omitted the
   field".

The visible "unknown" is therefore a symptom of bug #1 in the field, and
bug #2 keeps the symptom misleading for anyone debugging.

## Confirmed facts

- The closeout-comment renderer labels `required_state` per-PR via
  `required_state.map(check_status_label).unwrap_or("unknown")` at
  `crates/plan-issue-cli/src/lifecycle_record.rs:2062-2065`. Every `None`
  collapses to the string `unknown`. [F1]
- The fixture-path snapshot reader reads `requiredCheckRollup.{state,count}`
  via `crates/plan-issue-cli/src/execute.rs:561-573`. Fixtures that omit
  `requiredCheckRollup` therefore feed `required_state: None` into the
  renderer. [F2]
- The live GitHub adapter probes required checks in
  `crates/plan-issue-cli/src/github.rs:138-225` (`pr_required_summary`).
  The function shells out to:

  ```text
  gh pr checks <pr> --repo <repo> --required \
    --json bucket,state,conclusion,name
  ```

  and is documented to return `(Some("success"), Some(0), [])` when the
  array is empty (the canonical "no required checks defined" case). [F3]
- The `conclusion` JSON field is not accepted by `gh pr checks` on the
  installed `gh` version. The call exits 5 with `Unknown JSON field:
  "conclusion"` / `Available fields: bucket | completedAt | description
  | event | link | name | startedAt | state`. Reproduced 2026-05-26
  against PRs #542 and #553 on sympoies/nils-cli. [F4]
- Even with a valid `--json` field set, `gh pr checks <pr> --required
  --json name,state` exits 1 with stderr `no required checks reported
  on the '<branch>' branch` and empty stdout when the target branch has
  no branch protection. Reproduced 2026-05-26 against PR #553 on
  sympoies/nils-cli. [F5]
- `Self::run` at `crates/plan-issue-cli/src/github.rs:97-111` treats any
  non-zero `gh` exit as `Err(...)`. `pr_required_summary` then short-
  circuits to `(None, None, Vec::new())` at lines 152-156, which feeds
  `required_state: None` into the renderer. [F6]
- The fallback inside `pr_required_summary` that returns `(Some("success"),
  Some(0), Vec::new())` (lines 164-166) is only reachable when
  `Self::run` succeeded (exit 0) and `serde_json::from_str` failed
  because the stdout is empty. With `gh` exiting non-zero on the
  "no required checks reported" path, that branch is dead code on
  current `gh` versions. [F7]
- `main` on `sympoies/nils-cli` has no branch protection. `gh api
  repos/sympoies/nils-cli/branches/main/protection` returns HTTP 404
  `Branch not protected`. The repo is therefore the canonical
  reproduction surface for the bug. [F8]
- The closeout payload `closeout.v1` carries `linked_prs[].required_state`
  as a nullable string. The hex-encoded payload on
  <https://github.com/sympoies/nils-cli/issues/541#issuecomment-4543937296>
  shows `"required_state":null` for every PR. Historical closeout
  records on this repository are immutable; any rendering change applies
  only to records produced after the fix lands. [F9]
- An earlier fix landed for the close-gate side of the same field in
  `docs/plans/plan-issue-close-non-required-checks/`, which already
  stopped non-required failures from blocking close. That change did not
  touch the renderer or the `gh pr checks --required` invocation. The
  surface this document targets is downstream of that fix. [F10]
- The forge-cli GitLab adapter (`crates/plan-issue-cli/src/forge_cli_adapter.rs:353-354`)
  returns `required_state: None, required_count: None` for every PR
  because it has no equivalent of `gh pr checks --required`. That code
  path is out of scope here; the GitLab adapter already documents the
  gap in `docs/plans/plan-issue-close-non-required-checks/` Risks. [F11]

## Decisions

1. **Fix the `gh` invocation first.** Drop the unsupported `conclusion`
   field from the `--json` argument list in `pr_required_summary` and
   restrict it to fields the function actually reads (`name`, plus
   `state` for defensive future use). The dead-code "empty stdout" branch
   becomes reachable only when a future `gh` version changes behavior
   again; we keep it as a defensive fallback rather than relying on it.
2. **Recognise "no required checks reported" as a non-error.** When `gh
   pr checks --required` exits non-zero and stderr matches the canonical
   "no required checks reported" pattern (substring `no required checks
   reported`), treat it as the zero-required-checks case and return
   `(Some("success"), Some(0), Vec::new())`. Other non-zero exits remain
   errors and propagate as `(None, None, Vec::new())`.
3. **Distinguish the three closeout-table labels.** Replace the single
   `Option<CheckStatus>` rendering with a closed set of labels at the
   render layer only:
   - `Some(CheckStatus::Pass)` with `required_count == Some(0)` →
     `n/a (no required)` (the "no rule on branch" case)
   - `Some(CheckStatus::Pass)` with `required_count >= 1` →
     `pass (<n>)`
   - `Some(CheckStatus::Fail)` → `fail (<n>)` (and the non-required
     failures column carries the detail as today)
   - `Some(CheckStatus::None)` → `none`
   - `None` (probe failed or fixture omitted) → `unknown`
4. **No wire-format change to `closeout.v1`.** Render-only fix. The
   payload's `required_state` stays `Option<CheckStatus>` and
   `required_count` stays `Option<u32>`. Historical closeouts continue to
   parse; their hex-encoded payloads do not need re-encoding.
5. **Test the live probe deterministically.** Add a unit test that
   exercises `pr_required_summary` against an injected `Self::run`
   replacement (refactor `pr_required_summary` to take a runner closure
   or move the `gh`-shellout behind a trait) so the no-required-checks
   path is regression-tested without requiring a live `gh` install or a
   protected branch.
6. **Update the fixture renderer test set.** Add render-layer unit tests
   covering all five label branches enumerated in decision 3, exercised
   through the existing renderer entry points without going through the
   live probe.
7. **Out of scope.** No new branch protection on `sympoies/nils-cli main`
   is requested by this work; the rendering must be correct whether or
   not the repository adopts branch protection later. No GitLab adapter
   parity — that gap is already tracked under
   `docs/plans/plan-issue-close-non-required-checks/`. No bump of
   `closeout.v1` schema; future schema bumps can carry the distinction in
   the wire format if a reader wants it.

## Scope

- `crates/plan-issue-cli/src/github.rs`: change the `--json` field list
  in `pr_required_summary`, add the "no required checks reported" stderr
  branch, and refactor enough to inject a test-only runner for the
  function.
- `crates/plan-issue-cli/src/lifecycle_record.rs`: change the render
  branch at lines 2062-2074 to emit one of the five labels in decision
  3; add unit tests for each branch.
- `crates/plan-issue-cli/src/forge_cli_adapter.rs`: no behavior change,
  but verify the function continues returning `(None, None)` for GitLab
  and add a comment pointing at the GitLab-adapter follow-up plan so the
  next reader knows the asymmetry is intentional.
- `crates/plan-issue-cli/tests/`: add an integration test asserting the
  rendered closeout table contains `n/a (no required)` for a PR seeded
  with `required_state=Pass, required_count=0`, `pass (N)` for a PR
  seeded with `required_state=Pass, required_count=N (N>=1)`, and
  `unknown` for a PR seeded with `required_state=None`.

## Non-scope

- `closeout.v1` payload schema. The wire format is unchanged.
- The `Checks` (aggregate) column. PR #554 in #541's closeout shows
  `Checks: none` because GitHub Actions did not register any check
  suite for the merge SHA; that is unrelated to required-check
  resolution and is not addressed here.
- GitLab adapter parity. Tracked under
  `docs/plans/plan-issue-close-non-required-checks/`.
- Adding branch protection on the host repository. The bug must be
  fixed independently of whether the repo adopts protection.
- Backfill of historical closeout comments. They remain in their
  posted form.

## Implementation boundaries

- Render-layer change must be a pure function of the payload it
  receives. No new fetches, no provider lookups inside the renderer.
- The runner injection in `pr_required_summary` should be the smallest
  change that lets tests pass without `gh` on PATH. A function-pointer
  parameter, a small `Runner` trait, or moving the function to a method
  on a struct that carries a runner are all acceptable; pick whichever
  is least invasive given the rest of `github.rs`.
- No new dependencies. `serde_json::Value` and `std::process::ExitStatus`
  are sufficient.

## Requirements

- **R1.** `plan-issue record close --dry-run` against a PR on a branch
  with no required checks renders that PR's row as `n/a (no required)`
  in the `Required` column.
- **R2.** `plan-issue record close --dry-run` against a PR on a branch
  with N≥1 required checks (all passing) renders that PR's row as
  `pass (N)`.
- **R3.** `plan-issue record close --dry-run` against a fixture PR whose
  `requiredCheckRollup` field is omitted renders the row as `unknown`
  and does not crash.
- **R4.** `pr_required_summary` returns `(Some("success"), Some(0), [])`
  when `gh pr checks --required` exits non-zero with stderr matching `no
  required checks reported`, with no other side effects.
- **R5.** A unit test exercises the no-required-checks success path of
  `pr_required_summary` without invoking the real `gh` binary.

## Acceptance criteria

- **AC-1.** `cargo test -p plan-issue-cli` is green.
- **AC-2.** A new render-layer unit test covers all five label
  branches enumerated in decision 3; each branch asserts the expected
  table cell text.
- **AC-3.** A new probe-layer unit test asserts that the "no required
  checks reported" stderr path of `gh pr checks --required` produces the
  `(Some("success"), Some(0), [])` return value, exercised through an
  injected runner.
- **AC-4.** Re-running `plan-issue record close --dry-run` against
  sympoies/nils-cli#541's PR set on a freshly built binary renders the
  `Required` column as `n/a (no required)` for every PR (matching the
  current state of `main`'s branch protection).
- **AC-5.** The diff does not touch `closeout.v1` payload writing or
  parsing. Existing closeout comments still parse cleanly.

## Validation plan

1. `cargo test -p plan-issue-cli` (workspace and per-crate; the second
   guards against the `serde_json/preserve_order` workspace-unification
   trap captured in
   `core/policies/heuristic-system/error-inbox/workspace-feature-union-preserve-order/`).
2. `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` for
   docs-only hygiene.
3. `cargo clippy -p plan-issue-cli --all-targets --all-features -- -D
   warnings` to keep the crate's clippy footprint clean.
4. Manual: build `plan-issue` locally and re-run `record close --dry-run
   --issue 541 --linked-pr sympoies/nils-cli#542 --linked-pr ...` against
   the live provider; eyeball the rendered table for the new label set.
5. Manual: run the same dry-run against a fixture whose
   `requiredCheckRollup` is intentionally omitted (extend an existing
   fixture under `crates/plan-issue-cli/tests/fixtures/` for this) and
   confirm the row reads `unknown` rather than crashing.

## Findings table

| ID  | Source                                                    | Disposition                                                           |
| --- | --------------------------------------------------------- | --------------------------------------------------------------------- |
| F-1 | `crates/plan-issue-cli/src/lifecycle_record.rs:2062-2074` | In scope — replace unwrap_or("unknown") with five-branch label table  |
| F-2 | `crates/plan-issue-cli/src/github.rs:151`                 | In scope — drop unsupported `conclusion` JSON field                   |
| F-3 | `crates/plan-issue-cli/src/github.rs:152-156`             | In scope — recognise "no required checks reported" stderr             |
| F-4 | `crates/plan-issue-cli/src/github.rs:97-111`              | In scope — refactor for runner injection                              |
| F-5 | `crates/plan-issue-cli/src/execute.rs:561-573`            | Confirmed read-only — fixture parser already handles `None` correctly |
| F-6 | `crates/plan-issue-cli/src/forge_cli_adapter.rs:353-354`  | Annotate intentional gap; follow-up tracked elsewhere                 |
| F-7 | sympoies/nils-cli#541 closeout comment evidence           | Bug-tracer source; render fix validated against the same case         |

## Risks and guardrails

- **R-1.** A future `gh` release changes the no-required-checks stderr
  string. Mitigation: keep the stderr-match defensive (substring rather
  than full-line equality), and include the matched substring in a unit
  test asserting the canonical message string. Document the canonical
  string in a `// upstream contract:` comment near the matcher so the
  next reader knows what to update if `gh` regresses.
- **R-2.** The render-layer label set leaks into closeout-comment
  consumers (other tooling that scrapes the table). Mitigation: the
  label strings are already free-form Markdown, not part of the
  `closeout.v1` payload, and no existing automation is known to parse
  them. The migration plan adds a release note flagging the label
  change.
- **R-3.** Refactoring `pr_required_summary` for runner injection
  unintentionally changes behavior on a real `gh` install. Mitigation:
  add a contract-style integration test marked `#[ignore]` by default
  that hits the real `gh` and confirms the no-required-checks path
  still returns `(Some("success"), Some(0), Vec::new())` end-to-end.

## Execution

- Recommended plan:
  `docs/plans/plan-issue-closeout-required-check-rendering/plan-issue-closeout-required-check-rendering-plan.md`
- Recommended execution state:
  `docs/plans/plan-issue-closeout-required-check-rendering/plan-issue-closeout-required-check-rendering-execution-state.md`
- Status: ready
- Next-task source: this document; the plan can fit in a single sprint.

## Retention intent

Cleanup after execution. This bug is local to the rendering / probe
layer and does not introduce a new contract; once the fix lands and the
plan closes, the source document can be dropped without losing any
durable knowledge.

## Read-first references

- `docs/plans/plan-issue-close-non-required-checks/plan-issue-close-non-required-checks-discussion-source.md`
  — required-vs-non-required gate fix; explains the
  `LinkedPrEvidence.required_state` field shape and the GitLab parity
  gap.
- `crates/plan-issue-cli/docs/specs/issue-backed-plan-record-contract-v2.md`
  — `closeout.v1` schema; confirms `required_state` stays
  `Option<CheckStatus>` over the wire.
- `core/policies/heuristic-system/error-inbox/workspace-feature-union-preserve-order/ENTRY.md`
  — when adding `serde_json::Value` fixture assertions, pin
  `serde_json/preserve_order` in the crate.

## Recommended next artifact

- Issue + small PR. Pre-PR: open a sympoies/nils-cli issue summarising
  the two-bug stack, link this source doc as `Read First`, then implement
  in a single PR with the unit/integration tests required by AC-1
  through AC-5.
