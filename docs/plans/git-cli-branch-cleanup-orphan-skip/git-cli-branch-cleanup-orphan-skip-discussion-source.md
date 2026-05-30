# git-cli branch cleanup orphan-skip — Source

| Field              | Value                                                                                                                                                                                                                                                                                  |
| ------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Status             | Ready for plan generation                                                                                                                                                                                                                                                              |
| Date               | 2026-05-30                                                                                                                                                                                                                                                                             |
| Source             | Verification 2026-05-30: running `git-cli branch cleanup --squash` (v0.29.0) against `agent-runtime-kit` hard-aborts on the first orphan-history branch with `❌ Failed to find merge-base`, so the whole sweep cleans nothing. Root-caused in `git-cli` source.                          |
| Intended next step | Generate a plan to skip branches that have no merge-base against `base` (unrelated history) instead of aborting the run, with regression coverage for an orphan branch alongside a real squash-merge.                                                                                   |

## Purpose

`git-cli branch cleanup --squash` is meant to list every local branch
whose changes already landed on `base` (including squash-merges) so they
can be deleted. v0.29.0 (#660) fixed the squash-detection algorithm, but
the per-branch loop still treats any failure of its `git merge-base`
probe as fatal. A repository that contains a branch with unrelated
history — e.g. an orphan test-fixture branch — therefore aborts the
entire sweep before any candidate is listed. This document captures the
confirmed root cause and the agreed fix.

## Confirmed facts

- The squash-mode loop in `crates/git-cli/src/branch.rs` computes, per
  branch, `git merge-base <base> <branch>` and then synthesizes a single
  commit on that merge-base to patch-compare against `base`. The
  `merge-base` result is consumed via `git_stdout_trimmed`, whose `Err`
  arm prints `❌ Failed to find merge-base for <branch> against <base>`
  and `return 1`, aborting the whole run.
- `git merge-base <base> <branch>` exits non-zero when the two refs share
  **no common ancestor** (unrelated history). This is a legitimate state,
  not a tool error: an orphan branch is simply not a squash-merge of
  `base`.
- This abort is **pre-existing**, not introduced by #660. The v0.28.6
  code used `git cherry <base> <branch>`, which also requires a
  merge-base and returned `1` on failure. #660 changed the detection
  algorithm but inherited the same fatal-on-error pattern; it only
  changed the surfaced message from `Failed to compare` to
  `Failed to find merge-base`.
- Observed on `agent-runtime-kit`: 8 orphan `runtime-smoke` fixture
  branches (root commit `4832cbb`, no common ancestor with `main`) cause
  the sweep to die on the first one (`feat/dispatch-lane-runtime-smoke`),
  so the 16 genuinely squash-merged branches in that repo are never
  listed. `agent-runtime-kit` is exactly the repo whose un-cleaned
  branches motivated #660, so the gap defeats the original use case.
- A branch with no merge-base against `base` cannot be a squash-merge of
  `base`, so the correct behavior is to skip it (continue), leaving the
  rest of the sweep to run.

## Options / decision

- **Option A (recommended): skip on no-merge-base.** Change the
  `merge-base` `Err` arm in the squash loop from `return 1` to `continue`.
  A branch the tool cannot relate to `base` is excluded from candidates,
  the sweep finishes, and genuine squash-merges are listed. Lowest risk;
  matches the "not a squash-merge → don't list it" contract.
- **Option B: distinguish exit codes.** `git merge-base` exits `1` for
  "no merge base" and `128` for bad arguments / refs; we could skip only
  on `1` and still error on `128`. Rejected for now: the branch ref comes
  straight from `for-each-ref`, so a `128` is not a realistic state here,
  and `git_stdout_trimmed` does not expose the exit code. If a future
  need arises, switch that one probe to `git_output` and inspect the
  status. Recorded as a non-blocking follow-up, not part of this change.
- **Option C: pre-filter unrelated branches.** Compute the orphan set up
  front and exclude it. Rejected: more code for the same outcome as
  Option A, and it duplicates the per-branch merge-base probe.

Decision: implement Option A. Orphan branches are out of scope for
deletion here — the user can prune them separately; this change only
stops them from aborting the sweep.

## Execution

- Recommended plan: docs/plans/git-cli-branch-cleanup-orphan-skip/git-cli-branch-cleanup-orphan-skip-plan.md
- Recommended execution state: docs/plans/git-cli-branch-cleanup-orphan-skip/git-cli-branch-cleanup-orphan-skip-execution-state.md
