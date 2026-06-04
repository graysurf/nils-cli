---
name: project-deliver-dependabot-bump-pr
description: Fix a dependabot bump PR whose CI fails because THIRD_PARTY_LICENSES.md / THIRD_PARTY_NOTICES.md drifted, then wait for CI green and merge.
---

# Nils CLI Deliver Dependabot Bump PR

## Contract

Prereqs:

- Run inside the `nils-cli` git work tree (the script resolves the repo root via `git`).
- `bash`, `git`, `cargo`, `python3`, `gh`, and `semantic-commit` available on `PATH`.
- `gh auth status` passes for the `sympoies/nils-cli` repo.
- Working tree is clean on the start branch (the script will abort on dirty tree).
- The following repo helpers exist:
  - `scripts/generate-third-party-artifacts.sh`
  - `scripts/ci/third-party-artifacts-audit.sh`

Inputs:

- Required:
  - `--pr <N>`: dependabot PR number to deliver.
  - `--all-open`: deliver all open Dependabot PRs sequentially. Use this when
    the queue has multiple artifact-drift bumps; it must start from `main`.
- Optional:
  - `--sync-main` (default): fast-forward merge `origin/main` into the PR branch before refresh.
  - `--no-sync-main`: keep branch as-is; only regenerate artifacts on current commit.
  - `--skip-merge`: push the fix commit but do not merge the PR (useful when merge requires manual review).
  - `--skip-push`: generate and commit the refresh locally only (for dry-run / review).
  - `--merge-method squash|merge|rebase` (default `squash`): method passed to `gh pr merge`.
  - `--no-ci-wait`: do not block on CI after pushing the refresh commit (let dependabot-merge auto-flow handle it).
  - `--allow-non-dependabot`: skip the dependabot-author guard (default refuses non-dependabot PRs).

Outputs:

- Checks out the PR head branch via `gh pr checkout <N>`.
- Enforces the PR is authored by `dependabot[bot]` and the head branch matches `dependabot/*` (unless `--allow-non-dependabot`).
- Optionally fast-forward merges `origin/main` into the PR branch (`--sync-main`, on by default). If that is not possible, it requests
  `@dependabot rebase`, waits for the PR head SHA to change, then checks out the rebased branch with `gh pr checkout --force`.
- Runs `bash scripts/generate-third-party-artifacts.sh --check` to detect drift.
- On drift, runs `bash scripts/generate-third-party-artifacts.sh --write`, derives the bumped dep name from the PR title
  (e.g. `libc`, `rand`), stages `THIRD_PARTY_LICENSES.md` + `THIRD_PARTY_NOTICES.md`, and commits via `semantic-commit`
  with `fix(ci): refresh third-party artifacts for <dep> bump` and a body line short enough for the commit-body gate.
- Pushes the fix commit to the PR branch on `origin` (unless `--skip-push`). If the push is rejected because the Dependabot branch moved,
  fetches the latest PR head, rebases only the refresh commit(s) onto it, and retries.
- Waits for CI green via `gh pr checks <N> --watch` (unless `--no-ci-wait`). If GitHub has not attached checks to the PR summary yet,
  falls back to `gh run list` + `gh run watch` for the PR head SHA.
- Merges the PR via `gh pr merge <N> --<merge-method>` on CI green (unless `--skip-merge`).
- Restores the starting branch at the end. In `--all-open` mode, syncs `main` after each successful merge before delivering the next PR.

Exit codes:

- `0`: success (drift fixed + merged, or no drift + merged, or explicit skip paths completed).
- `1`: failure (prerequisite missing, CI failed, merge blocked, push rejected, etc.).
- `2`: usage error or invalid inputs.

Failure modes:

- Neither `--pr` nor `--all-open` supplied, or both supplied.
- `--pr` is not a positive integer.
- `--all-open` is run from a branch other than `main`.
- PR not found or not open.
- PR author is not `dependabot[bot]` (without `--allow-non-dependabot`).
- Head branch is not `dependabot/*` (without `--allow-non-dependabot`).
- Working tree is dirty on the starting branch.
- `scripts/generate-third-party-artifacts.sh --check` fails with a non-drift error (exit code not in `{0,1}`).
- `semantic-commit` validation fails.
- `git push` is still rejected after the configured retry count, or the refresh commit cannot be rebased cleanly onto the updated PR head.
- CI concludes with failure, cancelled, or timeout under both the PR check watcher and the workflow-run fallback.
- `gh pr merge` rejected (branch protection / required reviews pending).

## Scripts (only entrypoints)

- `.agents/skills/project-deliver-dependabot-bump-pr/scripts/project-deliver-dependabot-bump-pr.sh`

## Workflow

1. Validate inputs and environment (`gh auth status`, required binaries, clean tree).
2. Resolve PR metadata via `gh pr view <N> --json number,state,title,headRefName,headRefOid,author,isCrossRepository`.
3. Enforce dependabot guard (author + branch prefix), unless `--allow-non-dependabot`.
4. In `--all-open` mode, list open Dependabot PRs, then invoke this same script once per PR. After each non-skip merge, restore `main`
   and fast-forward to `origin/main`.
5. `gh pr checkout <N> --force` (records starting branch for later restoration).
6. On `--sync-main`, `git fetch origin main` and `git merge --ff-only origin/main`; on non-fast-forward, comment `@dependabot rebase`,
   wait for `headRefOid` to change, then checkout the rebased PR head and retry the fast-forward check.
7. Run the drift check; on drift:
   a. `bash scripts/generate-third-party-artifacts.sh --write`.
   b. `git add THIRD_PARTY_LICENSES.md THIRD_PARTY_NOTICES.md`.
   c. `printf 'fix(ci): refresh third-party artifacts for %s bump\n\n- Regenerate third-party artifacts after the %s bump.\n' "$dep" "$dep" | semantic-commit commit`.
   d. `git push origin HEAD` (unless `--skip-push`); if rejected, fetch the current PR head, `git rebase --onto` the refresh commit(s),
      and retry.
8. On `--no-ci-wait` + not merging: exit 0.
9. Wait for CI via `gh pr checks <N> --watch`; if no checks are reported yet, select the matching pull-request CI run by head SHA and
   wait with `gh run watch`.
10. On `--skip-merge`: exit 0.
11. `gh pr merge <N> --squash` (or configured merge method); restore starting branch.

## Reference

PR #307 (`chore(deps): bump libc from 0.2.182 to 0.2.183`) is the canonical example of this flow:

- Dependabot updates `Cargo.lock` + `Cargo.toml` for the direct dep.
- CI fails on `scripts/ci/third-party-artifacts-audit.sh --strict` because `Cargo.lock` SHA256 is embedded in both markdown artifacts.
- Maintainer pushes `fix(ci): refresh third-party artifacts for libc bump` (and a second refresh commit after re-merging `main`).
- CI goes green; PR is squash-merged.

See: https://github.com/sympoies/nils-cli/pull/307
