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
a separate plan after this lands.

## Read First

- Primary source:
  `docs/plans/gitlab-mr-unblock/gitlab-mr-unblock-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Open questions carried into execution: none
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

## Sprint 1: Forge-cli unblock

**Goal**: Land F-1 and F-2, install rebuilt binary locally, and verify the
downstream sandbox MR-delivery path on GitLab.

**Demo/Validation**:

- Commands:
  - `cargo test -p forge-cli`
  - `cargo build --release -p forge-cli`
  - From sandbox repo:
    `forge-cli --format json issue list --state all` returns ok=true envelope
- Verify: P1 issue list, P6 mode-2 merge, and P7 deliver advance past the
  version probe step against the rebuilt binary.

### Task 1.1: F-1 — switch GitLab issue list to `--output json`

- **Location**:
  - `crates/forge-cli/src/ops/issue_list.rs`
- **Description**: glab 1.46+ does not honor `-F json` for `issue list` (it
  still emits the human-readable table), but `--output json` works
  consistently. Update the two argv lines in the GitLab arm of
  `build_list_call` and adjust the existing
  `build_list_call_gitlab_maps_state_to_flag_and_repeats_labels` test
  assertion (and add an empty-array parse regression test if not already
  covered).
- **Dependencies**:
  - none
- **Complexity**: 1
- **Acceptance criteria**:
  - `forge-cli --format json issue list --state all` against a GitLab repo
    returns an ok=true envelope on any state (open / closed / all) and on
    empty / non-empty result sets.
  - Unit test asserts the GitLab plan argv includes `--output` + `json`
    instead of `-F` + `json`.
  - Existing JSON parsing tests still pass.
- **Validation**:
  - `cargo test -p forge-cli ops::issue_list`

### Task 1.2: F-2 — bump `SUPPORTED_MINOR` 45 → 99

- **Location**:
  - `crates/forge-cli/src/glab_version.rs`
- **Description**: The text parser for `glab ci status` is intentionally
  pinned to one glab minor. Bump the constant from `45` to `99` so the
  currently shipping homebrew glab is supported. The in-tree doc comment
  already documents this as the canonical one-line maintenance change. The
  parser itself is not touched; if any unit test breaks post-bump that
  signals a real parser regression that warrants a follow-up.
- **Dependencies**:
  - none
- **Complexity**: 1
- **Acceptance criteria**:
  - `cargo test -p forge-cli glab_version` is green.
  - On a host with glab 1.99 installed, `forge-cli pr checks <id>`,
    `pr wait-checks`, `pr merge`, and `pr deliver` no longer fail with
    `glab_version_unsupported`.
- **Validation**:
  - `cargo test -p forge-cli glab_version`

### Task 1.3: Build + install local binary

- **Location**:
  - `~/.local/nils-cli/forge-cli` (install target)
  - `target/release/forge-cli` (build artifact)
- **Description**: Rebuild `forge-cli` with both fixes and copy the binary
  into the user's local install dir so the downstream GitLab sandbox can
  pick it up.
- **Dependencies**:
  - Task 1.1
  - Task 1.2
- **Complexity**: 1
- **Acceptance criteria**:
  - `cargo build --release -p forge-cli` succeeds.
  - `forge-cli --version` from PATH resolves with the rebuilt binary.
- **Validation**:
  - `forge-cli --version`

### Task 1.4: Downstream sandbox revalidation

- **Location**:
  - Downstream sandbox repo: `terrylin/agent-runtime-testing` on
    `gitlab.gamania.com`
- **Description**: Re-run the sandbox sweep phases that F-1 / F-2
  previously blocked. Update the sandbox source doc Findings table marking
  F-1 / F-2 as resolved and linking the upstream PR.
- **Dependencies**:
  - Task 1.3
- **Complexity**: 2
- **Acceptance criteria**:
  - `forge-cli --format json issue list --state all` returns an ok=true
    envelope.
  - `forge-cli pr merge <id> --method squash` on a fresh disposable ready
    MR returns an ok=true envelope.
  - `forge-cli pr deliver --kind feature --no-merge --timeout 30s` advances
    past the version probe step (subsequent failures, if any, are no longer
    `glab_version_unsupported`).
- **Validation**:
  - Sandbox source doc Findings table updated and pushed.
