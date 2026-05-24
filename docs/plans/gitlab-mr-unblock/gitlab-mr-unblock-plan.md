# Plan: GitLab MR Unblock

## Overview

Land two small, self-contained fixes in `forge-cli` so the GitLab branch of
the skill stack stops blocking MR delivery on `glab >= 1.46`:

1. **F-1**: `forge-cli issue list` GitLab arm uses the wrong glab JSON flag
   (`-F json`) which glab 1.46+ does not honor for `issue list`. Switch to
   `--output json`.
2. **F-2**: Bump the `glab` text-parser support pin from minor 45 to minor 99
   so `pr checks` / `pr wait-checks` / `pr merge` / `pr deliver` work on
   current homebrew glab.

F-3 (plan-issue-cli is hardwired to gh) is **out of scope** and spins out to
a separate plan.

## Read First

- Primary source:
  `docs/plans/gitlab-mr-unblock/gitlab-mr-unblock-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Downstream sandbox findings live in `terrylin/agent-runtime-testing`
  (GitLab) under
  `docs/plans/gitlab-skill-validation/gitlab-skill-validation-discussion-source.md`.

## Scope

- In scope:
  - F-1 argv edit + test update in `crates/forge-cli/src/ops/issue_list.rs`.
  - F-2 constant bump in `crates/forge-cli/src/glab_version.rs`.
  - Optional empty-array parse regression test in `issue_list.rs` if not
    already covered.
  - `cargo test -p forge-cli` clean.
- Out of scope:
  - Anything in `plan-issue-cli`.
  - `pr_checks_gitlab` parser changes (only the pin is touched; if the parser
    breaks on 1.99 that is a separate finding handled in a follow-up).
  - Dispatch / plan-tracking skills (downstream gap rooted in F-3).

## Sprint 1: Forge-cli fixes

### T1: F-1 — switch GitLab issue list to `--output json`

- File: `crates/forge-cli/src/ops/issue_list.rs`
- Change the two argv lines in the GitLab arm of `build_list_call` from
  `-F` / `json` to `--output` / `json`.
- Update the unit test `build_list_call_gitlab_maps_state_to_flag_and_repeats_labels`
  so the assertion checks for `--output` (or simply removes the `-F`-specific
  assertion).
- Add a regression test that `parse_list_output` accepts an empty array
  (`[]`) without erroring, if no such test exists.
- Source type: discussion-to-implementation-doc

Acceptance:
- `cargo test -p forge-cli ops::issue_list` is green.
- Manual repro against sandbox repo:
  `forge-cli --format json issue list --state all`
  returns a JSON envelope with `ok=true`.

### T2: F-2 — bump `SUPPORTED_MINOR` 45 → 99

- File: `crates/forge-cli/src/glab_version.rs`
- Change line 18: `pub const SUPPORTED_MINOR: u32 = 45;` → `99`.
- No test changes expected; the existing
  `ensure_supported_rejects_other_minors` test uses `SUPPORTED_MINOR ± 1`
  symbolically.
- Source type: discussion-to-implementation-doc

Acceptance:
- `cargo test -p forge-cli glab_version` is green.
- Manual repro against sandbox repo:
  `forge-cli pr checks <id>` on any MR no longer returns
  `glab_version_unsupported`.

### T3: Build + install local binary

- `cargo build --release -p forge-cli` in the worktree.
- Copy `target/release/forge-cli` to `~/.local/nils-cli/forge-cli`.
- Source type: discussion-to-implementation-doc

Acceptance:
- `forge-cli --version` resolves and reports the rebuilt SHA.
- All sandbox sweep follow-ups (T4) pass.

### T4: Downstream sandbox revalidation

- From `~/Project/gamania/agent-runtime-testing`:
  - Re-run P1 issue list flow.
  - Open a fresh disposable MR, mark ready, run `pr merge --method squash`
    (P6 mode-2 closure).
  - Open another disposable MR, run `pr deliver --no-merge --timeout 30s`
    and confirm the failure mode (if any) is no longer
    `glab_version_unsupported`.
- Update sandbox source doc Findings table marking F-1 / F-2 resolved with
  the PR link.
- Source type: discussion-to-implementation-doc

Acceptance:
- P1 list returns JSON envelope.
- P6 mode-2 returns a `merged=true` envelope.
- P7 deliver advances past the version probe step.
