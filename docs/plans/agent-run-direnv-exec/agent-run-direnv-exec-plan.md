# Plan: agent-run Direnv Exec

## Overview

Implement a released `agent-run` primitive under
`nils-agent-workflow-primitives` so agents can run project build, test, and
validation commands through the project environment contract that developers use.
The first release focuses narrowly on `direnv`-aware command execution,
machine-readable diagnostics, stable exit semantics, completions, and docs.

## Read First

- Primary source:
  `docs/plans/agent-run-direnv-exec/agent-run-direnv-exec-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Source issue: [#467](https://github.com/sympoies/nils-cli/issues/467)
- Open questions carried into execution: none
- Resolved v1 decisions carried into execution:
  - `agent-run env --format json` reports status and paths only; no environment
    diff is emitted in v1.
  - `agent-run exec` stays quiet on successful normal execution and prints
    wrapper output only for warnings or errors.
  - `agent-runtime doctor --check-project` integration is deferred until
    `agent-run` is released and adopted by at least one agent-facing skill.

## Scope

- In scope:
  - Add an `agent-run` binary target in `crates/agent-workflow-primitives`.
  - Implement `agent-run exec --cwd <dir> [--direnv auto|require|off] --
    <command> ...`.
  - Implement `agent-run doctor --cwd <dir> --format text|json`.
  - Implement `agent-run env --cwd <dir> --format json`.
  - Discover applicable `.envrc` / `.env` files for the selected working
    directory.
  - Use `direnv exec` when a project env file applies and `direnv` is available
    and allowed.
  - Fail closed before running the child command when a project env file applies
    but `direnv` is missing, blocked, or not allowed.
  - Preserve child argv boundaries, stdout, stderr, and normal exit status.
  - Add stable wrapper exit-code mapping and JSON error envelopes for JSON
    surfaces.
  - Generate bash/zsh completions and update user-facing docs.
- Out of scope:
  - Running `direnv allow`, `direnv edit`, or any trust-mutating command.
  - Building a task runner, package-manager detector, shell emulator, or
    provider adapter.
  - Persisting env changes across unrelated agent tool calls.
  - Emitting environment variable diffs in v1.
  - Adding `agent-runtime doctor --check-project` integration in the first PR.

## Assumptions

1. `direnv exec <dir> <command> ...` is the right execution primitive because it
   loads the applicable project env and then runs the command without relying on
   interactive shell hooks.
2. Tests can cover allowed, blocked, missing-direnv, and direct execution paths
   with temporary fixture repositories and fake `direnv` binaries on `PATH`.
3. `agent-run` belongs in `crates/agent-workflow-primitives` because it is an
   agent workflow primitive, not an `agent-runtime-kit` render/install/audit
   operation.
4. Existing service JSON envelope conventions in `agent-workflow-primitives`
   are sufficient for `agent-run doctor` and `agent-run env`.

## Sprint 1: CLI Scaffold And Environment Decision Model

**Goal**: Add the `agent-run` CLI surface and a tested decision model before
running child commands.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-agent-workflow-primitives agent_run`
  - `cargo run -p nils-agent-workflow-primitives --bin agent-run -- --help`
  - `bash scripts/workspace-bins.sh | grep '^agent-run$'`
- Verify: the binary appears in workspace inventory, root help exposes
  `exec`, `doctor`, `env`, `completion`, and root `-V, --version`, and the
  decision model classifies direct, direnv, blocked, missing, and bypassed
  cases.

### Task 1.1: Scaffold `agent-run` binary and clap surface

- **Location**:
  - `crates/agent-workflow-primitives/Cargo.toml`
  - `crates/agent-workflow-primitives/src/lib.rs`
  - `crates/agent-workflow-primitives/src/agent_run.rs`
  - `crates/agent-workflow-primitives/src/bin/agent-run.rs`
- **Description**: Add the binary target, root clap parser, subcommands,
  completion subcommand, version support, and help text following existing
  primitive patterns.
- **Dependencies**:
  - none
- **Complexity**: 3
- **Acceptance criteria**:
  - `cargo run -p nils-agent-workflow-primitives --bin agent-run -- --help`
    shows `exec`, `doctor`, `env`, `completion`, and `-V, --version`.
  - `bash scripts/workspace-bins.sh | grep '^agent-run$'` succeeds.
  - Empty or malformed `exec` invocations return a usage error without spawning
    a child process.
- **Validation**:
  - `cargo test -p nils-agent-workflow-primitives agent_run`

### Task 1.2: Implement env-file discovery and direnv decision model

- **Location**:
  - `crates/agent-workflow-primitives/src/agent_run.rs`
