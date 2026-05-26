# Plan: plan-issue closeout GitLab `Required` column parity

## Overview

Land the small follow-up fix that closes sympoies/nils-cli#557: switch
the GitLab branch of
`crates/plan-issue-cli/src/forge_cli_adapter.rs::pr_merge_summary` from
returning `required_state: None, required_count: None,
non_required_failures: []` to returning
`required_state: Some("success".to_string()), required_count: Some(0),
non_required_failures: []`. The `PrMergeSummary.required_state` field
is `Option<String>` at the adapter boundary; downstream of the adapter
`execute.rs::check_status_from_state` converts `"success"` to
`CheckStatus::Pass`, and the render-layer five-label table (landed in
sympoies/nils-cli#563) already maps `(Some(Pass), Some(0))` to
`none required`. So the closeout-comment `Required` column will stop
reading `unknown` for every GitLab PR while the wire format
(`closeout.v1`) and the close-gate match arms in
`lifecycle_record.rs:2446-2469` stay unchanged.

The change is a single struct-literal swap plus a comment rewrite plus
one extended adapter test. No new helpers, no module reorganisation, no
schema bump. Source: this bundle's discussion source doc.

## Read First

- Primary source: docs/plans/plan-issue-closeout-gitlab-required-parity/plan-issue-closeout-gitlab-required-parity-discussion-source.md
- Source type: discussion-to-implementation-doc
- Open questions carried into execution: none (close-gate semantic shift
  acknowledged in source doc Decision 2 / Risk R-1; consistent with
  #502)
- Sibling fix (already on `main`):
  `docs/plans/plan-issue-closeout-required-check-rendering/plan-issue-closeout-required-check-rendering-plan.md`
  / sympoies/nils-cli#563
- Earlier close-gate contract:
  `docs/plans/plan-issue-close-non-required-checks/plan-issue-close-non-required-checks-discussion-source.md`
  / sympoies/nils-cli#502 / #512
- Tracked issue: sympoies/nils-cli#557

## Scope

- In scope:
  - Replace the `(None, None, [])` return triple in
    `crates/plan-issue-cli/src/forge_cli_adapter.rs::pr_merge_summary`
    (GitLab branch) with `(Some("success".to_string()), Some(0), [])`.
  - Refresh the inline comment at
    `crates/plan-issue-cli/src/forge_cli_adapter.rs:343-347` to state
    the new contract (GitLab reports zero required checks; close-gate
    treats as clean resolve per #502) and reference #557.
  - Extend `pr_merge_summary_composes_view_and_checks`
    (`crates/plan-issue-cli/src/forge_cli_adapter.rs:748-766`), or add
    a sibling test in the same `#[cfg(test)] mod tests` block, asserting
    the new triple.
  - CHANGELOG entry under `crates/plan-issue-cli/CHANGELOG.md`'s
    `[Unreleased]` block referencing this fix and #557.
- Out of scope:
  - The GitHub adapter (`crates/plan-issue-cli/src/github.rs`). The
    render fix landed in #563; this PR does not touch it.
  - The closeout-comment renderer
    (`crates/plan-issue-cli/src/lifecycle_record.rs`). The five-branch
    `required_check_label` already covers the new triple.
  - `closeout.v1` payload schema. Wire format is unchanged.
  - The close-gate logic in `lifecycle_record.rs:2446-2469`. Match arms
    stay as today; only which arm GitLab PRs land in changes.
  - Backfill of historical GitLab closeout comments. Immutable.
  - Adding required-check rules at the GitLab project level. Operator
    concern.

## Assumptions

- The existing renderer test
  `required_check_label_emits_five_distinct_branches`
  (`crates/plan-issue-cli/src/lifecycle_record.rs:3223-3260`) already
  pins `(Some(Pass), Some(0)) → "none required"`. No render-layer test
  change is needed.
- The adapter test scaffolding `adapter_with(vec![...])` at
  `crates/plan-issue-cli/src/forge_cli_adapter.rs:723-766` accepts the
  same `cli.forge-cli.pr.view.v1` / `cli.forge-cli.pr.checks.v1`
  envelope fixtures used by the existing test; no new fake-process
  helper needed.
- The close-gate semantic shift (Decision 2 in the source doc; Risk R-1)
  is acceptable. Operators who want a strict "must be green to close"
  policy on GitLab can mark the pipeline as required at the GitLab
  project level.

## Sprint 1: GitLab adapter parity

**Goal**: GitLab PRs in a closeout comment render `Required: none
required` (matching the GitHub fix in #563) instead of `Required:
unknown`, without touching the renderer, the close-gate, or the wire
format.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-plan-issue-cli`
  - `cargo clippy -p nils-plan-issue-cli --all-targets --all-features -- -D warnings`
  - `cargo build -p nils-plan-issue-cli --locked`
- Verify: `cargo test -p nils-plan-issue-cli forge_cli_adapter` is green
  with the extended assertion; the renderer test
  `required_check_label_emits_five_distinct_branches` continues to pass
  unmodified.

### Task 1.1: Swap GitLab adapter return triple and refresh comment

- **Location**:
  - `crates/plan-issue-cli/src/forge_cli_adapter.rs:343-356`
- **Description**: Replace the `(None, None, [])` triple in the GitLab
  branch of `pr_merge_summary` with
  `(Some("success".to_string()), Some(0), Vec::new())`. The string is
  converted to `CheckStatus::Pass` downstream of the adapter by
  `execute.rs::check_status_from_state`. Rewrite the inline
  comment at `forge_cli_adapter.rs:343-347` to state the new contract:
  GitLab has no first-class required-check concept, so the adapter
  reports zero required checks (mirroring the shape the GitHub adapter
  returns for a branch without a required-check rule); the close gate
  in `lifecycle_record.rs:2450` then treats it as a clean resolve per
  the #502 contract. Reference sympoies/nils-cli#557 in the comment so
  the next reader can audit the assumption.
- **Dependencies**: none
- **Complexity**: 1
- **Acceptance criteria**:
  - `pr_merge_summary` on the GitLab adapter returns
    `required_state.as_deref() == Some("success")`,
    `required_count == Some(0)`, and an empty `non_required_failures`
    vector for every PR.
  - The inline comment cites #557 and states the new contract; the
    stale "leave the required fields at `None`/empty" sentence is gone.
  - No other adapter call sites are modified.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli forge_cli_adapter`

### Task 1.2: Extend adapter unit test and CHANGELOG entry

- **Location**:
  - `crates/plan-issue-cli/src/forge_cli_adapter.rs:748-766`
    (or a sibling test in the same `#[cfg(test)] mod tests` block)
  - `crates/plan-issue-cli/CHANGELOG.md`
- **Description**: Extend `pr_merge_summary_composes_view_and_checks`
  (or add a focused sibling test) to assert the new triple alongside
  the existing `state` / `merged` / `merge_sha` / `checks` assertions:
  `summary.required_state.as_deref() == Some("success")`,
  `summary.required_count == Some(0)`,
  `summary.non_required_failures.is_empty()`. Add a CHANGELOG entry
  under the `[Unreleased]` `### Fixed` (or equivalent) block referencing
  this fix and sympoies/nils-cli#557.
- **Dependencies**: Task 1.1
- **Complexity**: 1
- **Acceptance criteria**:
  - The test asserts the full new triple on the existing fixture, with
    no new `adapter_with(...)` fixture envelope required.
  - The test passes without `glab` on PATH.
  - The renderer test
    `required_check_label_emits_five_distinct_branches` continues to
    pass unmodified.
  - CHANGELOG entry references sympoies/nils-cli#557 and the parent
    sympoies/nils-cli#502 / #563 context.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli`
  - `cargo clippy -p nils-plan-issue-cli --all-targets --all-features -- -D warnings`
  - `cargo build -p nils-plan-issue-cli --locked`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
  - `cargo nextest run --workspace`
