# Plan: Testing Audits and CLI Contracts

<!-- markdownlint-disable MD013 -->

## Overview

Restore confidence in the testing and audit surfaces called out by issue #421.
The work starts with the shared-helper adoption audit because stale seeded
paths make its output unreliable for future helper extraction, then adds narrow
CLI contract tests for `cli-template` and `agent-runtime-cli`. Runtime behavior
changes are allowed only when the new tests reveal a contract mismatch.

## Read First

- Primary source: docs/plans/testing-audits-cli-contracts/testing-audits-cli-contracts-review-source.md
- Source type: review-to-improvement-doc
- Open questions carried into execution: none

## Scope

- In scope:
  - Refresh `shared-helper-adoption-audit.sh` so seeded candidates match the
    current workspace layout or are generated from current inventory.
  - Add a regression self-test that fails when the shared-helper audit seeds a
    missing file.
  - Strengthen the `cli-template` no-subcommand integration test.
  - Add `agent-runtime-cli` install overlay dry-run CLI coverage for
    no-mutation and effective-plan behavior.
  - Add `agent-runtime-cli` uninstall CLI coverage for foreign symlink recovery
    output and missing link-map error mapping.
- Out of scope:
  - Broad helper extraction or migration work.
  - Reworking the installer or uninstaller beyond fixes required by the new
    operator-facing contract tests.
  - Changing the issue lifecycle for #421 before implementation closeout.

## Assumptions

1. The intended shape is mostly tests and audit-script hardening.
2. The `cli-template` no-subcommand path may intentionally emit no stdout; if
   so, the test should pin that silence rather than inventing output.
3. Non-doc implementation PRs must run the full required gate with coverage.

## Sprint 1: Audit reliability and baseline CLI contract

**Goal**: Make the shared-helper audit trustworthy and pin the weakest
`cli-template` happy path before touching `agent-runtime-cli`.

**Demo/Validation**:

- Commands:
  - `bash scripts/dev/shared-helper-adoption-audit.sh --format tsv --out target/testing-audits-cli-contracts/shared-helper-adoption.tsv`
  - `bash scripts/ci/test-stale-audit.sh --strict`
  - `cargo test -p nils-cli-template --test integration cli_template_runs_without_subcommand`
- Verify: the audit reports against current files, stale seed paths fail a
  deterministic self-test, and the no-subcommand CLI path has meaningful
  stdout/stderr assertions.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 1.1: Refresh shared-helper adoption audit seeds

- **Location**:
  - scripts/dev/shared-helper-adoption-audit.sh
  - scripts/ci/tests/shared-helper-adoption-audit.test.sh
  - scripts/ci/nils-cli-checks-entrypoint.sh
- **Description**: Replace stale seeded test paths with current workspace paths
  or generate candidate rows from current inventory where practical. Add a
  shell self-test that fails when a seeded candidate path does not exist, and
  wire it into the smallest appropriate CI entrypoint if it should guard future
  drift.
- **Dependencies**:
  - none
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - The audit no longer reports `missing-file` for stale seeded integration
    paths that moved to `tests/integration/...`.
  - The self-test fails on a synthetic missing seeded path.
  - `scripts/dev/shared-helper-adoption-audit.sh --format tsv` still writes the
    documented TSV columns.
  - `scripts/ci/test-stale-audit.sh --strict` continues to pass.
- **Validation**:
  - `bash scripts/ci/tests/shared-helper-adoption-audit.test.sh`
  - `bash scripts/dev/shared-helper-adoption-audit.sh --format tsv --out target/testing-audits-cli-contracts/shared-helper-adoption.tsv`
  - `bash scripts/ci/test-stale-audit.sh --strict`

### Task 1.2: Pin cli-template no-subcommand output

- **Location**:
  - crates/cli-template/tests/integration/cli.rs
  - crates/cli-template/src/main.rs
- **Description**: Update `cli_template_runs_without_subcommand` so it asserts
  the intended no-subcommand stdout/stderr contract in addition to exit code 0.
  Change `main.rs` only if the current behavior is not the intended
  operator-facing contract.