- **Description**: Resolve `--cwd`, walk upward to find the applicable `.envrc`
  or `.env`, and classify `--direnv auto|require|off` into a deterministic
  execution decision before any child process starts.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 5
- **Acceptance criteria**:
  - `auto` selects direct execution when no env file applies.
  - `auto` selects direnv execution when an env file applies.
  - `require` fails when no env file applies.
  - `off` bypasses direnv even when an env file applies and records that bypass
    for JSON status.
  - Tests cover nested cwd discovery and avoid hard-coded user-local paths.
- **Validation**:
  - `cargo test -p nils-agent-workflow-primitives agent_run::decision`

### Task 1.3: Detect direnv availability and blocked environment state

- **Location**:
  - `crates/agent-workflow-primitives/src/agent_run.rs`
  - `crates/agent-workflow-primitives/tests/` if integration fixtures are
    needed
- **Description**: Add a `direnv status --json` probe or equivalent stable
  status adapter that detects missing `direnv`, blocked/not-allowed env files,
  and allowed env files without depending on localized prose.
- **Dependencies**:
  - Task 1.2
- **Complexity**: 5
- **Acceptance criteria**:
  - Missing `direnv` is an environment failure only when an env file applies.
  - Blocked or not-allowed env files fail before child execution and name the
    file that needs user review.
  - Allowed env files permit `direnv exec` selection.
  - Unit or integration tests use fake `direnv` output to pin parser behavior.
- **Validation**:
  - `cargo test -p nils-agent-workflow-primitives agent_run::direnv`

## Sprint 2: Command Execution And Machine-Readable Status

**Goal**: Implement the command runner and structured `doctor` / `env` outputs
without adding noisy wrapper output on the success path.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-agent-workflow-primitives --test integration agent_run`
  - `cargo run -p nils-agent-workflow-primitives --bin agent-run -- doctor --cwd . --format json`
  - `cargo run -p nils-agent-workflow-primitives --bin agent-run -- exec --cwd . -- sh -c 'pwd'`
- Verify: direct, direnv, blocked, require, and off-mode cases preserve the
  expected exit behavior and JSON status.

### Task 2.1: Implement direct and off-mode `exec`

- **Location**:
  - `crates/agent-workflow-primitives/src/agent_run.rs`
  - `crates/agent-workflow-primitives/tests/integration/`
- **Description**: Spawn child commands with the selected cwd and argv
  boundaries intact for direct execution and explicit `--direnv off`.
- **Dependencies**:
  - Task 1.2
- **Complexity**: 4
- **Acceptance criteria**:
  - `agent-run exec --cwd <repo-without-envrc> -- <command>` executes directly.
  - `agent-run exec --direnv off --cwd <repo-with-envrc> -- <command>` executes
    directly and records `bypassed` in status surfaces.
  - Child stdout and stderr are streamed without wrapper text on success.
  - Normal child exit codes are preserved.
- **Validation**:
  - `cargo test -p nils-agent-workflow-primitives --test integration agent_run`

### Task 2.2: Implement fail-closed direnv `exec`

- **Location**:
  - `crates/agent-workflow-primitives/src/agent_run.rs`
  - `crates/agent-workflow-primitives/tests/integration/`
- **Description**: Route allowed env-file execution through `direnv exec` and
  fail before child execution for missing, blocked, or not-allowed direnv state.
- **Dependencies**:
  - Task 1.3
  - Task 2.1
- **Complexity**: 6
- **Acceptance criteria**:
  - `agent-run exec --cwd <repo-with-allowed-envrc> -- <command>` invokes
    `direnv exec` with argv boundaries preserved.
  - `agent-run exec --cwd <repo-with-blocked-envrc> -- <command>` fails before
    the child command starts.
  - `agent-run exec --direnv require --cwd fixtures/no-env -- sh -c 'exit 7'`
    fails before child execution and does not return the child status.
  - Wrapper failures use stable exit codes and clear stderr diagnostics.
- **Validation**:
  - `cargo test -p nils-agent-workflow-primitives --test integration agent_run`

### Task 2.3: Implement `doctor` and `env` JSON contracts

- **Location**:
  - `crates/agent-workflow-primitives/src/agent_run.rs`
  - `crates/agent-workflow-primitives/README.md`
- **Description**: Add `agent-run.doctor.v1` and `agent-run.env.v1` payloads
  using existing service JSON envelope conventions. Keep `env` v1 limited to
  status, paths, selected mode, and decision.
- **Dependencies**:
  - Task 1.3
- **Complexity**: 5
- **Acceptance criteria**:
  - `agent-run doctor --cwd <dir> --format json` emits cwd, direnv
    availability, env-file state, selected mode, and decision.
  - `agent-run env --cwd <dir> --format json` states whether project env is
    active, absent, blocked, or bypassed.
  - JSON errors use the standard envelope with stable `code`, `message`, and
    optional `details`.
  - No environment variable values or diffs are emitted in v1.
- **Validation**:
  - `cargo test -p nils-agent-workflow-primitives agent_run::json`

## Sprint 3: Docs, Completions, And Delivery Gate

**Goal**: Make the new primitive discoverable, regenerate completions, and run
the repository gates needed before PR delivery.

**Demo/Validation**:

- Commands:
  - `zsh -n completions/zsh/_agent-run`
  - `bash -n completions/bash/agent-run`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
  - `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage`
- Verify: completions are syntactically valid, docs describe the v1 boundary,
  and the full non-doc gate passes.

### Task 3.1: Document agent-facing usage and binary dependency behavior

- **Location**:
  - `crates/agent-workflow-primitives/README.md`
  - `crates/agent-workflow-primitives/docs/README.md`
  - `BINARY_DEPENDENCIES.md`
  - `docs/runbooks/` if a focused runbook is needed
- **Description**: Document when skills should call `agent-run exec`, how
  `direnv auto|require|off` behaves, why `direnv allow` is never automatic, and
  how to inspect `doctor` / `env` output.
- **Dependencies**:
  - Task 2.3
- **Complexity**: 3
- **Acceptance criteria**:
  - Docs state that `agent-run` is an environment normalizer, not a task runner.
  - Docs show at least one direct command and one `direnv` project example.
  - Docs name the v1 no-env-diff and quiet-success-path decisions.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`

