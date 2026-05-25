---
name: nils-cli-bump-version-tag-release
description: Bump CLI versions, tag a release, and bump the homebrew-tap formula end-to-end.
---

# Nils CLI Bump Version Tag Release

## Contract

Prereqs:

- Run inside the `nils-cli` git work tree (the script resolves the repo root via `git`).
- `git`, `python3`, `cargo`, `semantic-commit`, and `git-scope` available on `PATH`.
- `forge-cli` available on `PATH` for the default PR-based delivery (`forge-cli pr deliver`).
  Falls back to legacy direct-push only when `--direct-push` or `--skip-push` is passed.
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
  - `--direct-push` (opt out of PR-based delivery: commit, tag, and push directly to the current
    branch as the script used to. Fails on branches with required status checks; use PR mode there.)
  - `--release-branch <name>` (override the PR-mode release branch name. Default is
    `chore/release-X-Y-Z` with dots → dashes. Must start with `chore/` to satisfy the
    `forge-cli pr deliver --kind chore` prefix rule.)
  - `--full-checks` (opt-in: run the full local audit stack via `nils-cli-verify-required-checks.sh`
    before commit; slow — only needed for paranoid releases such as toolchain or major dep bumps)
  - `--skip-checks` (deprecated alias of the new default; tolerated for backward-compat callers
    and emits an info note)
  - `--ci-gate-main` (pre-bump strict gate: require the prior `origin/main` commit's `ci.yml` to be
    green; fail when gate conditions are not met)
  - `--skip-readme` (do not update README release tag example)
  - `--skip-push` (do not push commit or tag to `origin`; **implies `--direct-push` semantics
    locally and disables the tap stage and the bump-commit ci.yml wait**)
  - `--skip-ci-wait` (direct-push only: do not wait for `ci.yml` on the bump commit before the
    tap stage. Ignored in PR mode because PR delivery already gates CI before merge.)
  - `--allow-dirty` (allow dirty release-managed files only: Cargo manifests, `Cargo.lock`,
    root `README.md`, and tracked third-party artifacts; fail on other dirty paths)
  - `--force-tag` (delete existing local/remote tag before re-tagging)
  - `NILS_CLI_TEST_RUNNER=cargo|nextest` (environment variable; default is `nextest` in this release script;
    only consulted when `--full-checks` is active)
  - `NILS_CLI_CI_WAIT_SECONDS` (env var; direct-push only: max seconds to wait for the bump commit's
    `ci.yml`, default 1800)
  - `NILS_CLI_PR_WAIT` (env var; PR mode only: budget passed to `forge-cli pr deliver --timeout`,
    default `30m`)
- Optional (tap stage):
  - `--tap-dir <path>` (overrides env + convention resolution)
  - `--skip-tap` (skip the tap stage entirely)
  - `--skip-tap-wait` (do not wait for tap `release.yml` after pushing the prefix tag)
  - `--skip-tap-tag` (commit + push tap formula bump but do not create the prefix tag)
  - `--from-tap` (resume mode: skip nils-cli stages 1-8 and run only the tap stage; requires
    an existing local `v<version>` tag in the nils-cli work tree)
  - `--tap-formula <name>` (formula basename, default `nils-cli`; reserved for AWL et al.)
  - `--skip-dev-clean` (do not clear `~/.local/nils-cli/bin` after a successful release)
  - `--skip-local-brew-upgrade` (do not run `brew update` + `brew upgrade <formula>` for an installed local formula after a successful tap release)
  - `NILS_CLI_HOMEBREW_TAP_DIR` (env var; tap path)
  - `NILS_CLI_RELEASE_WAIT_SECONDS` (env var; max seconds to wait for source `release.yml`, default 1200)
  - `NILS_CLI_TAP_WAIT_SECONDS` (env var; max seconds to wait for tap `release.yml`, default 1200)

Default delivery mode (PR-based):

- Switches from `main` to a fresh `chore/release-X-Y-Z` branch before bumping any versions.
  (If the user is already on that branch, it is reused; any other starting branch is rejected
  so that `--direct-push` is an explicit opt-in.)
