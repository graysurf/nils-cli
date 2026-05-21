# Plan: Agent Workflow Primitive Crate Boundary Cleanup

<!-- markdownlint-disable MD013 -->

## Overview

Fix the workflow primitive publish-order defect and fold the
`test-first-evidence` binary into `nils-agent-workflow-primitives` before
`nils-test-first-evidence` is first published as a standalone package. The
public binary contract stays stable; the package boundary changes.

This plan is intentionally narrow. It does not move `web-evidence` or
`agent-scope-lock`, and it does not perform a release.

## Read First

- Primary source: docs/plans/agent-workflow-primitive-crate-boundary-cleanup/agent-workflow-primitive-crate-boundary-cleanup-discussion-source.md
- Source type: discussion-to-implementation-doc
- Open questions carried into execution:
  - Whether the old crate README is deleted or folded into the multi-binary
    crate docs. Default: fold the public contract into
    `crates/agent-workflow-primitives/README.md` and delete the old package
    docs with the package.
  - Whether the publish dry run covers only the affected pair or the full list.
    Default: use the affected pair during implementation and rely on the full
    required gate before delivery.

## Scope

- In scope:
  - `release/crates-io-publish-order.txt` dependency ordering.
  - Rehoming the `test-first-evidence` binary under
    `crates/agent-workflow-primitives`.
  - Preserving `test-first-evidence` JSON schemas, exit codes, help output,
    record file names, and completion behavior.
  - Removing the standalone `crates/test-first-evidence` package boundary.
  - Updating workspace metadata, docs, generated completion assets, and tests.
- Out of scope:
  - Publishing crates, tagging a release, or bumping Homebrew.
  - Changing `test-first-evidence` record semantics.
  - Renaming the public `test-first-evidence` binary.
  - Moving `web-evidence` or `agent-scope-lock`.
  - Editing downstream `agent-kit` workflows.

## Assumptions

1. `crates/agent-workflow-primitives` is the right package home because it
   already owns the workflow evidence primitive family.
2. `nils-test-first-evidence` has not been published, so removing the package
   boundary does not create a crates.io compatibility obligation.
3. The public compatibility promise is at the binary, schema, exit-code, and
   generated-completion layer.
4. Workspace binary discovery comes from Cargo metadata through
   `scripts/workspace-bins.sh`, so adding the binary target to the multi-binary
   crate is the canonical inventory update.
5. Completion assets remain generated artifacts and should be regenerated or
   validated after the move.

## Sprint 1: Publish order and binary rehome

**Goal**: Put affected crates in publishable dependency order and make
`test-first-evidence` build from `nils-agent-workflow-primitives`.

**Demo/Validation**:

- Commands:
  - `cargo run -p nils-agent-workflow-primitives --bin test-first-evidence -- --help`
  - `cargo test -p nils-agent-workflow-primitives test_first_evidence`
  - `bash scripts/publish-crates.sh --dry-run --crates "nils-term nils-agent-workflow-primitives"`
- Verify: the binary works from the new package and the affected publish dry
  run is not blocked by `nils-term` ordering.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 1.1: Fix affected publish order

- **Location**:
  - release/crates-io-publish-order.txt
- **Description**: Move `nils-term` before
  `nils-agent-workflow-primitives` so the dependent package no longer appears
  before its local dependency. Keep the standalone
  `nils-test-first-evidence` entry only until Task 2 removes the package.
- **Dependencies**:
  - none
- **Complexity**:
  - 2
- **Acceptance criteria**:
  - `nils-term` appears before `nils-agent-workflow-primitives`.
  - No unrelated crate order changes are introduced.
  - Affected dry-run publish ordering no longer fails for the two-crate pair.
- **Validation**:
  - `bash scripts/publish-crates.sh --dry-run --crates "nils-term nils-agent-workflow-primitives"`

### Task 1.2: Register `test-first-evidence` in workflow primitives

- **Location**:
  - crates/agent-workflow-primitives/Cargo.toml
  - crates/agent-workflow-primitives/src/bin/test-first-evidence.rs
  - crates/agent-workflow-primitives/src/test_first_evidence.rs
  - crates/agent-workflow-primitives/src/test_first_evidence/cli.rs
  - crates/agent-workflow-primitives/src/lib.rs
- **Description**: Move the `test-first-evidence` implementation into
  `crates/agent-workflow-primitives`, add a `[[bin]]` target for
  `test-first-evidence`, and preserve the current command dispatch and library
  API shape needed by tests.
- **Dependencies**:
  - Task 1.1
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - `cargo run -p nils-agent-workflow-primitives --bin test-first-evidence -- --help` works.
  - `test-first-evidence -V` reports the workspace crate version.
  - The new binary preserves all current subcommands.
  - The old standalone package can remain present only as a temporary source
    during this task.
- **Validation**:
  - `cargo run -p nils-agent-workflow-primitives --bin test-first-evidence -- --help`
  - `cargo run -p nils-agent-workflow-primitives --bin test-first-evidence -- -V`

### Task 1.3: Port contract tests into the multi-binary crate

