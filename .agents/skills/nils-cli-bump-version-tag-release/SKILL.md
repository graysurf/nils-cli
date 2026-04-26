---
name: nils-cli-bump-version-tag-release
description: Bump CLI versions, tag a release, and bump the homebrew-tap formula end-to-end.
---

# Nils CLI Bump Version Tag Release

## Contract

Prereqs:

- Run inside the `nils-cli` git work tree (the script resolves the repo root via `git`).
- `git`, `python3`, `cargo`, `semantic-commit`, and `git-scope` available on `PATH`.
- `gh` available on `PATH` to use the CI-gated fast path (required for strict `--ci-gate-main`)
  and to wait on `release.yml` runs during the tap stage.
- `cargo-nextest` available on `PATH` when full release checks are required (`NILS_CLI_TEST_RUNNER=nextest`).
- Release checks available at `.agents/skills/nils-cli-verify-required-checks/scripts/nils-cli-verify-required-checks.sh` (unless `--skip-checks`).
- Tap stage prereqs (only when the tap stage runs — auto-skipped otherwise):
  - `curl` and `ruby` on `PATH`. `brew` is recommended; `brew style` is skipped with a warning if absent.
  - A homebrew-tap git work tree resolvable via one of:
    - `--tap-dir <path>` flag,
    - `NILS_CLI_HOMEBREW_TAP_DIR` env var,
    - convention `<nils-cli repo parent>/homebrew-tap`.
  - Tap on `main`, clean, and fast-forwardable from `origin/main`. The script fetches with
    `--no-tags` to avoid stale-tag clobbers (e.g. legacy unprefixed tags).

Inputs:

- Required:
  - `--version X.Y.Z` (accepts `vX.Y.Z` and normalizes to `X.Y.Z`)
- Optional (existing nils-cli stage):
  - `--skip-checks` (skip full lint/tests; refreshes `Cargo.lock` then runs `cargo check --workspace --locked`)
  - `--ci-gate-main` (strict mode: require CI gate on `main`; fail when gate conditions are not met)
  - `--skip-readme` (do not update README release tag example)
  - `--skip-push` (do not push commit or tag to `origin`; **also disables the tap stage**)
  - `--allow-dirty` (allow a dirty working tree)
  - `--force-tag` (delete existing local/remote tag before re-tagging)
  - `NILS_CLI_TEST_RUNNER=cargo|nextest` (environment variable; default is `nextest` in this release script)
- Optional (tap stage):
  - `--tap-dir <path>` (overrides env + convention resolution)
  - `--skip-tap` (skip the tap stage entirely)
  - `--skip-tap-wait` (do not wait for tap `release.yml` after pushing the prefix tag)
  - `--skip-tap-tag` (commit + push tap formula bump but do not create the prefix tag)
  - `--from-tap` (resume mode: skip nils-cli stages 1-8 and run only the tap stage; requires
    an existing local `v<version>` tag in the nils-cli work tree)
  - `--tap-formula <name>` (formula basename, default `nils-cli`; reserved for AWL et al.)
  - `--skip-dev-clean` (do not clear `~/.local/nils-cli/bin` after a successful release)
  - `NILS_CLI_HOMEBREW_TAP_DIR` (env var; tap path)
  - `NILS_CLI_RELEASE_WAIT_SECONDS` (env var; max seconds to wait for source `release.yml`, default 1200)
  - `NILS_CLI_TAP_WAIT_SECONDS` (env var; max seconds to wait for tap `release.yml`, default 1200)

Default check selection (no `--skip-checks` and no `--ci-gate-main`):

- First try CI gate conditions (`main`, `HEAD == origin/main`, green `ci.yml` run).
- If CI gate passes, refresh `Cargo.lock` and run `cargo check --workspace --locked`.
- If CI gate does not pass, run full release checks via `nils-cli-verify-required-checks.sh`.

Default tap stage activation:

- The tap stage runs automatically after a successful nils-cli tag push when ALL of:
  - `--skip-push` is **not** set,
  - `--skip-tap` is **not** set,
  - tap directory resolves via flag/env/convention.
- If no flag/env is set and the conventional path does not exist, the tap stage is skipped
  with a one-line note (no failure). If a flag or env is set but invalid, the script fails loud.

Outputs (nils-cli stage):

- Updates workspace version in `Cargo.toml` and any crate `Cargo.toml` files with explicit `version = "..."`.
- Pins workspace crate-to-crate `path` dependencies to the target version (and adds `version = "X.Y.Z"` when missing).
- If manifests are already at target version, treats version bump as idempotent and continues.
- Updates README release tag examples (unless `--skip-readme`).
- Selects check mode in this order: strict CI gate (`--ci-gate-main`) or auto CI gate attempt, then full checks fallback.
- Refreshes `Cargo.lock` via `cargo generate-lockfile` and then validates via `cargo check --workspace --locked` (CI-gated/skip-check path), or uses the full checks script.
- Automatically disables an incompatible `RUSTC_WRAPPER` (for example a broken `sccache` wrapper) before running release cargo commands.
- Regenerates tracked third-party artifacts (`THIRD_PARTY_LICENSES.md`, `THIRD_PARTY_NOTICES.md`) before strict full-check audits and again before commit.
- Runs full release checks through `nils-cli-verify-required-checks.sh` with `NILS_CLI_TEST_RUNNER=nextest` by default (unless overridden).
- Creates a semantic commit for the version bump.
- Creates an annotated tag `vX.Y.Z` and (unless `--skip-push`) pushes commit + tag to `origin`.
- GitHub Release artifacts are built by `.github/workflows/release.yml` and include all workspace `bin` targets (auto-discovered via `scripts/workspace-bins.py`).

