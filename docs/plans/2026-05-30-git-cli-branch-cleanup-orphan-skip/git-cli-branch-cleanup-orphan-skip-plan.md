# Plan: git-cli branch cleanup skip unrelated-history branches

## Overview

Make `git-cli branch cleanup --squash` resilient to local branches that
have no merge-base with `base` (unrelated / orphan history). Today the
squash-mode loop treats a failed `git merge-base` probe as fatal and
`return 1`s, aborting the whole sweep before any candidate is listed; a
single orphan branch defeats the command. The fix: a branch with no
merge-base cannot be a squash-merge of `base`, so skip it (`continue`)
and let the sweep finish. Add an integration test that reproduces the
abort (an orphan branch alongside a real squash-merge) so the regression
is pinned.

Source: this bundle's discussion source doc (Read First, below). The one
option (skip vs. exit-code-discrimination) is resolved there to the
recommended skip-on-no-merge-base default; no open questions are carried
into execution.

## Read First

- Primary source:
  `docs/plans/git-cli-branch-cleanup-orphan-skip/git-cli-branch-cleanup-orphan-skip-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Source issue: none (found 2026-05-30 while verifying #660 / v0.29.0 on
  `agent-runtime-kit`).
- Open questions carried into execution: none (exit-code discrimination
  is recorded as a non-blocking follow-up in the source doc, not part of
  this change).
- Implementation surface:
  - `crates/git-cli/src/branch.rs` — the squash-mode loop's `merge-base`
    `Err` arm that currently prints `Failed to find merge-base` and
    `return 1`.
  - `crates/git-cli/tests/integration/branch.rs` — add an orphan-branch
    fixture and a test asserting the sweep skips it and still lists the
    squash-merged branch.

## Read First boundary

- Skipping no-merge-base branches must not change behavior for
  related-history branches: the v0.29.0 squash detection (synthetic
  merge-base commit + patch-id compare) stays exactly as is.
- Orphan branches are skipped, never auto-listed for deletion: they are
  not squash-merges of `base`, so they must not appear as cleanup
  candidates.
- No new third-party dependency, so the `Cargo.lock` locked-build and
  third-party gates stay clean.

## Scope

- In scope:
  - Change the squash-loop `merge-base` failure handling from fatal
    (`return 1`) to skip (`continue`).
  - An integration test with an orphan-history branch (own root commit)
    plus a real squash-merge, asserting the run completes and lists only
    the squash-merge.
- Out of scope:
  - Deleting / pruning orphan branches (a separate cleanup concern).
  - `--merged` (non-squash) mode behavior.
  - Exit-code discrimination between "no merge base" (1) and other git
    failures (128) — recorded as a follow-up in the source doc.

## Assumptions

- Branch names fed to the loop come from `git for-each-ref`, so they are
  valid refs; the only realistic `git merge-base` failure here is "no
  common ancestor", which `continue` handles correctly.
- The existing related-history fixtures (`setup_repo_with_branches`,
  `setup_repo_with_real_squash`) already cover the positive squash-merge
  path, so the new test only needs to add the orphan dimension.

## Sprint 1: skip unrelated-history branches in squash mode

**Goal**: `git-cli branch cleanup --squash` completes when a local
branch has unrelated history, skipping that branch and still listing the
genuinely squash-merged branches.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-git-cli`
  - In a repo with an orphan branch + a squash-merged branch:
    `printf 'n\n' | git-cli branch cleanup --squash`
- Verify: the orphan branch is absent from the candidate list, the
  squash-merged branch is present, and the command does not abort with
  `Failed to find merge-base`.

### Task 1.1: Reproduce the abort with a failing test (test-first)

- **Location**:
  - `crates/git-cli/tests/integration/branch.rs` (new orphan fixture +
    test)
- **Description**: Add a fixture that creates an orphan-history branch
  (e.g. `git checkout --orphan`, one fixture commit) alongside the
  existing real squash-merge, then assert `branch cleanup --squash`
  lists the squash-merged branch and does not abort. Confirm the test
  fails against the current `return 1` behavior.
- **Dependencies**: none
- **Complexity**: 1
- **Acceptance criteria**:
  - The new test fails on current `main` with the merge-base abort
    (non-zero exit / missing candidate).
- **Validation**:
  - `cargo test -p nils-git-cli branch_cleanup` (expect the new case to
    fail before Task 1.2)

### Task 1.2: Skip no-merge-base branches instead of aborting

- **Location**:
  - `crates/git-cli/src/branch.rs` (squash-mode loop `merge-base` `Err`
    arm)
- **Description**: Change the `merge-base` `Err` arm from
  `eprintln!(...); return 1` to `continue`, with a comment explaining
  that unrelated history cannot be a squash-merge of `base`. Leave the
  `--squash` detection logic otherwise unchanged.
- **Dependencies**: Task 1.1
- **Complexity**: 1
- **Acceptance criteria**:
  - The Task 1.1 test passes.
  - The existing `branch_cleanup_squash_*` tests stay green.
- **Validation**:
  - `cargo test -p nils-git-cli`

### Task 1.3: Format, lint, and full crate gate

- **Location**:
  - `crates/git-cli`
- **Description**: Run formatter, clippy, and the crate test suite to
  confirm no regression and no lint debt.
- **Dependencies**: Task 1.2
- **Complexity**: 1
- **Acceptance criteria**:
  - `cargo fmt --all -- --check`, `cargo clippy -p nils-git-cli
    --all-targets --all-features -- -D warnings`, and
    `cargo test -p nils-git-cli` all pass.
- **Validation**:
  - `cargo fmt --all -- --check`
  - `cargo clippy -p nils-git-cli --all-targets --all-features -- -D warnings`
  - `cargo test -p nils-git-cli`

## Risks

- **R-1**: `continue` could mask a genuine git failure (not "no merge
  base"). Mitigation: branch refs come from `for-each-ref` so a non-"no
  merge base" failure is not a realistic state; exit-code discrimination
  is recorded as a follow-up if that ever changes.
- **R-2**: The orphan fixture could behave differently across git
  versions (`--orphan` semantics). Mitigation: use a minimal
  single-commit orphan and assert only on candidate presence/absence and
  exit, not on git-internal messages.