- **Location**:
  - crates/agent-workflow-primitives/tests/integration.rs
  - crates/agent-workflow-primitives/tests/integration/cli.rs
  - crates/agent-workflow-primitives/tests/integration/exit_codes.rs
  - crates/agent-workflow-primitives/tests/integration/help_snapshot.rs
  - crates/agent-workflow-primitives/tests/integration/test_first_evidence.rs
  - crates/agent-workflow-primitives/tests/integration/test_first_evidence/cli.rs
  - crates/agent-workflow-primitives/tests/integration/test_first_evidence/exit_codes.rs
  - crates/agent-workflow-primitives/tests/integration/test_first_evidence/help_snapshot.rs
- **Description**: Move the `test-first-evidence` integration coverage under
  `crates/agent-workflow-primitives` and extend the shared binary matrices so
  completion, help, and exit-code checks include `test-first-evidence`.
- **Dependencies**:
  - Task 1.2
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - Existing `test-first-evidence` command tests pass from the new package.
  - Shared workflow primitive completion and help snapshot tests include
    `test-first-evidence`.
  - Exit-code tests continue to assert usage and data error behavior.
- **Validation**:
  - `cargo test -p nils-agent-workflow-primitives test_first_evidence`
  - `cargo test -p nils-agent-workflow-primitives --test integration`

## Sprint 2: Remove stale package boundary and validate artifacts

**Goal**: Delete the old package boundary, refresh metadata and docs, and prove
the public binary contract remains stable from the workspace.

**Demo/Validation**:

- Commands:
  - `bash scripts/workspace-bins.sh | rg '^test-first-evidence$'`
  - `zsh -n completions/zsh/_test-first-evidence`
  - `bash -n completions/bash/test-first-evidence`
  - `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage`
- Verify: the workspace advertises one `test-first-evidence` binary from
  `nils-agent-workflow-primitives`, completion assets are valid, and the full
  required gate passes.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 2.1: Remove standalone crate metadata and publish entry

- **Location**:
  - Cargo.toml
  - Cargo.lock
  - THIRD_PARTY_LICENSES.md
  - THIRD_PARTY_NOTICES.md
  - release/crates-io-publish-order.txt
  - crates/agent-workflow-primitives/Cargo.toml
- **Description**: Remove `crates/test-first-evidence` from workspace members,
  delete the old package files, and remove `nils-test-first-evidence` from the
  crates.io publish order after its binary has moved.
- **Dependencies**:
  - Task 1.3
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - Root workspace metadata no longer lists `crates/test-first-evidence`.
  - `release/crates-io-publish-order.txt` no longer lists
    `nils-test-first-evidence`.
  - Cargo metadata no longer reports a standalone
    `nils-test-first-evidence` package.
  - `test-first-evidence` remains present as a workspace binary.
- **Validation**:
  - `cargo metadata --no-deps --format-version 1`
  - `bash scripts/workspace-bins.sh | rg '^test-first-evidence$'`

### Task 2.2: Fold docs and regenerate completion assets

- **Location**:
  - crates/agent-workflow-primitives/README.md
  - crates/agent-workflow-primitives/docs/README.md
  - README.md
  - docs/specs/completion-coverage-matrix-v1.md
  - completions/bash/test-first-evidence
  - completions/zsh/_test-first-evidence
- **Description**: Move user-facing `test-first-evidence` documentation into
  the multi-binary workflow primitive docs, update completion coverage metadata,
  and regenerate or revalidate the existing completion assets from the new
  package target.
- **Dependencies**:
  - Task 2.1
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - Docs no longer describe `nils-test-first-evidence` as a standalone package.
  - The completion matrix still marks `test-first-evidence` as required.
  - Bash and zsh completion assets validate after the move.
  - README examples continue to use the public `test-first-evidence` binary
    name.
- **Validation**:
  - `zsh -n completions/zsh/_test-first-evidence`
  - `bash -n completions/bash/test-first-evidence`
  - `bash scripts/ci/completion-asset-audit.sh --strict`
  - `bash scripts/ci/completion-flag-parity-audit.sh --strict`

### Task 2.3: Run delivery gates and update tracking issue

- **Location**:
  - docs/plans/agent-workflow-primitive-crate-boundary-cleanup/agent-workflow-primitive-crate-boundary-cleanup-plan.md
  - docs/plans/agent-workflow-primitive-crate-boundary-cleanup/agent-workflow-primitive-crate-boundary-cleanup-discussion-source.md
  - .github/workflows/ci.yml
- **Description**: Run the required repo checks for the final change set and
  update issue #425 with the validation result. Do not create a release in this
  plan.
- **Dependencies**:
  - Task 2.2
- **Complexity**:
  - 2
- **Acceptance criteria**:
  - The plan bundle validates.
  - The full required gate passes for non-docs changes.
  - Issue #425 records the final implementation PR and validation status.
  - Release work is explicitly deferred.
- **Validation**:
  - `plan-tooling validate --file docs/plans/agent-workflow-primitive-crate-boundary-cleanup/agent-workflow-primitive-crate-boundary-cleanup-plan.md`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
  - `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage`