- After commit, pushes the branch and invokes `forge-cli pr deliver --kind chore --method squash`,
  which opens a draft PR, waits for required status checks to pass, promotes it to ready, and
  squash-merges into `main` (deleting the source branch).
- Fast-forwards local `main` to the merge commit, creates the annotated tag `vX.Y.Z` on that
  commit, and pushes the tag to trigger `release.yml`.
- The tap stage then runs as before.

Use `--direct-push` to opt out and reuse the legacy "commit + tag directly on the current
branch" path. Use `--skip-push` to keep everything local without opening a PR (also implies
the legacy semantics locally).

Default check selection (no `--full-checks` and no `--ci-gate-main`):

- Refresh `Cargo.lock` via `cargo generate-lockfile`.
- Regenerate tracked third-party artifacts so they match the new lockfile (CI's drift audit will
  reject mismatches on the bump commit).
- Run `cargo check --workspace --locked` to catch lockfile/compile breaks locally.
- No `ci.yml` query before bump — in PR mode, `forge-cli pr deliver` waits for required checks
  before merging; in direct-push mode, the post-push `ci.yml` wait on the bump commit is the
  safety net (see Workflow below).

Use `--full-checks` to additionally run the full audit stack locally
(`nils-cli-verify-required-checks.sh`: clippy, nextest, zsh completion, all CI audit scripts).
Use `--ci-gate-main` to additionally require that the prior `origin/main` commit's `ci.yml` was
green before tagging.

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
- Refreshes `Cargo.lock` via `cargo generate-lockfile` and validates via `cargo check --workspace --locked`. With `--full-checks`, additionally runs the full audit stack via `nils-cli-verify-required-checks.sh`.
- Automatically disables an incompatible `RUSTC_WRAPPER` (for example a broken `sccache` wrapper) before running release cargo commands.
- Regenerates tracked third-party artifacts (`THIRD_PARTY_LICENSES.md`, `THIRD_PARTY_NOTICES.md`) so the bump commit matches CI's drift audit, then refreshes them again before commit.
- Runs `nils-cli-verify-required-checks.sh` with `NILS_CLI_TEST_RUNNER=nextest` by default (only under `--full-checks`).
- Creates a semantic commit for the version bump.
- PR mode (default): pushes the release branch and uses `forge-cli pr deliver --kind chore
  --method squash` to open a draft PR, wait for required checks, promote, and squash-merge.
  Then fast-forwards local `main` to the merge commit, deletes the local release branch,
  creates the annotated tag `vX.Y.Z` on the merge commit, and pushes the tag to `origin`.
- Direct-push mode (`--direct-push` or `--skip-push`): creates the annotated tag `vX.Y.Z` on
  the current branch and (unless `--skip-push`) pushes commit + tag to `origin`. In this mode,
  unless `--skip-ci-wait`, waits for the source repo's `ci.yml` run on the bump commit to
  complete `success` before the tap stage (default 1800s; configurable via
  `NILS_CLI_CI_WAIT_SECONDS`).
- GitHub Release artifacts are built by `.github/workflows/release.yml` and include all workspace `bin` targets (auto-discovered via `scripts/workspace-bins.sh`).

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
- Unless `--skip-local-brew-upgrade`, when Homebrew is available and `<formula>` is installed locally, runs `brew update`, runs `brew upgrade <formula>`, and verifies `brew list --versions <formula>` matches `X.Y.Z`.

Exit codes:

- `0`: success
- `1`: command failed or a prerequisite is missing
- `2`: usage error or invalid inputs

Failure modes:

- Invalid version format or missing `--version`.
- Dirty working tree without `--allow-dirty`, or dirty non-release-managed paths with
  `--allow-dirty`.
- Tag already exists without `--force-tag`.
- Required commands missing (`git`, `python3`, `cargo`, `semantic-commit`, `git-scope`); in PR
  mode also `forge-cli`.
