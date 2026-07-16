# Development Guide

This document is the local development contract for:

- environment setup
- local test/check execution
- local development validation and CI delivery gates

For runtime dependency details and degradation behavior, see `BINARY_DEPENDENCIES.md`.

## 1. Environment setup

### 1.1 Recommended bootstrap

Run once on a new machine:

```bash
scripts/setup-rust-tooling.sh
```

This installs/updates:

- rustup + cargo
- Rust components: `rustfmt`, `clippy`, `llvm-tools-preview`
- `cargo-nextest`
- `cargo-llvm-cov`

### 1.2 Minimum tools required for local checks

All local check flows assume `bash` is available to run repo scripts.

Docs-only checks require:

- `git`
- `npx`
- `plan-tooling`

Local fast changed-scope checks also require:

- `python3`
- `cargo` for non-document changes

CI/full parity checks also require:

- `cargo`
- `python3`
- `zsh`
- `rg`
- `cargo-nextest` when `NILS_CLI_TEST_RUNNER=nextest`

Coverage checks also require:

- `cargo-llvm-cov`
- `cargo-nextest`

For optional runtime tools used by individual CLIs, see `BINARY_DEPENDENCIES.md`.

## 2. Build and quick smoke checks

- Build workspace: `cargo build`
- Example CLI help checks:
  - `cargo run -p nils-cli-template -- --help`
  - `cargo run -p nils-git-scope -- --help`

### 2.1 Codex skill-surface shape checks

`agent-runtime doctor --class skill-surface --product codex` validates
install-map shape only. A passing shape check is not Codex Desktop acceptance;
live acceptance still requires `codex debug prompt-input` in a fresh Codex
Desktop session with `$HOME/.agents` absent and legacy skill environment
variables unset. Rationale: see the archived plan bundle
`agent-plan-archive:plans/github.com/sympoies/nils-cli/2026-05-23-codex-skill-surface-primitives/`.

## 3. Canonical validation flows

Local development defaults to changed-scope validation. The full workspace test
stack and coverage gate are CI responsibilities for normal PRs; run them
locally only when you need CI parity, release-quality verification, coverage
maintenance, or explicit debugging evidence.

Primary local entrypoint for day-to-day implementation work:

```bash
bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast
```

This runs changed-scope validation against `origin/main` by default:

- documentation-only changes use the docs-only lane
- non-shared crate changes run package-scoped `fmt`, `clippy`, and tests
- shared crates and workspace-level files escalate to the workspace Rust gate

Override the base ref when needed:

```bash
bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast --base main
```

Use `--plan-only` to inspect the selected validation scope without running it:

```bash
bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast --plan-only
```

The canonical CI/full-check entrypoint remains:

```bash
bash scripts/ci/nils-cli-checks-entrypoint.sh
```

This delegates to `./.agents/skills/project-verify-required-checks/scripts/project-verify-required-checks.sh`.
It is what CI uses for the full `test` and `test_macos` jobs; it is not the
default local development loop.

### 3.1 Docs-only changes fast path

If all changed files are documentation-only (`*.md`, `docs/**`, `crates/*/docs/**`, root docs like `README.md` and `DEVELOPMENT.md`):

```bash
bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only
```

This also validates touched `docs/plans/**` bundles with:

```bash
bash scripts/ci/plan-bundle-validate.sh --strict
```

The docs-only entrypoint additionally runs the CLI output contract lint
(`docs/specs/cli-output-contract-v1.md`) so envelope and exit-code drift gets
caught even on PRs that only touch documentation:

```bash
bash scripts/ci/cli-output-contract-lint.sh --strict
```

The lint has a self-test under
`scripts/ci/tests/cli-output-contract-lint.test.sh` that exercises every
regression class against synthetic fixtures.

In CI, the `changes` job runs `scripts/ci/detect-docs-only.sh` (sharing the
`scripts/ci/lib/doc_classify.py` classifier with the local-fast planner). When
every changed file is documentation, the `test` and `test_macos` jobs run
`--docs-only` and the `coverage` job skips its steps, so docs-only PRs avoid the
full Rust build/test/coverage cost. The skips are step-level, so all three
release-gated checks still report success.

### 3.2 Local fast changed-scope checks

For most local implementation loops:

```bash
bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast
```

