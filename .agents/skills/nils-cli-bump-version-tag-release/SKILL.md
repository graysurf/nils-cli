---
name: nils-cli-bump-version-tag-release
description: Bump CLI versions and tag a release to trigger the CI flow.
---

# Nils CLI Bump Version Tag Release

## Contract

Prereqs:

- Run inside the `nils-cli` git work tree (the script resolves the repo root via `git`).
- `git`, `python3`, `cargo`, `semantic-commit`, and `git-scope` available on `PATH`.
- `gh` available on `PATH` to use the CI-gated fast path (required for strict `--ci-gate-main`).
- `cargo-nextest` available on `PATH` when full release checks are required (`NILS_CLI_TEST_RUNNER=nextest`).
- Release checks available at `.agents/skills/nils-cli-verify-required-checks/scripts/nils-cli-verify-required-checks.sh` (unless `--skip-checks`).

Inputs:

- Required:
  - `--version X.Y.Z` (accepts `vX.Y.Z` and normalizes to `X.Y.Z`)
- Optional:
  - `--skip-checks` (skip full lint/tests; refreshes `Cargo.lock` then runs `cargo check --workspace --locked`)
  - `--ci-gate-main` (strict mode: require CI gate on `main`; fail when gate conditions are not met)
  - `--skip-readme` (do not update README release tag example)
  - `--skip-push` (do not push commit or tag to `origin`)
  - `--allow-dirty` (allow a dirty working tree)
  - `--force-tag` (delete existing local/remote tag before re-tagging)
  - `NILS_CLI_TEST_RUNNER=cargo|nextest` (environment variable; default is `nextest` in this release script)

Default check selection (no `--skip-checks` and no `--ci-gate-main`):

- First try CI gate conditions (`main`, `HEAD == origin/main`, green `ci.yml` run).
- If CI gate passes, refresh `Cargo.lock` and run `cargo check --workspace --locked`.
- If CI gate does not pass, run full release checks via `nils-cli-verify-required-checks.sh`.

Outputs:

- Updates workspace version in `Cargo.toml` and any crate `Cargo.toml` files with explicit `version = "..."`.
- Pins workspace crate-to-crate `path` dependencies to the target version (and adds `version = "X.Y.Z"` when missing).
- If manifests are already at target version, treats version bump as idempotent and continues.
- Updates README release tag examples (unless `--skip-readme`).
- Selects check mode in this order: strict CI gate (`--ci-gate-main`) or auto CI gate attempt, then full checks fallback.
- Refreshes `Cargo.lock` via `cargo generate-lockfile` and then validates via `cargo check --workspace --locked` (CI-gated/skip-check path), or uses the full checks script.
- Automatically disables an incompatible `RUSTC_WRAPPER` (for example a broken `sccache` wrapper) before running release cargo commands.
- Regenerates tracked third-party artifacts (`THIRD_PARTY_LICENSES.md`, `THIRD_PARTY_NOTICES.md`) before strict full-check audits and
  again before commit.
- Runs full release checks through `nils-cli-verify-required-checks.sh` with `NILS_CLI_TEST_RUNNER=nextest` by default (unless overridden).
- Creates a semantic commit for the version bump.
- Creates an annotated tag `vX.Y.Z` and (unless `--skip-push`) pushes commit + tag to `origin`.
- GitHub Release artifacts are built by `.github/workflows/release.yml` and include all workspace `bin` targets (auto-discovered via `scripts/workspace-bins.py`).

Exit codes:

- `0`: success
- `1`: command failed or a prerequisite is missing
- `2`: usage error or invalid inputs

Failure modes:

- Invalid version format or missing `--version`.
- Dirty working tree without `--allow-dirty`.
- Tag already exists without `--force-tag`.
- Required commands missing (`git`, `python3`, `cargo`, `semantic-commit`, `git-scope`).
- `cargo-nextest` missing while default check path (`nextest`) is active.
- Strict `--ci-gate-main` requested but CI gate conditions are not met (`main`, `HEAD == origin/main`, green CI run, `gh` available).
- Release checks or `cargo check` fail.
- Commit or tag creation fails.

## Scripts (only entrypoints)

- `.agents/skills/nils-cli-bump-version-tag-release/scripts/nils-cli-bump-version-tag-release.sh`

## Workflow

- Validate inputs and environment.
- Probe `RUSTC_WRAPPER` and disable it when it is incompatible with the active `rustc`.
- Bump workspace + crate versions and update README.
- Run checks with CI-gate-first logic:
  - `--skip-checks`: refresh `Cargo.lock`; run `cargo check --workspace --locked`.
  - `--ci-gate-main`: require CI gate; then refresh `Cargo.lock`; run `cargo check --workspace --locked`.
  - default: try CI gate first; if unavailable, refresh `Cargo.lock`, regenerate third-party artifacts, then run full checks
    (`nils-cli-verify-required-checks.sh`).
- Regenerate tracked third-party artifacts again before commit to keep release/CI artifacts in sync.
- Commit with `semantic-commit`, tag `vX.Y.Z`, and push to trigger the release workflow.

## Alternate entry points

This skill is also reachable through the Claude Code `/release` slash command,
which dispatches here via `<repo>/.agents/scripts/release.sh` — a thin wrapper
that `exec`s the script above. Args forward unchanged; behaviour is identical
whether you invoke the skill directly or type `/release --version X.Y.Z`.

The skill script remains the canonical implementation per the multi-CLI mirror
rule (see claude-kit's `docs/dispatcher-commands.md`): codex / opencode discover
work through `.agents/skills/`, Claude Code reaches the same logic through the
dispatcher convention.
