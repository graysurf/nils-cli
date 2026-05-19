# Development Guide

This document is the local development contract for:

- environment setup
- local test/check execution
- mandatory pre-commit and pre-delivery gates

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

Full required checks also require:

- `cargo`
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

## 3. Canonical local test flows

Primary local entrypoint:

```bash
bash scripts/ci/nils-cli-checks-entrypoint.sh
```

This delegates to `./.agents/skills/nils-cli-verify-required-checks/scripts/nils-cli-verify-required-checks.sh`.

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

### 3.2 CI-like required checks (recommended before push)

```bash
NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh
```

Notes:

- `nextest` mode runs `cargo nextest run --profile ci --workspace`.
- Because doctests are not included in nextest, the entrypoint also runs
  `cargo test --workspace --doc` when `NILS_CLI_TEST_RUNNER=nextest`.

### 3.3 Full pre-delivery flow (required for non-docs changes)

Coverage gate is mandatory for non-doc changes (total line coverage must stay `>= 85.00%`):

```bash
NILS_CLI_TEST_RUNNER=nextest \
  bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage
```

`--with-coverage` runs, after required checks:

```bash
mkdir -p target/coverage
cargo llvm-cov nextest --profile ci --workspace --lcov --output-path target/coverage/lcov.info --fail-under-lines 85
bash scripts/ci/coverage-summary.sh target/coverage/lcov.info
cargo test --workspace --doc
```

Use the default threshold for delivery. To run a stricter local check, override the threshold:

```bash
NILS_CLI_COVERAGE_FAIL_UNDER_LINES=90 bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage
```

## 4. Required checks included by the entrypoint

`bash scripts/ci/nils-cli-checks-entrypoint.sh` includes:

- `bash scripts/ci/docs-placement-audit.sh --strict`
- `bash scripts/ci/docs-hygiene-audit.sh --strict`
- `bash scripts/ci/markdownlint-audit.sh --strict`
- `bash scripts/ci/plan-bundle-validate.sh --strict`
- `bash scripts/ci/cli-output-contract-lint.sh --strict`
- `bash scripts/ci/test-stale-audit.sh --strict`
- `bash scripts/ci/third-party-artifacts-audit.sh --strict`
- `bash scripts/ci/completion-asset-audit.sh --strict`
- `bash scripts/ci/completion-flag-parity-audit.sh --strict`
- `zsh -f tests/zsh/completion.test.zsh`
- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --workspace` (or `cargo nextest run --profile ci --workspace`
  plus `cargo test --workspace --doc` when `NILS_CLI_TEST_RUNNER=nextest`)

## 5. Additional checks when completion assets change

When completion/alias assets are changed, also run:

- `zsh -n completions/zsh/_<cli>`
- `bash -n completions/bash/<cli>`

Canonical completion policy and validation workflow:

- `docs/runbooks/cli-completion-development-standard.md`

## 6. Test conventions

- In Rust tests, prefer `pretty_assertions::{assert_eq, assert_ne}` for readable diffs.

## 7. CLI version policy

- Every user-facing CLI must expose root `-V, --version`.
- For clap-based CLIs, set `#[command(version)]` on the root `Parser`.
- `--help` output should show `-V, --version`.

## 8. Local release install helper

Build and install workspace binaries into `~/.local/nils-cli/`:

```bash
./.agents/skills/nils-cli-install-local-release-binaries/scripts/nils-cli-install-local-release-binaries.sh
```