- **Dependencies**:
  - none
- **Complexity**:
  - 2
- **Acceptance criteria**:
  - The no-subcommand test asserts stdout and stderr explicitly.
  - The assertion makes the intended default behavior clear to future
    maintainers.
  - Existing `hello`, `status`, help, and parse-error tests keep their current
    contract.
- **Validation**:
  - `cargo test -p nils-cli-template --test integration cli_template_runs_without_subcommand`
  - `cargo test -p nils-cli-template --test integration`

## Sprint 2: Agent-runtime CLI contract coverage

**Goal**: Add operator-facing install/uninstall tests for behavior already
covered only partially or only at the library layer.

**Demo/Validation**:

- Commands:
  - `cargo test -p agent-runtime-cli --test integration`
  - `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage`
- Verify: overlay dry-run no-mutation, effective-plan reporting, foreign
  symlink recovery output, and missing link-map error mapping are all pinned at
  the CLI boundary.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 2.1: Add overlay dry-run CLI no-mutation assertions

- **Location**:
  - crates/agent-runtime-cli/tests/integration/install_flags.rs
  - crates/agent-runtime-cli/src/commands/install.rs
- **Description**: Extend overlay dry-run coverage beyond the stderr
  announcement. Assert that dry-run does not mutate live or state homes and
  that the CLI-visible plan/action summary reflects the overlay-dropped
  effective config.
- **Dependencies**:
  - Task 1.1
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - The overlay dry-run test proves no symlink, managed-block, or backup output
    is written.
  - The CLI summary reflects the post-overlay action count.
  - No-overlay and explicit-overlay-path tests remain distinct.
- **Validation**:
  - `cargo test -p agent-runtime-cli --test integration overlay`

### Task 2.2: Add uninstall CLI recovery and error contract tests

- **Location**:
  - crates/agent-runtime-cli/tests/integration/uninstall.rs
  - crates/agent-runtime-cli/src/commands/uninstall.rs
  - crates/agent-runtime-cli/src/uninstall.rs
- **Description**: Add CLI-level tests that pin foreign symlink recovery output
  and missing link-map error mapping. Reuse the existing library fixtures where
  possible so the new tests prove formatting and exit behavior instead of
  duplicating low-level uninstall logic.
- **Dependencies**:
  - Task 2.1
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - Foreign symlink CLI output names the skipped destination, actual target,
    and expected source.
  - Missing link-map failure exits with the intended code and message shape.
  - Existing library-level recovery tests still pass.
- **Validation**:
  - `cargo test -p agent-runtime-cli --test integration uninstall`

## Final Integration

- Run `cargo fmt --all -- --check`.
- Run `cargo clippy --all-targets --all-features -- -D warnings`.
- Run `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage`.
- Update issue #421 or the plan-tracking issue with the validation summary and
  any intentionally deferred findings.

## Testing Strategy

- Unit: add shell self-test coverage for the shared-helper adoption audit seed
  integrity.
- Integration: strengthen existing Rust integration tests for `cli-template`
  and `agent-runtime-cli`.
- E2E/manual: no browser or manual QA required; this is CLI/test tooling work.

## Risks & gotchas

- `test-stale-audit.sh` and `shared-helper-adoption-audit.sh` are related but
  not equivalent. Passing stale-test audit must not be used as proof that
  shared-helper adoption seeds are current.
- Adding CLI output assertions can overfit incidental formatting. Assert the
  operator-facing contract and avoid pinning temp paths or unstable ordering.
- If production code changes are needed, keep them narrow and rerun the full
  coverage gate because this stops being docs/test-only work.
- New shell tests should be Bash 3.2 compatible because macOS CI may use
  `/bin/bash`.

## Rollback plan

- Revert the plan implementation PR if audit or CLI contract tests prove
  unstable.
- If only the shared-helper self-test is noisy, remove it from the required
  entrypoint while keeping the refreshed seed paths and open a follow-up issue.