Set `NILS_CLI_TEST_RUNNER=nextest` to require `cargo-nextest`; otherwise the
local fast gate auto-selects `cargo nextest` when available and falls back to
`cargo test`.

The local fast gate is conservative. Changes to `nils-common`, `nils-term`,
`nils-test-support`, root manifests, CI scripts, completions, `.agents/`,
`.github/`, or other workspace-level paths use a workspace Rust gate because
package-scoped checks can miss reverse-dependency breakage.

### 3.3 Full checks (CI gate / optional local parity)

```bash
NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh
```

Notes:

- `nextest` mode runs `cargo nextest run --profile ci --workspace`.
- Because doctests are not included in nextest, the entrypoint also runs
  `cargo test --workspace --doc` when `NILS_CLI_TEST_RUNNER=nextest`.

### 3.4 Full coverage flow (CI gate / explicit local parity)

Coverage gate is mandatory in CI for non-doc changes and in explicit
release-quality verification (total line coverage must stay `>= 85.00%`).
Normal local development does not need to run coverage before opening a PR:

```bash
NILS_CLI_TEST_RUNNER=nextest \
  bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage
```

`--with-coverage` runs, after the full check stack:

```bash
mkdir -p target/coverage
cargo llvm-cov nextest --profile ci --workspace --lcov --output-path target/coverage/lcov.info --fail-under-lines 85
bash scripts/ci/coverage-summary.sh target/coverage/lcov.info
cargo test --workspace --doc
```

Use the default threshold for CI parity. To run a stricter local check, override
the threshold:

```bash
NILS_CLI_COVERAGE_FAIL_UNDER_LINES=90 bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage
```

## 4. Full checks included by the CI entrypoint

`bash scripts/ci/nils-cli-checks-entrypoint.sh` includes:

- `bash scripts/ci/docs-placement-audit.sh --strict`
- `bash scripts/ci/docs-hygiene-audit.sh --strict`
- `bash scripts/ci/markdownlint-audit.sh --strict`
- `bash scripts/ci/plan-bundle-validate.sh --strict`
- `bash scripts/ci/cli-output-contract-lint.sh --strict`
- `bash scripts/ci/forge-cli-fixture-lint.sh --strict`
- `bash scripts/ci/tests/install-local-release-binaries.test.sh`
- `bash scripts/ci/tests/completion-freshness-audit.test.sh`
- `bash scripts/ci/tests/local-fast-checks.test.sh`
- `bash scripts/ci/tests/detect-docs-only.test.sh`
- `bash scripts/ci/tests/detect-release-only.test.sh`
- `node scripts/ci/tests/release-ci-gate.test.cjs`
- `bash scripts/ci/tests/release-workflow-contract.test.sh`
- `bash scripts/ci/tests/shared-helper-adoption-audit.test.sh`
- `bash scripts/ci/tests/publish-order-audit.test.sh`
- `bash scripts/ci/tests/docs-hygiene-audit.test.sh`
- `bash scripts/ci/tests/prepare-private-release-workflow.test.sh`
- `bash scripts/ci/skill-shell-suites.sh` (runs every
  `.agents/skills/*/tests/test_*.sh` smoke suite)