Outputs (tap stage):

- Fetches tap `origin/main` with `--no-tags`, fast-forwards `main`.
- Waits for the source repo's `release.yml` run on tag `vX.Y.Z` to complete `success` (so artifacts exist).
- Parses artifact origin (`<owner>/<repo>`) from the existing formula URL — no hardcoded source.
- Fetches `.tar.gz.sha256` sidecars for all four platforms (`aarch64-apple-darwin`, `x86_64-apple-darwin`, `aarch64-unknown-linux-gnu`, `x86_64-unknown-linux-gnu`) from that origin.
- Rewrites `Formula/<formula>.rb` URL + sha256 lines for those four platforms (idempotent: no-op when already at target).
- Validates with `ruby -c` and (when available) `HOMEBREW_NO_AUTO_UPDATE=1 brew style`.
- Creates a tap-side semantic commit `chore(formula): bump <formula> to v<version>` and pushes `main`.
- Creates an annotated prefix tag `<formula>-v<version>` and pushes it to trigger the tap `release.yml`.
- Unless `--skip-tap-wait`, waits for the tap `release.yml` run on the prefix tag to finish `success`.
- Unless `--skip-dev-clean`, clears `~/.local/nils-cli/bin` so the freshly published brew formula takes precedence over any prior dev install (no-op when the directory is missing or already empty).

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
- Tap stage failures (only when the tap stage runs):
  - `--tap-dir` or `NILS_CLI_HOMEBREW_TAP_DIR` set but does not resolve to a git work tree.
  - `--from-tap` requested without a matching local `v<version>` tag, without an `origin` slug, or without a resolvable tap directory.
  - Tap is dirty, off `main`, or not fast-forward to `origin/main`.
  - `curl` / `ruby` missing on `PATH`.
  - `release.yml` wait exceeds `NILS_CLI_RELEASE_WAIT_SECONDS` (or the run finishes non-success).
  - Formula does not reference all four expected archs (would silently miss platforms otherwise).
  - sha256 sidecar fetch fails or returns non-hex content.
  - `ruby -c` or `brew style` fail on the rewritten formula.
  - `--from-tap` and `--skip-tap` passed together (mutually exclusive).

## Scripts (only entrypoints)

- `.agents/skills/nils-cli-bump-version-tag-release/scripts/nils-cli-bump-version-tag-release.sh`

## Workflow

- Validate inputs and environment.
- Probe `RUSTC_WRAPPER` and disable it when it is incompatible with the active `rustc`.
- `--from-tap` shortcut: skip nils-cli bump+tag and jump to the tap stage.
- nils-cli stage:
  - Bump workspace + crate versions and update README.
  - Run checks with CI-gate-first logic:
    - `--skip-checks`: refresh `Cargo.lock`; run `cargo check --workspace --locked`.
    - `--ci-gate-main`: require CI gate; then refresh `Cargo.lock`; run `cargo check --workspace --locked`.
    - default: try CI gate first; if unavailable, refresh `Cargo.lock`, regenerate third-party artifacts, then run full checks (`nils-cli-verify-required-checks.sh`).
  - Regenerate tracked third-party artifacts again before commit to keep release/CI artifacts in sync.
  - Commit with `semantic-commit`, tag `vX.Y.Z`, and push to trigger the source `release.yml`.
- Tap stage (auto-skipped on `--skip-push` / `--skip-tap` / unresolved tap dir):
  - Verify tap is on clean `main`; fetch `--no-tags` and fast-forward.
  - Wait for source `release.yml` on `vX.Y.Z` to reach `completed success`.
  - Parse artifact origin from existing formula; fetch four `.sha256` sidecars.
  - Rewrite `Formula/<formula>.rb` URL + sha256 lines (idempotent: no-op if already at target).
  - Validate with `ruby -c` (+ `brew style` when available).
  - `semantic-commit` + push `main` (only when formula changed).
  - Create + push annotated prefix tag `<formula>-vX.Y.Z` to trigger tap `release.yml`.
  - Unless `--skip-tap-wait`, wait for tap `release.yml` to finish `success`.

## Alternate entry points

This skill is also reachable through the Claude Code `/release` slash command,
which dispatches here via `<repo>/.agents/scripts/release.sh` — a thin wrapper
that `exec`s the script above. Args forward unchanged; behaviour is identical
whether you invoke the skill directly or type `/release --version X.Y.Z`.

The skill script remains the canonical implementation per the multi-CLI mirror
rule (see claude-kit's `docs/dispatcher-commands.md`): codex / opencode discover
work through `.agents/skills/`, Claude Code reaches the same logic through the
dispatcher convention.
