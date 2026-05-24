# GitLab MR Unblock Implementation Handoff

| Field | Value |
| --- | --- |
| Status | Ready for implementation |
| Date | 2026-05-25 |
| Source | Downstream sandbox validation in `terrylin/agent-runtime-testing` (gitlab.gamania.com); see Read-first references |
| Intended next step | Land F-1 + F-2 fixes in this plan; F-3 spins out to a separate plan |

## Purpose

A downstream sandbox sweep against `gitlab.gamania.com` revealed six findings in
the `forge-cli` / `plan-issue` skill stack. This source captures the **two
small, self-contained** findings (F-1, F-2) that block GitLab MR delivery via
`forge-cli`. Landing them lets the sandbox finish validating
`pr/create-gitlab-mr`, `pr/close-gitlab-mr` (merge mode), and
`pr/deliver-gitlab-mr` end to end.

The bigger architectural finding (F-3: `plan-issue-cli` is hardwired to `gh`)
needs its own design effort and is **out of scope** here — it gets a follow-up
plan after this lands.

## Confirmed facts

- `forge-cli 0.21.0` ships against `glab 1.45.x` (`crates/forge-cli/src/glab_version.rs` SUPPORTED_MINOR=45). [F1]
- Local `glab` is `1.99.0`; this is the current homebrew bottle. [A1]
- `forge-cli issue list` (GitLab arm) sends `glab issue list ... -F json`. On glab 1.99.0 this command returns text (the tab-separated table), not JSON; `--output json` returns JSON. `crates/forge-cli/src/ops/issue_list.rs:139-140` is the only argv path. [F2]
- The version pin is intentionally one-line bumpable per the in-tree comment: "When `glab` ships a new minor that breaks the text parser, bumping these constants is the one-line tracking change." `crates/forge-cli/src/glab_version.rs:11-12`. [F3]
- `glab ci status` text format has not visibly broken between 1.45 and 1.99 — sandbox repo has no pipeline so this cannot be fully empirically validated, but the parser is unit-tested at the line level and 1.99's CLI surface for `ci status` is unchanged in glab release notes through 1.99. [I1]

## Decisions

1. **F-1 fix**: change the GitLab arm of `build_list_call` from `-F json` to `--output json`. Update the affected unit test (`build_list_call_gitlab_maps_state_to_flag_and_repeats_labels`).
2. **F-2 fix**: bump `SUPPORTED_MINOR` from `45` to `99` in `glab_version.rs`. Update the existing tests that assert the constant (`ensure_supported_rejects_other_minors` already uses `SUPPORTED_MINOR ± 1` so no test change required).
3. **No broader refactor**: do not refactor the version pin into a range (would expand scope). The one-line bump is the canonical maintenance change per existing contract.
4. **F-3 deferred**: open a follow-up tracking issue after this lands. F-3 needs provider abstraction in `plan-issue-cli`, which is multi-PR work.

## Scope