- `cargo-nextest` missing while `--full-checks` is active with the default `nextest` runner.
- Strict `--ci-gate-main` requested but CI gate conditions are not met (`main`, `HEAD == origin/main`, green CI run, `gh` available).
- `--full-checks` audit stack or `cargo check` fail.
- PR mode: starting branch is neither `main` nor the resolved release branch.
- PR mode: `--release-branch <name>` does not start with `chore/`.
- PR mode: the resolved release branch already exists locally (avoid silently reusing stale state).
- PR mode: `forge-cli pr deliver` fails (CI check failure, merge rejection, timeout). The
  pushed release branch is left in place for recovery.
- Direct-push mode: bump-commit `ci.yml` wait fails (non-success conclusion or exceeds
  `NILS_CLI_CI_WAIT_SECONDS`); use `--skip-ci-wait` only if you accept tap formula bump
  without that verification.
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
  - Local Homebrew upgrade fails, or the installed local formula version remains different from `X.Y.Z`.
  - `--from-tap` and `--skip-tap` passed together (mutually exclusive).

## Scripts (only entrypoints)

- `.agents/skills/nils-cli-bump-version-tag-release/scripts/nils-cli-bump-version-tag-release.sh`

## Workflow

- Validate inputs and environment.
- Probe `RUSTC_WRAPPER` and disable it when it is incompatible with the active `rustc`.
- Resolve delivery mode (PR by default; `--direct-push` or `--skip-push` switches to legacy).
- `--from-tap` shortcut: skip nils-cli bump+tag and jump to the tap stage.
- nils-cli stage:
  - Optional pre-bump strict gate: `--ci-gate-main` requires the prior `origin/main` commit's `ci.yml` to be green (otherwise dies).
  - PR mode: switch from `main` to a freshly created `chore/release-X-Y-Z` branch
    (or `--release-branch <name>` override) so the bump commit lands on a feature branch.
  - Bump workspace + crate versions and update README.
  - Refresh `Cargo.lock`, regenerate tracked third-party artifacts, then run `cargo check --workspace --locked`.
  - With `--full-checks`, additionally run the full local audit stack via `nils-cli-verify-required-checks.sh`.
  - Regenerate tracked third-party artifacts again before commit to keep release/CI artifacts in sync.
  - Commit with `semantic-commit`.
  - PR mode: push the release branch and run `forge-cli pr deliver --kind chore --method squash`
    to open + wait + squash-merge. Fast-forward local `main` to the merge commit, then tag
    `vX.Y.Z` on it and push the tag to trigger `release.yml`.
  - Direct-push mode: tag `vX.Y.Z` on the current branch and push commit + tag to trigger
    the source `release.yml` and `ci.yml`. Unless `--skip-ci-wait`, wait for `ci.yml` on the
    bump commit to reach `completed success` before entering the tap stage (use
    `NILS_CLI_CI_WAIT_SECONDS` to tune the timeout).
- Tap stage (auto-skipped on `--skip-push` / `--skip-tap` / unresolved tap dir):
  - Verify tap is on clean `main`; fetch `--no-tags` and fast-forward.
  - Wait for source `release.yml` on `vX.Y.Z` to reach `completed success`.
  - Parse artifact origin from existing formula; fetch four `.sha256` sidecars.
  - Rewrite `Formula/<formula>.rb` URL + sha256 lines (idempotent: no-op if already at target).
  - Validate with `ruby -c` (+ `brew style` when available).
  - `semantic-commit` + push `main` (only when formula changed).
  - Create + push annotated prefix tag `<formula>-vX.Y.Z` to trigger tap `release.yml`.
  - Unless `--skip-tap-wait`, wait for tap `release.yml` to finish `success`.
  - Clear stale dev-install binaries, then upgrade the installed local Homebrew formula when present.

## Alternate entry points

This skill is also reachable through the Claude Code `/release` slash command,
which dispatches here via `<repo>/.agents/scripts/release.sh` — a thin wrapper
that `exec`s the script above. Args forward unchanged; behaviour is identical
whether you invoke the skill directly or type `/release --version X.Y.Z`.

The skill script remains the canonical implementation per the multi-CLI mirror
rule (see claude-kit's `docs/dispatcher-commands.md`): codex / opencode discover
work through `.agents/skills/`, Claude Code reaches the same logic through the
dispatcher convention.