### Task 3.2: Generate and validate completions

- **Location**:
  - `completions/bash/agent-run`
  - `completions/zsh/_agent-run`
  - completion generation scripts or inventory files if required
- **Description**: Generate bash and zsh completion assets for `agent-run` and
  keep workspace completion policy satisfied.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 2
- **Acceptance criteria**:
  - Generated completion files are tracked.
  - `zsh -n completions/zsh/_agent-run` passes.
  - `bash -n completions/bash/agent-run` passes.
- **Validation**:
  - `zsh -n completions/zsh/_agent-run`
  - `bash -n completions/bash/agent-run`

### Task 3.3: Run full gate and prepare PR delivery

- **Location**:
  - workspace root
  - `docs/plans/agent-run-direnv-exec/agent-run-direnv-exec-execution-state.md`
- **Description**: Run targeted tests, completion validation, docs-only gate,
  and full non-doc gate. Update the issue-backed execution state with evidence
  before opening the implementation PR.
- **Dependencies**:
  - Task 2.3
  - Task 3.1
  - Task 3.2
- **Complexity**: 4
- **Acceptance criteria**:
  - Targeted `agent-run` tests pass.
  - Workspace binary inventory includes `agent-run`.
  - Docs-only and full non-doc gates pass or record explicit blockers.
  - Tracking issue state is updated with validation evidence and linked PR.
- **Validation**:
  - `cargo test -p nils-agent-workflow-primitives agent_run`
  - `cargo test -p nils-agent-workflow-primitives --test integration agent_run`
  - `bash scripts/workspace-bins.sh | grep '^agent-run$'`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
  - `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage`

## Validation Summary

- Plan-bundle validation:
  - `plan-tooling validate --file docs/plans/agent-run-direnv-exec/agent-run-direnv-exec-plan.md --format text --explain`
- Docs-only artifact validation:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
- Implementation validation:
  - `cargo test -p nils-agent-workflow-primitives agent_run`
  - `cargo test -p nils-agent-workflow-primitives --test integration agent_run`
  - `cargo run -p nils-agent-workflow-primitives --bin agent-run -- --help`
  - `cargo run -p nils-agent-workflow-primitives --bin agent-run -- doctor --cwd . --format json`
  - `cargo run -p nils-agent-workflow-primitives --bin agent-run -- exec --cwd . -- sh -c 'pwd'`
  - `bash scripts/workspace-bins.sh | grep '^agent-run$'`
  - `zsh -n completions/zsh/_agent-run`
  - `bash -n completions/bash/agent-run`
  - `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage`

## Risk Controls

- Trust mutation remains user-owned: never run `direnv allow`.
- Wrapper decisions are machine-readable, but child output remains unwrapped on
  successful normal execution.
- Tests use fake `direnv` fixtures for deterministic CI behavior and avoid
  requiring a developer's real direnv state.
- `agent-runtime` integration stays out of v1 to keep ownership boundaries
  clean.