- Edit `crates/forge-cli/src/ops/issue_list.rs` GitLab argv and corresponding test.
- Edit `crates/forge-cli/src/glab_version.rs` `SUPPORTED_MINOR` constant.
- `cargo test -p forge-cli` passes.
- `pre-pr` gate (or the project's local-fast-CI policy equivalent) passes.

## Non-scope

- F-3 / `plan-issue-cli` GitHub-only behavior.
- F-4 `create-dispatch-lane-pr` GitLab port (subsumed by F-3).
- F-5 `semantic-commit` body bullet uppercase rule.
- F-6 `pre-pr` repo-local fallback.
- Reworking the GitLab `ci status` text parser (only the version pin is touched).
- Adding a CI lane that auto-bumps the pin (separate ergonomics work).

## Requirements

- R1. After the patch, `forge-cli issue list` against any GitLab repo with glab 1.99 returns a parseable JSON envelope (empty + non-empty + closed states).
- R2. After the patch, `forge-cli pr checks` / `pr wait-checks` / `pr merge` / `pr deliver` no longer reject glab 1.99 with `glab_version_unsupported`.
- R3. All existing `forge-cli` tests continue to pass.
- R4. Local sandbox revalidation (P1, P6 mode-2 merge, P7 deliver E2E) passes against the rebuilt binary.

## Acceptance criteria

- AC-1. `cargo test -p forge-cli` is green on `feat/gitlab-mr-unblock`.
- AC-2. Rebuilt `forge-cli` binary at `~/.local/nils-cli/forge-cli` resolves `forge-cli --version` and runs all P1 lifecycle commands without `glab_version_unsupported` or JSON parse errors.
- AC-3. Sandbox P1 (`issue list`), P6 mode-2 (`pr merge` of ready MR), and P7 (`pr deliver` E2E) all complete with `ok=true` envelopes.

## Validation plan

1. `cargo test -p forge-cli` (worktree).
2. `cargo build --release -p forge-cli` and copy binary into `~/.local/nils-cli/forge-cli`.
3. From sandbox repo:
   - `forge-cli issue list --state all`
   - Reopen a disposable MR, mark ready, run `pr merge --method squash`
   - Open another disposable MR, run `pr deliver --kind feature --no-merge --timeout 30s` to confirm the wait-checks path no longer hits the version pin (this will still fail at wait-checks if the repo has no pipeline, but the **failure mode** must change from `glab_version_unsupported` to `checks_timeout` or similar).
4. Update sandbox source doc Findings to mark F-1 / F-2 as `resolved (PR #...)`.

## Findings table (this plan)

| ID | Source | Disposition |
| --- | --- | --- |
| F-1 | Sandbox P1 | In scope — landed this plan |
| F-2 | Sandbox P6/P7 | In scope — landed this plan |
| F-3 | Sandbox P8/P9 | Deferred — separate plan after this lands |
| F-4 | Subsumed by F-3 | Deferred |
| F-5 | Sandbox P2 | heuristic-inbox, not this plan |
| F-6 | Sandbox P4 | heuristic-inbox, not this plan |

## Risks and guardrails

- **R-1**: F-2 bump assumes `glab ci status` text format is unchanged 1.45 → 1.99. Mitigation: existing parser unit tests in `pr_checks_gitlab.rs` already exercise the format; if any break post-bump that flags a real parser regression that needs its own fix in this plan.
- **R-2**: Empty-array JSON parse (the original symptom of F-1) must still work. Mitigation: the existing test `parse_list_output_gitlab_accepts_string_or_object_labels` covers parsing; add a separate test asserting empty-array success if not already present.
- **R-3**: Don't accidentally touch the user's WIP on `feat/agent-run-direnv-exec`. Mitigation: this work happens on a separate worktree at `~/.codex/worktrees/gitlab-mr-unblock`.

## Execution

- Recommended plan: docs/plans/gitlab-mr-unblock/gitlab-mr-unblock-plan.md
- Recommended execution state: docs/plans/gitlab-mr-unblock/gitlab-mr-unblock-execution-state.md
- Status: ready
- Next-task source: this plan's Sprint 1

## Retention intent

Promote after merge. The constant-bump pattern documented here (F-2) is the
canonical playbook for future glab minor bumps; consider folding the
walkthrough into `crates/forge-cli/src/glab_version.rs` doc comments if not
already there.

## Read-first references

- Sandbox source doc (downstream):
  `terrylin/agent-runtime-testing:docs/plans/gitlab-skill-validation/gitlab-skill-validation-discussion-source.md`
  — see Findings table for F-1 / F-2 evidence
- `crates/forge-cli/src/ops/issue_list.rs:139-140` (F-1 fix site)
- `crates/forge-cli/src/glab_version.rs:11-18` (F-2 fix site)
- `crates/forge-cli/src/ops/pr_checks_gitlab.rs:43-78` (the only caller of the version gate)

## Source type

`discussion-to-implementation-doc`
