# Plan: plan-issue closeout `Required` column rendering fix

## Overview

Land a focused fix in `plan-issue-cli` so that the `record close`
closeout-comment's `Required` column stops collapsing every healthy and
unhealthy code path into the single label `unknown`. The fix has three
layers:

- Repair the live probe in
  `crates/plan-issue-cli/src/github.rs:138-225` (`pr_required_summary`)
  so it can actually return `(Some("success"), Some(0), [])` on the
  canonical "no required-check rule" path: drop the `conclusion`
  field from the `gh pr checks --required --json …` invocation and
  add a stderr branch recognising the "no required checks reported"
  message.
- Make every existing `Self::run` call site in `github.rs` go through
  a single module-scope `GhRunner` abstraction so the live probe can
  be exercised in unit tests without a real `gh` binary on PATH.
- Widen the closeout-comment renderer in
  `crates/plan-issue-cli/src/lifecycle_record.rs:2062-2074` to five
  labels (`none required` / `pass (N)` / `fail (N)` / `none` /
  `unknown`) so the three present-day `required_state == None` causes
  are distinguishable, and a future regression remains visible.

The `closeout.v1` payload wire format is intentionally unchanged.
Historical closeout comments (e.g. sympoies/nils-cli#541's #4543937296)
remain immutable; only records produced after the fix lands carry the
new labels.

Source: this bundle's discussion source doc (Read First, below).

## Read First

- Primary source:
  `docs/plans/plan-issue-closeout-required-check-rendering/plan-issue-closeout-required-check-rendering-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Open questions carried into execution: none (three resolved at
  source-doc fold-in commit `0d09ccd`: label string locked to
  `none required`; `GhRunner` covers all `Self::run` callers; GitLab
  fallback explicitly out of scope and tracked under
  sympoies/nils-cli#557)
- Upstream contract:
  `crates/plan-issue-cli/docs/specs/issue-backed-plan-record-contract-v2.md`
  — `closeout.v1` payload `linked_prs[].required_state` stays nullable
  string over the wire.
- Sibling follow-up (out of scope here, tracked separately):
  sympoies/nils-cli#557 (GitLab adapter renders `Required: unknown`
  for healthy PRs).
- Earlier related plan whose Risk R-2 deferred the GitLab parity:
  `docs/plans/plan-issue-close-non-required-checks/plan-issue-close-non-required-checks-discussion-source.md`

## Scope

- In scope:
  - Module-scope `GhRunner` abstraction in
    `crates/plan-issue-cli/src/github.rs` and migration of every
    existing `Self::run(&args)` call site onto it.
  - `pr_required_summary` `--json` field list fix (drop `conclusion`).
  - `pr_required_summary` stderr branch recognising `no required
    checks reported`.
  - Renderer change in `crates/plan-issue-cli/src/lifecycle_record.rs`
    `linked_prs[]` row construction (`required` column only).
  - Renderer-layer unit tests covering all five label branches.
  - Probe-layer unit test exercising the no-required-checks success
    path of `pr_required_summary` through an injected runner.
  - Optional `#[ignore]`-by-default contract test that hits the real
    `gh` binary to guard against future `gh` regressions on the
    `no required checks reported` stderr.
  - CHANGELOG entry referencing this fix and the parent
    sympoies/nils-cli#541 closeout observation.
- Out of scope:
  - Any change to the `closeout.v1` payload shape or the
    `LinkedPrEvidence` field shape.
  - Any change to the close-gate semantics fixed under
    `docs/plans/plan-issue-close-non-required-checks/` /
    sympoies/nils-cli#502.
  - GitLab adapter parity (`forge_cli_adapter.rs:353-354` stays
    `(None, None)`; tracked at sympoies/nils-cli#557).
  - Adding branch protection on `sympoies/nils-cli main`.
  - Backfilling historical closeout comments.
  - Any change to the `Checks` (aggregate) column rendering. PR #554's
    `Checks: none` row on #541's closeout reflects a separate GHA
    workflow-trigger glitch and is not addressed here.

## Assumptions

- Current `gh` (Homebrew `gh` on the maintainer's machine) is the
  representative client. Reproduced 2026-05-26 against PRs #542 and
  #553 on sympoies/nils-cli: `gh pr checks <pr> --required --json
  bucket,state,conclusion,name` exits 5 with `Unknown JSON field:
  "conclusion"`; `gh pr checks <pr> --required --json name,state`
  exits 1 with stderr `no required checks reported on the '<branch>'
  branch` when the target branch has no branch protection.
- The `none required` label string is acceptable to closeout-comment
  consumers; no automation is known to parse the `Required` column
  today.

## Sprint 1: Live probe and renderer fix

**Goal**: Make `plan-issue record close --dry-run` render PRs under a
branch without a required-check rule as `Required: none required` (not
`unknown`) on a live `gh` install, with deterministic unit-test
coverage for every render branch and the probe's happy path.

**Demo/Validation**:

- Commands:
  - `cargo test -p plan-issue-cli`
  - `cargo build --release -p plan-issue-cli`
  - `plan-issue record close --issue 541 --repo sympoies/nils-cli
    --linked-pr sympoies/nils-cli#542 --linked-pr sympoies/nils-cli#553
    --approval "post-fix verification" --dry-run`
- Verify: the rendered closeout-comment table renders `Required: none
  required` for every PR row (matching the current absence of
  required-check rules on `main`).

### Task 1.1: Introduce `GhRunner` abstraction and migrate all `Self::run` call sites

- **Location**:
  - `crates/plan-issue-cli/src/github.rs:97-111` (current `Self::run`)
  - All other `Self::run` call sites in the same file
- **Description**: Introduce a small module-scope abstraction — either
  a function pointer type alias (`type GhRunner = fn(&[&str]) ->
  Result<RunOutput, String>`) or a tiny `trait GhRunner` — that wraps
  the existing `gh` shellout behaviour. Migrate every existing
  `Self::run(&args)` call site in `github.rs` onto the new runner.
  Production code passes the real runner; tests can inject a fake.
  No behaviour change for production callers.
- **Dependencies**: none
- **Complexity**: 2
- **Acceptance criteria**:
  - Every existing `Self::run(&args)` call site in
    `crates/plan-issue-cli/src/github.rs` is routed through the new
    abstraction.
  - Production behaviour unchanged: `cargo test -p plan-issue-cli`
    passes without modifications to existing fixture data.
  - A new module-level test exercises an injected fake runner and
    asserts the abstraction handles success, non-zero exit, and
    stderr capture correctly.
- **Validation**:
  - `cargo test -p plan-issue-cli github`

### Task 1.2: Repair `pr_required_summary` live probe

- **Location**:
  - `crates/plan-issue-cli/src/github.rs:138-225`
- **Description**: Drop the unsupported `conclusion` field from the
  `gh pr checks --required --json …` invocation (the function never
  reads `conclusion` from the resulting array; the rollup-side
  `.get("conclusion").or_else(|| item.get("state"))` is on a different
  Value entirely). Add a stderr branch: when `gh` exits non-zero and
  stderr contains the substring `no required checks reported`, treat
  it as the zero-required-checks success case and return
  `(Some("success"), Some(0), Vec::new())`. Keep the existing
  empty-stdout fallback as a defensive secondary path. Add a
  `// upstream contract:` comment near the matcher pinning the
  canonical `gh` message so the next reader knows what to update if
  `gh` regresses.
- **Dependencies**: Task 1.1
- **Complexity**: 2
- **Acceptance criteria**:
  - The function returns `(Some("success"), Some(0), Vec::new())`
    when the injected runner reports exit 1 + stderr containing
    `no required checks reported`.
  - Other non-zero exits still propagate as `(None, None, Vec::new())`.
  - The `--json` argument list contains only fields the function
    actually reads.
- **Validation**:
  - `cargo test -p plan-issue-cli github pr_required_summary`

### Task 1.3: Widen renderer to five-label `Required` column

- **Location**:
  - `crates/plan-issue-cli/src/lifecycle_record.rs:2062-2074`
- **Description**: Replace the current
  `required_state.map(check_status_label).unwrap_or("unknown")` with a
  closed-form match over `(required_state, required_count)` that emits
  one of five labels: `none required` for
  `Some(CheckStatus::Pass) + Some(0)`; `pass (N)` for
  `Some(CheckStatus::Pass) + Some(N>=1)`; `fail (N)` for
  `Some(CheckStatus::Fail) + Some(N)` (count omitted when
  `required_count` is `None`); `none` for `Some(CheckStatus::None)`;
  `unknown` for `None`. The renderer can land before or after Task
  1.2 because it operates on the payload-side fields, not on `gh`.
- **Dependencies**: none
- **Complexity**: 1
- **Acceptance criteria**:
  - All five branches are exercised by unit tests asserting the exact
    rendered cell text.
  - No change to the `closeout.v1` payload shape; only the rendered
    Markdown cell changes.
  - Existing renderer test fixtures continue to pass with their
    expected labels adjusted to the new strings.
- **Validation**:
  - `cargo test -p plan-issue-cli lifecycle_record render`

### Task 1.4: Probe and renderer integration coverage

- **Location**:
  - `crates/plan-issue-cli/src/github.rs` (probe-side test, using the
    injected runner from Task 1.1)
  - `crates/plan-issue-cli/tests/integration/` (closeout-render
    integration; reuses existing fixture infrastructure)
- **Description**: Add a probe-side unit test asserting the "no
  required checks reported" stderr path produces
  `(Some("success"), Some(0), Vec::new())` through an injected
  runner. Add an integration test asserting that `record close
  --dry-run` against a fixture PR seeded with `required_state=Pass,
  required_count=0` renders `Required: none required` in the closeout
  table, and that a fixture omitting `requiredCheckRollup` renders
  `Required: unknown` without crashing.
- **Dependencies**: Task 1.1, Task 1.2, Task 1.3
- **Complexity**: 2
- **Acceptance criteria**:
  - Probe unit test passes deterministically without `gh` on PATH.
  - Integration tests cover the two new render outcomes.
  - Optional `#[ignore]`-by-default integration test wires the real
    `gh` shellout and asserts the no-required-checks path end-to-end
    against `sympoies/nils-cli#553` (or equivalent), so a future `gh`
    regression on the stderr message can be caught manually with
    `cargo test --ignored`.
- **Validation**:
  - `cargo test -p plan-issue-cli`
  - `cargo nextest run --workspace`

### Task 1.5: CHANGELOG, manual live verification, and PR

- **Location**:
  - `crates/plan-issue-cli/CHANGELOG.md`
  - `target/release/plan-issue` (build artifact)
- **Description**: Add a CHANGELOG entry under the next
  `plan-issue-cli` version referencing this fix and the parent
  observation on sympoies/nils-cli#541. Build the binary locally,
  re-run `plan-issue record close --dry-run --issue 541 --repo
  sympoies/nils-cli --linked-pr sympoies/nils-cli#542 …` against the
  live provider, and screenshot the rendered closeout-table snippet
  in the PR description so reviewers can see the before/after
  rendering without rebuilding locally. Open the implementation PR.
- **Dependencies**: Task 1.4
- **Complexity**: 1
- **Acceptance criteria**:
  - CHANGELOG entry lands in the same PR as the implementation.
  - The PR description embeds (or links) a manual verification
    transcript showing the new `none required` rendering.
  - PR title fits the project's 70-char title cap.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
    (CHANGELOG hygiene)
  - `cargo nextest run --workspace` (full gate)
