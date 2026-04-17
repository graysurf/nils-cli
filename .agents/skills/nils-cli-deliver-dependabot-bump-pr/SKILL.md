---
name: nils-cli-deliver-dependabot-bump-pr
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
- Optionally fast-forward merges `origin/main` into the PR branch (`--sync-main`, on by default).
- Runs `bash scripts/generate-third-party-artifacts.sh --check` to detect drift.
- On drift, runs `bash scripts/generate-third-party-artifacts.sh --write`, derives the bumped dep name from the PR title
  (e.g. `libc`, `rand`), stages `THIRD_PARTY_LICENSES.md` + `THIRD_PARTY_NOTICES.md`, and commits via `semantic-commit`
  with `fix(ci): refresh third-party artifacts for <dep> bump`.
- Pushes the fix commit to the PR branch on `origin` (unless `--skip-push`).
- Waits for CI green via `gh pr checks <N> --watch` (unless `--no-ci-wait`).
- Merges the PR via `gh pr merge <N> --<merge-method>` on CI green (unless `--skip-merge`).
- Restores the starting branch at the end.

Exit codes:

- `0`: success (drift fixed + merged, or no drift + merged, or explicit skip paths completed).
- `1`: failure (prerequisite missing, CI failed, merge blocked, push rejected, etc.).
- `2`: usage error or invalid inputs.

Failure modes:

- `--pr` missing or not a positive integer.
- PR not found or not open.
- PR author is not `dependabot[bot]` (without `--allow-non-dependabot`).
- Head branch is not `dependabot/*` (without `--allow-non-dependabot`).
- Working tree is dirty on the starting branch.
- `scripts/generate-third-party-artifacts.sh --check` fails with a non-drift error (exit code not in `{0,1}`).
- `semantic-commit` validation fails.
- `git push` rejected (e.g. dependabot rebased; retry manually with `@dependabot rebase` then re-run).
- CI concludes with failure, cancelled, or timeout under `gh pr checks --watch`.
- `gh pr merge` rejected (branch protection / required reviews pending).

## Scripts (only entrypoints)

- `.agents/skills/nils-cli-deliver-dependabot-bump-pr/scripts/nils-cli-deliver-dependabot-bump-pr.sh`

## Workflow

1. Validate inputs and environment (`gh auth status`, required binaries, clean tree).
2. Resolve PR metadata via `gh pr view <N> --json number,state,title,headRefName,author,isCrossRepository`.
3. Enforce dependabot guard (author + branch prefix), unless `--allow-non-dependabot`.
4. `gh pr checkout <N>` (records starting branch for later restoration).
5. On `--sync-main`, `git fetch origin main` and `git merge --ff-only origin/main` — abort on non-fast-forward.
6. Run the drift check; on drift:
   a. `bash scripts/generate-third-party-artifacts.sh --write`.
   b. `git add THIRD_PARTY_LICENSES.md THIRD_PARTY_NOTICES.md`.
   c. `printf 'fix(ci): refresh third-party artifacts for %s bump\n\n- Regenerate THIRD_PARTY_LICENSES.md and THIRD_PARTY_NOTICES.md via scripts/generate-third-party-artifacts.sh --write after the %s bump.\n' "$dep" "$dep" | semantic-commit commit`.
   d. `git push origin HEAD` (unless `--skip-push`).
7. On `--no-ci-wait` + not merging: exit 0.
8. Wait for CI via `gh pr checks <N> --watch`; bail on non-success conclusion.
9. On `--skip-merge`: exit 0.
10. `gh pr merge <N> --squash` (or configured merge method); restore starting branch.

## Reference

PR #307 (`chore(deps): bump libc from 0.2.182 to 0.2.183`) is the canonical example of this flow:

- Dependabot updates `Cargo.lock` + `Cargo.toml` for the direct dep.
- CI fails on `scripts/ci/third-party-artifacts-audit.sh --strict` because `Cargo.lock` SHA256 is embedded in both markdown artifacts.
- Maintainer pushes `fix(ci): refresh third-party artifacts for libc bump` (and a second refresh commit after re-merging `main`).
- CI goes green; PR is squash-merged.

See: https://github.com/sympoies/nils-cli/pull/307
