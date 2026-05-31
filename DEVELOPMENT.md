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
- `bash scripts/ci/tests/local-fast-checks.test.sh`
- `bash scripts/ci/tests/detect-docs-only.test.sh`
- `bash scripts/ci/tests/shared-helper-adoption-audit.test.sh`
- `bash scripts/ci/test-stale-audit.sh --strict`
- `bash scripts/ci/workspace-version-lockstep.sh --strict`
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
./scripts/install-local-release-binaries.sh
```