- `bash scripts/ci/test-stale-audit.sh --strict`
- `bash scripts/ci/workspace-version-lockstep.sh --strict`
- `bash scripts/ci/crate-naming-audit.sh`
- `bash scripts/ci/publish-order-audit.sh --strict`
- `bash scripts/ci/third-party-artifacts-audit.sh --strict`
- `bash scripts/ci/completion-asset-audit.sh --strict`
- `bash scripts/ci/completion-freshness-audit.sh --strict`
- `bash scripts/ci/completion-flag-parity-audit.sh --strict`
- `zsh -f tests/zsh/completion.test.zsh`
- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --workspace` (or `cargo nextest run --profile ci --workspace`
  plus `cargo test --workspace --doc` when `NILS_CLI_TEST_RUNNER=nextest`)

## 4.1 Supply-chain audit (cargo-deny)

A dedicated `cargo-deny` GitHub Actions job (`.github/workflows/ci.yml`) runs on
every push and PR, independent of the Rust test jobs. Run the same gate locally
with:

```bash
bash scripts/ci/cargo-deny-audit.sh   # cargo deny check advisories bans
```

It enforces two policies from the root `deny.toml`:

- **advisories** — any RUSTSEC vulnerability / unsound advisory fails the build.
- **bans** — `multiple-versions = "deny"`: a *new* duplicate crate version fails
  the build. Duplicates that exist today only because of in-progress upstream
  ecosystem transitions are recorded in the `deny.toml` `skip` list (a ratchet).

Requires `cargo-deny` (`cargo install cargo-deny --locked` or `brew install
cargo-deny`). When a new duplicate is unavoidable, add a `skip` entry with a
`reason`; to temporarily accept an advisory, add an `ignore` entry with a
`reason`.

## 5. Additional checks when completion assets change

When completion/alias assets are changed, also run:

- `zsh -n completions/zsh/_<cli>`
- `bash -n completions/bash/<cli>`

Canonical completion policy and validation workflow:

- `docs/runbooks/cli-completion-development-standard.md`

## 6. Generated artifacts

Regenerate third-party license/notice artifacts after dependency or metadata
changes:

```bash
bash scripts/generate-third-party-artifacts.sh --write
```

Verify the generated artifacts are current before delivery:

```bash
bash scripts/generate-third-party-artifacts.sh --check
```

The artifact contract is documented in
`docs/specs/third-party-artifacts-contract-v1.md`.

## 7. Test conventions

- In Rust tests, prefer `pretty_assertions::{assert_eq, assert_ne}` for readable diffs.

## 8. CLI version policy

- Every user-facing CLI must expose root `-V, --version`.
- For clap-based CLIs, set `#[command(version)]` on the root `Parser`.
- `--help` output should show `-V, --version`.

## 9. Local install, release, and publishing

### 9.1 Local release install helper

Build and install workspace binaries into `~/.local/nils-cli/bin` by default:

```bash
./scripts/install-local-release-binaries.sh
```

Install only one binary when you are smoke-checking a focused change:

```bash
./scripts/install-local-release-binaries.sh --bin git-scope
```

Add the install directory to `PATH` when needed:

```bash
export PATH="$HOME/.local/nils-cli/bin:$PATH"
```

### 9.2 GitHub release packaging

Release tags matching `v*` trigger `.github/workflows/release.yml`. The workflow
first verifies the tagged commit has green `test`, `test_macos`, and `coverage`
checks, then builds release tarballs for Linux and macOS on x86_64 and aarch64.

Release tarballs include:

- release-default workspace binaries from `scripts/workspace-bins.sh`
- `completions/zsh/` and `completions/bash/`
- `README.md`, `LICENSE`, `THIRD_PARTY_LICENSES.md`, and `THIRD_PARTY_NOTICES.md`

Use the repo-owned release skill for the normal bump, tag, GitHub Release, tap,
and local Homebrew verification flow:

```bash
.agents/skills/project-bump-version-tag-release/scripts/project-bump-version-tag-release.sh --version X.Y.Z
```

`.github/workflows/prepare-private-release.yml` is a narrower preparation-only
entrypoint for the private infrastructure orchestrator. It runs the same
canonical version preparation and locked workspace check on a GitHub-hosted
runner, but uses `--skip-push` and uploads only a patch plus a checksum-bound
manifest. It has read-only repository permissions, accepts no credentials, and
does not create a branch, PR, tag, or release. The private orchestrator must
bind the artifact to the workflow run's immutable `headSha`, independently
validate the patch semantics, and own all delivery mutations.

### 9.3 crates.io publishing

Local crate publish dry-runs and direct publishes use `scripts/publish-crates.sh`.
The default crate order is `release/crates-io-publish-order.txt`.

```bash
scripts/publish-crates.sh --dry-run
scripts/publish-crates.sh --publish
scripts/publish-crates.sh --crates "nils-term nils-common" --dry-run
```

GitHub workflow dispatch is available through `.github/workflows/publish-crates.yml`.
In `publish` mode the workflow requires the repository secret
`CARGO_REGISTRY_TOKEN`.

Use the repo-owned dispatch helper when you want workflow dispatch with run
reporting and post-run crates.io status snapshots:

```bash
.agents/skills/project-dispatch-crates-io-publish/scripts/publish-crates-io.sh --all --wait
```

To query crates.io publish status locally, use:

```bash
scripts/crates-io-status.sh --all --format text
```

Detailed status-script semantics live in
`docs/runbooks/crates-io-status-script-runbook.md`.
