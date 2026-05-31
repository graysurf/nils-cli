# Plan: Rust Plan-Issue CLI Full Delivery

## Overview
This plan delivers a shell-free Rust implementation for the current plan-issue orchestration workflow, including all existing subcommands and gates. The implementation is local-first: issue content rendering and dry-run execution become independently runnable without GitHub access, while live GitHub mode is added through explicit adapters. The architecture is split to reduce command misuse by agents, improve correctness, and keep long-term maintenance costs lower than the current large Bash plus inline Python scripts. Delivery is sprint-gated: each sprint must pass its acceptance validations before the next sprint starts.

## Scope
- In scope:
  - Build a new Rust crate that covers all current plan-issue orchestration capabilities end to end.
  - Define and implement explicit command contracts for live and local execution.
  - Separate core logic from GitHub adapter logic so issue-body generation and local dry-run are independently executable.
  - Add machine-consumable output mode to reduce agent command parsing mistakes.
  - Add test suites that validate behavior parity for all critical gates and outputs.
- Out of scope:
  - Refactoring old Bash scripts for maintainability as an intermediate step.
  - Redesigning workflow policy semantics beyond current documented contract.
  - Replacing existing workspace issue-lifecycle policy definitions.

## Assumptions
1. Source behavior contract is the existing `plan-issue-delivery-loop.sh` flow and its documented skill contract.
2. The new Rust CLI is built in this workspace and follows `DEVELOPMENT.md` quality gates.
3. `plan-tooling` remains available and is the canonical plan parser/validator for plan markdown.
4. GitHub live mode may use `gh` subprocess adapter in V1 if direct API integration is not required.

## Naming Decision
- Domain statement: this is not a generic plain-issue CLI; it is a plan-issue orchestration CLI.
- Product scope decision: one CLI product (`nils-plan-issue`) with two executables (`plan-issue`, `plan-issue-local`), not separate plain-issue and plan-issue products.
- Crate package name: `nils-plan-issue`.
- Binary 1 (live orchestration): `plan-issue`.
- Binary 2 (local-only execution): `plan-issue-local`.
- Shared internal modules: `core` (pure logic), `adapters/github`, `adapters/local`, and `output`.

## Success criteria
- All current command surfaces are implemented in Rust:
  - `build-task-spec`, `build-plan-task-spec`, `start-plan`, `status-plan`, `ready-plan`, `close-plan`, `cleanup-worktrees`, `start-sprint`, `ready-sprint`, `accept-sprint`, `multi-sprint-guide`.
- Local issue rendering and local dry-run flows run without GitHub calls.
- Live mode enforces sprint and close gates with the same blocking semantics as current contract.
- Usage errors and runtime errors are deterministic and machine-consumable in JSON mode.
- Sprint-by-sprint acceptance checks pass in sequence with no skipped sprint gate.

## Sprint gate policy
- Rule: Sprint N is accepted only when `$AGENT_HOME/out/plan-issue-rust-cli/sprint-N/acceptance.md` exists and records `Result: PASS`.
- Rule: Sprint N+1 cannot start until Sprint N acceptance artifact is present.
- Rule: acceptance artifacts are produced at the end of each sprint and are never deferred.
- Rule: failures must include command, exit code, and key stderr.

## Validation command conventions
- Named test validations must prove the test exists and passes.
- Pattern:
  - `cargo test -p nils-plan-issue -- --list | rg '^test_name:'`
  - `cargo test -p nils-plan-issue test_name -- --exact`
- `test -f` is supplemental and not sufficient for behavior acceptance.

## Sprint 1: Contract freeze and parity baseline
**Goal**: Lock command contract, naming, and parity fixtures before implementation.
**Demo/Validation**:
- Command(s):
  - `plan-tooling validate --file crates/plan-issue/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md`
  - `bash "$AGENT_HOME/skills/automation/plan-issue-delivery-loop/scripts/plan-issue-delivery-loop.sh" --help`
- Verify:
  - Naming decision is documented and stable.
  - Full command contract and exit semantics are written as Rust target spec.
**Parallelizable tasks**:
- `Task 1.2` and `Task 1.3` can run in parallel after `Task 1.1`.

### Task 1.1: Write Rust CLI contract v2 and command matrix
- **Location**:
  - `crates/plan-issue/docs/specs/plan-issue-contract-v2.md`
  - `crates/plan-issue/README.md`
- **Description**: Define command surface, argument rules, mutual exclusivity, exit codes, live vs local mode boundaries, and output schemas for `text` and `json`.
- **Dependencies**:
  - none
- **Complexity**: 5
- **Acceptance criteria**:
  - Contract explicitly maps every existing shell subcommand to Rust command paths.
  - Contract defines required and incompatible flags for both binaries.
  - Contract defines deterministic error envelope for JSON mode.
- **Validation**:
  - `test -f crates/plan-issue/docs/specs/plan-issue-contract-v2.md`
  - `test -f crates/plan-issue/README.md`
  - `rg -n 'build-task-spec|start-plan|start-sprint|close-plan|multi-sprint-guide|format json' crates/plan-issue/docs/specs/plan-issue-contract-v2.md`

### Task 1.2: Capture baseline shell fixtures for parity assertions
- **Location**:
  - `crates/plan-issue/tests/fixtures/shell_parity/help.txt`
  - `crates/plan-issue/tests/fixtures/shell_parity/multi_sprint_guide_dry_run.txt`
  - `crates/plan-issue/tests/fixtures/shell_parity/comment_template_start.md`
- **Description**: Capture stable fixture outputs from current shell workflow for later parity assertions, including help surface, dry-run guide skeleton, and sprint comment structure.
- **Dependencies**:
  - `Task 1.1`
- **Complexity**: 6
- **Acceptance criteria**:
  - Fixtures are deterministic and checked into crate tests.
  - Fixture generation commands are documented.
  - Fixtures cover at least one example for each major output family.
- **Validation**:
  - `test -f crates/plan-issue/tests/fixtures/shell_parity/help.txt`
  - `test -f crates/plan-issue/tests/fixtures/shell_parity/multi_sprint_guide_dry_run.txt`
  - `test -f crates/plan-issue/tests/fixtures/shell_parity/comment_template_start.md`

### Task 1.3: Define state machine and gate invariants
- **Location**:
  - `crates/plan-issue/docs/specs/plan-issue-state-machine-v1.md`
  - `crates/plan-issue/docs/specs/plan-issue-gate-matrix-v1.md`
  - `$AGENT_HOME/out/plan-issue-rust-cli/sprint-1/acceptance.md`
- **Description**: Specify sprint progression state machine, merge gate invariants, close gate invariants, and worktree cleanup pass/fail conditions.
- **Dependencies**:
  - `Task 1.1`
- **Complexity**: 7
- **Acceptance criteria**:
  - State transitions are explicit for start, ready, accept, and close phases.
  - Gate matrix lists required evidence and blocking errors for each transition.
  - Dry-run local semantics are defined independently from live mode.
  - Sprint 1 acceptance artifact exists with `Result: PASS`.
- **Validation**:
  - `test -f crates/plan-issue/docs/specs/plan-issue-state-machine-v1.md`
  - `test -f crates/plan-issue/docs/specs/plan-issue-gate-matrix-v1.md`
  - `rg -n 'start-sprint|accept-sprint|close-plan|worktree cleanup|dry-run local' crates/plan-issue/docs/specs/plan-issue-state-machine-v1.md crates/plan-issue/docs/specs/plan-issue-gate-matrix-v1.md`
  - `test -f "$AGENT_HOME/out/plan-issue-rust-cli/sprint-1/acceptance.md"`

## Sprint 2: Crate scaffold and command skeleton
**Goal**: Create Rust crate and typed command skeleton for both binaries.
**Demo/Validation**:
- Command(s):
  - `cargo test -p nils-plan-issue cli_help`
  - `cargo test -p nils-plan-issue cli_usage_errors`
- Verify:
  - Both binaries exist and parse commands with typed args.
  - Usage errors are stable and documented.
**Parallelizable tasks**:
- `Task 2.2` starts after `Task 2.1`; `Task 2.3` prep can run in parallel but implementation starts after `Task 2.2`.

### Task 2.1: Scaffold `nils-plan-issue` crate with two binaries
- **Location**:
  - `Cargo.toml`
  - `crates/plan-issue/Cargo.toml`
  - `crates/plan-issue/src/main.rs`
  - `crates/plan-issue/src/bin/plan-issue-local.rs`
  - `crates/plan-issue/src/lib.rs`
- **Description**: Create new crate, wire workspace membership, and add two binaries sharing a library entrypoint.
- **Dependencies**:
  - `Task 1.3`
- **Complexity**: 4
- **Acceptance criteria**:
  - Workspace builds with new crate added.
  - Both binaries compile and expose version/help output.
  - Crate metadata follows workspace CLI conventions.
- **Validation**:
  - `cargo check -p nils-plan-issue`
  - `cargo run -p nils-plan-issue -- --help`
  - `cargo run -p nils-plan-issue --bin plan-issue-local -- --help`

### Task 2.2: Implement clap command tree and typed argument model
- **Location**:
  - `crates/plan-issue/src/cli.rs`
  - `crates/plan-issue/src/commands/mod.rs`
  - `crates/plan-issue/src/commands/build.rs`
  - `crates/plan-issue/src/commands/plan.rs`
  - `crates/plan-issue/src/commands/sprint.rs`
- **Description**: Implement complete command and subcommand definitions with mutual exclusion rules and argument validation.
- **Dependencies**:
  - `Task 2.1`
- **Complexity**: 8
- **Acceptance criteria**:
  - All shell command surfaces are represented in clap command tree.
  - Required options and conflicts are enforced at parse time.
  - Help text describes live and local usage paths clearly.
- **Validation**:
  - `cargo test -p nils-plan-issue cli_help`
  - `cargo test -p nils-plan-issue cli_parse_contract`
  - `cargo test -p nils-plan-issue cli_conflict_rules`

### Task 2.3: Add output envelope for text and JSON modes
- **Location**:
  - `crates/plan-issue/src/output/mod.rs`
  - `crates/plan-issue/src/output/text.rs`
  - `crates/plan-issue/src/output/json.rs`
  - `crates/plan-issue/tests/output_contract.rs`
- **Description**: Implement standardized output envelopes so agents consume JSON keys instead of brittle line parsing.
- **Dependencies**:
  - `Task 2.1`
  - `Task 2.2`
- **Complexity**: 6
- **Acceptance criteria**:
  - `--format json` returns versioned envelope with status and payload fields.
  - Error paths include structured code and message fields.
  - Text mode remains human-readable and deterministic.
  - Sprint 2 acceptance artifact exists with `Result: PASS`.
- **Validation**:
  - `cargo test -p nils-plan-issue output_json_contract`
  - `cargo test -p nils-plan-issue output_text_contract`
  - `test -f "$AGENT_HOME/out/plan-issue-rust-cli/sprint-2/acceptance.md"`

## Sprint 3: Local-first core and independent dry-run
**Goal**: Make issue rendering and dry-run fully independent from GitHub.
**Demo/Validation**:
- Command(s):
  - `cargo test -p nils-plan-issue local_mode`
  - `cargo run -p nils-plan-issue --bin plan-issue-local -- build-plan-task-spec --plan crates/plan-issue/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md --pr-grouping per-sprint`
- Verify:
  - Local commands run without `gh` on PATH.
  - Issue body and sprint comment generation are pure local operations.
**Parallelizable tasks**:
- `Task 3.2` and `Task 3.3` can run in parallel after `Task 3.1`.

### Task 3.1: Implement task-spec generation core using `plan-tooling`
- **Location**:
  - `crates/plan-issue/src/core/task_spec.rs`
  - `crates/plan-issue/src/core/plan_meta.rs`
  - `crates/plan-issue/tests/task_spec_generation.rs`
- **Description**: Implement plan/sprint task-spec generation and metadata extraction using deterministic grouping rules and explicit error messages.
- **Dependencies**:
  - `Task 2.2`
  - `Task 2.3`
- **Complexity**: 8
- **Acceptance criteria**:
  - Supports plan and sprint scope generation with `per-sprint` and `group` modes.
  - Task-spec columns remain compatible with downstream orchestration requirements.
  - Invalid grouping or missing mappings fail with deterministic diagnostics.
- **Validation**:
  - `cargo test -p nils-plan-issue task_spec_generation`
  - `cargo test -p nils-plan-issue task_spec_group_validation`

### Task 3.2: Implement issue-body and sprint-comment rendering engine
- **Location**:
  - `crates/plan-issue/src/core/issue_body.rs`
  - `crates/plan-issue/src/core/sprint_comment.rs`
  - `crates/plan-issue/tests/render_issue_body.rs`
  - `crates/plan-issue/tests/render_sprint_comment.rs`
- **Description**: Port markdown renderers from shell behavior into typed Rust rendering engine, including PR normalization and execution-mode projection.
- **Dependencies**:
  - `Task 3.1`
- **Complexity**: 9
- **Acceptance criteria**:
  - Rendered body includes required sections and decomposition table columns.
  - Sprint comments include start, ready, accepted modes with stable markdown layout.
  - PR display normalization follows contract rules.
- **Validation**:
  - `cargo test -p nils-plan-issue render_issue_body`
  - `cargo test -p nils-plan-issue render_sprint_comment`
  - `cargo test -p nils-plan-issue pr_normalization`

### Task 3.3: Implement independent local dry-run workflow
- **Location**:
  - `crates/plan-issue/src/adapters/local/state.rs`
  - `crates/plan-issue/src/adapters/local/filesystem.rs`
  - `crates/plan-issue/src/commands/local.rs`
  - `crates/plan-issue/tests/local_flow.rs`
  - `$AGENT_HOME/out/plan-issue-rust-cli/sprint-3/acceptance.md`
- **Description**: Add local state and command paths for start, ready, accept, close, and cleanup without GitHub calls, using body files and local artifacts.
- **Dependencies**:
  - `Task 3.2`
- **Complexity**: 8
- **Acceptance criteria**:
  - `plan-issue-local` executes full sprint loop offline from plan file and local state.
  - Dry-run mode does not require issue number from GitHub.
  - Local close enforces cleanup gate against local decomposition state.
  - Sprint 3 acceptance artifact exists with `Result: PASS`.
- **Validation**:
  - `cargo test -p nils-plan-issue local_flow`
  - `cargo test -p nils-plan-issue local_cleanup_gate`
  - `test -f "$AGENT_HOME/out/plan-issue-rust-cli/sprint-3/acceptance.md"`

## Sprint 4: Live GitHub adapter and full command parity
**Goal**: Add live orchestration mode with explicit adapter boundaries.
**Demo/Validation**:
- Command(s):
  - `cargo test -p nils-plan-issue github_adapter`
  - `cargo test -p nils-plan-issue command_parity_live`
- Verify:
  - Live commands perform expected GitHub issue and PR operations through adapter layer.
  - Local and live modes share core logic and differ only by adapters.
**Parallelizable tasks**:
- `Task 4.2` and `Task 4.3` can run in parallel after `Task 4.1`.

### Task 4.1: Implement GitHub adapter abstraction and `gh` backend
- **Location**:
  - `crates/plan-issue/src/adapters/github/mod.rs`
  - `crates/plan-issue/src/adapters/github/gh_cli.rs`
  - `crates/plan-issue/src/adapters/github/types.rs`
  - `crates/plan-issue/tests/github_adapter.rs`
- **Description**: Create adapter interface for issue and PR operations with a V1 backend that shells out to `gh` in a controlled and testable way.
- **Dependencies**:
  - `Task 3.3`
- **Complexity**: 8
- **Acceptance criteria**:
  - Adapter supports issue read/update/comment/close and PR merged-status queries.
  - Command execution and stderr mapping are normalized into typed errors.
  - Adapter can be mocked for deterministic integration tests.
- **Validation**:
  - `cargo test -p nils-plan-issue github_adapter`
  - `cargo test -p nils-plan-issue github_error_mapping`

### Task 4.2: Implement live plan-level commands
- **Location**:
  - `crates/plan-issue/src/commands/plan.rs`
  - `crates/plan-issue/src/core/plan_flow.rs`
  - `crates/plan-issue/tests/live_plan_commands.rs`
- **Description**: Implement `start-plan`, `status-plan`, `ready-plan`, `close-plan`, and `cleanup-worktrees` using core plus GitHub adapter.
- **Dependencies**:
  - `Task 4.1`
- **Complexity**: 9
- **Acceptance criteria**:
  - Plan-level commands enforce required options and close gates.
  - Dry-run local compatibility remains available where contract allows.
  - Worktree cleanup semantics match current strict behavior.
- **Validation**:
  - `cargo test -p nils-plan-issue live_plan_commands`
  - `cargo test -p nils-plan-issue close_gate`
  - `cargo test -p nils-plan-issue worktree_cleanup`

### Task 4.3: Implement live sprint-level commands and guide output
- **Location**:
  - `crates/plan-issue/src/commands/sprint.rs`
  - `crates/plan-issue/src/core/sprint_flow.rs`
  - `crates/plan-issue/tests/live_sprint_commands.rs`
  - `crates/plan-issue/tests/multi_sprint_guide.rs`
  - `$AGENT_HOME/out/plan-issue-rust-cli/sprint-4/acceptance.md`
- **Description**: Implement `start-sprint`, `ready-sprint`, `accept-sprint`, and `multi-sprint-guide` with prior-sprint gate checks and dispatch hint output.
- **Dependencies**:
  - `Task 4.1`
- **Complexity**: 9
- **Acceptance criteria**:
  - Previous-sprint merge and done gate is enforced before starting next sprint.
  - Accept-sprint updates status rows and posts acceptance records in live mode.
  - Guide output supports both live and local rehearsal modes.
  - Sprint 4 acceptance artifact exists with `Result: PASS`.
- **Validation**:
  - `cargo test -p nils-plan-issue live_sprint_commands`
  - `cargo test -p nils-plan-issue sprint_gate_enforcement`
  - `cargo test -p nils-plan-issue multi_sprint_guide`
  - `test -f "$AGENT_HOME/out/plan-issue-rust-cli/sprint-4/acceptance.md"`

## Sprint 5: Correctness hardening and anti-misuse guardrails
**Goal**: Reduce operator and agent mistakes while proving parity on edge cases.
**Demo/Validation**:
- Command(s):
  - `cargo test -p nils-plan-issue parity_edge_cases`
  - `cargo test -p nils-plan-issue command_guardrails`
- Verify:
  - High-risk edge cases are covered by tests.
  - CLI guardrails prevent common invalid command combinations.
**Parallelizable tasks**:
- `Task 5.2` and `Task 5.3` can run in parallel after Sprint 4 is accepted.

### Task 5.1: Add parity regression suite against shell fixtures
- **Location**:
  - `crates/plan-issue/tests/parity_shell.rs`
  - `crates/plan-issue/tests/fixtures/shell_parity/help.txt`
  - `crates/plan-issue/tests/fixtures/shell_parity/multi_sprint_guide_dry_run.txt`
  - `crates/plan-issue/tests/fixtures/shell_parity/comment_template_start.md`
- **Description**: Add regression tests that compare critical output fragments and gate outcomes with baseline fixtures captured in Sprint 1.
- **Dependencies**:
  - `Task 4.3`
- **Complexity**: 7
- **Acceptance criteria**:
  - Parity suite covers help, guide, comment rendering, PR normalization, and gate failure messages.
  - Fixture drift requires explicit updates and review.
  - Regression failures are diagnostic and actionable.
- **Validation**:
  - `cargo test -p nils-plan-issue parity_shell`
  - `cargo test -p nils-plan-issue parity_gate_messages`

### Task 5.2: Implement command guardrails and preflight checks
- **Location**:
  - `crates/plan-issue/src/guardrails.rs`
  - `crates/plan-issue/src/commands/common.rs`
  - `crates/plan-issue/tests/command_guardrails.rs`
- **Description**: Add proactive checks for dependency tools, missing files, incompatible flags, and mode misuse, with deterministic diagnostics.
- **Dependencies**:
  - `Task 4.2`
- **Complexity**: 6
- **Acceptance criteria**:
  - Invalid command mixes fail before side effects.
  - Tool preflight clearly reports missing dependencies.
  - Guardrail diagnostics are stable in text and JSON modes.
- **Validation**:
  - `cargo test -p nils-plan-issue command_guardrails`
  - `cargo test -p nils-plan-issue preflight_checks`

### Task 5.3: Add contract-level JSON compatibility tests
- **Location**:
  - `crates/plan-issue/tests/json_contract.rs`
  - `crates/plan-issue/docs/specs/json-contract-v1.md`
  - `$AGENT_HOME/out/plan-issue-rust-cli/sprint-5/acceptance.md`
- **Description**: Define and test JSON envelope compatibility guarantees for automation consumers.
- **Dependencies**:
  - `Task 4.3`
- **Complexity**: 5
- **Acceptance criteria**:
  - Required JSON fields are validated in success and error paths.
  - Sensitive values are not leaked in diagnostics.
  - Backward-compatible additive changes are documented.
  - Sprint 5 acceptance artifact exists with `Result: PASS`.
- **Validation**:
  - `cargo test -p nils-plan-issue json_contract`
  - `test -f crates/plan-issue/docs/specs/json-contract-v1.md`
  - `test -f "$AGENT_HOME/out/plan-issue-rust-cli/sprint-5/acceptance.md"`

## Sprint 6: Cutover, documentation, and release gate
**Goal**: Make Rust CLI the default execution path and complete release-quality checks.
**Demo/Validation**:
- Command(s):
  - `./.agents/skills/nils-cli-verify-required-checks/scripts/nils-cli-verify-required-checks.sh`
  - `mkdir -p target/coverage && cargo llvm-cov nextest --profile ci --workspace --lcov --output-path target/coverage/lcov.info --fail-under-lines 85`
- Verify:
  - Workspace checks pass with new crate and tests.
  - Cutover docs and rollback steps are runnable.
**Parallelizable tasks**:
- `Task 6.1` and `Task 6.2` can run in parallel after `Task 5.3`.

### Task 6.1: Wire skill entrypoint to Rust CLI and keep compatibility wrapper
- **Location**:
  - `wrappers/plan-issue-delivery-loop.sh`
  - `scripts/sync/sync-plan-issue-wrapper-to-agent-home.sh`
  - `crates/plan-issue/docs/runbooks/cutover.md`
- **Description**: Switch operational entrypoint to Rust CLI while keeping a thin compatibility wrapper for transition safety.
- **Dependencies**:
  - `Task 5.1`
  - `Task 5.2`
  - `Task 5.3`
- **Complexity**: 7
- **Acceptance criteria**:
  - Existing skill calls route to Rust CLI path.
  - Compatibility wrapper emits clear deprecation notice and forwards args safely.
  - Cutover runbook documents exact invocation changes.
- **Validation**:
  - `test -f wrappers/plan-issue-delivery-loop.sh`
  - `test -f scripts/sync/sync-plan-issue-wrapper-to-agent-home.sh`
  - `test -f crates/plan-issue/docs/runbooks/cutover.md`
  - `rg -n 'plan-issue|plan-issue-local' wrappers/plan-issue-delivery-loop.sh`

### Task 6.2: Update completion, aliases, and user docs
- **Location**:
  - `completions/zsh/_plan-issue`
  - `completions/bash/plan-issue`
  - `crates/plan-issue/README.md`
  - `crates/plan-issue/docs/README.md`
- **Description**: Add command completion and documentation for both binaries and mode boundaries.
- **Dependencies**:
  - `Task 5.2`
- **Complexity**: 5
- **Acceptance criteria**:
  - Completion files cover subcommands and key options.
  - README documents live vs local mode usage with examples.
  - Docs explain how guardrails reduce command misuse.
- **Validation**:
  - `zsh -n completions/zsh/_plan-issue`
  - `bash -n completions/bash/plan-issue`
  - `test -f crates/plan-issue/README.md && test -f crates/plan-issue/docs/README.md`

### Task 6.3: Final verification and sprint-6 acceptance artifact
- **Location**:
  - `$AGENT_HOME/out/plan-issue-rust-cli/sprint-6/acceptance.md`
  - `$AGENT_HOME/out/plan-issue-rust-cli/final-readiness.md`
- **Description**: Run final required checks, verify Sprint 1 to Sprint 5 acceptance artifacts already exist, and publish Sprint 6 plus final readiness summary.
- **Dependencies**:
  - `Task 6.1`
  - `Task 6.2`
- **Complexity**: 6
- **Acceptance criteria**:
  - Required checks pass.
  - Coverage gate passes.
  - Sprint 1 to Sprint 6 acceptance artifacts exist and summarize validation outcomes.
- **Validation**:
  - `./.agents/skills/nils-cli-verify-required-checks/scripts/nils-cli-verify-required-checks.sh`
  - `mkdir -p target/coverage && cargo llvm-cov nextest --profile ci --workspace --lcov --output-path target/coverage/lcov.info --fail-under-lines 85`
  - `scripts/ci/coverage-summary.sh target/coverage/lcov.info`
  - `for n in 1 2 3 4 5 6; do test -f "$AGENT_HOME/out/plan-issue-rust-cli/sprint-$n/acceptance.md"; done`

## Parallelization Notes
- Parallel after `Task 2.1`: `Task 2.2` plus prep work for `Task 2.3`; `Task 2.3` implementation starts after `Task 2.2`
- Parallel after `Task 3.1`: `Task 3.2`, `Task 3.3`
- Parallel after `Task 4.1`: `Task 4.2`, `Task 4.3`
- Parallel after Sprint 4 completion: `Task 5.2`, `Task 5.3`
- `Task 6.2` can start after `Task 5.2`; `Task 6.1` waits for `Task 5.1`, `Task 5.2`, and `Task 5.3`
- Sequential critical path: `Task 1.1` -> `Task 1.3` -> `Task 2.1` -> `Task 2.2` -> `Task 3.1` -> `Task 3.2` -> `Task 3.3` -> `Task 4.1` -> `Task 4.3` -> `Task 5.1` -> `Task 6.1` -> `Task 6.3`

## Recommended split-prs profile
- Objective: reduce operator/agent error while preserving useful parallelism.
- Profile:
  - Sprint 1: use `group` with 3 groups (`s1-foundation`, `s1-fixtures`, `s1-state`) so `Task 1.2` and `Task 1.3` can proceed independently after `Task 1.1`.
  - Sprint 2: use single-PR grouping (`s2-core`) because the dependency chain is effectively sequential (`Task 2.1` -> `Task 2.2` -> `Task 2.3`).
  - Sprint 3: use single-PR grouping (`s3-core`) because the dependency chain is sequential (`Task 3.1` -> `Task 3.2` -> `Task 3.3`).
  - Sprint 4: use `group` with 3 groups (`s4-adapter`, `s4-live-plan`, `s4-live-sprint`) so `Task 4.2` and `Task 4.3` can run in parallel after `Task 4.1`.
  - Sprint 5: use `group` with 3 groups (`s5-parity`, `s5-guardrails`, `s5-json`) to maximize parallel hardening tracks.
  - Sprint 6: use `group` with 3 groups (`s6-cutover`, `s6-docs`, `s6-final-gate`) so `Task 6.1` and `Task 6.2` can proceed in parallel before `Task 6.3`.
- Reference commands:
  - `plan-tooling split-prs --file crates/plan-issue/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md --scope sprint --sprint 1 --pr-grouping group --pr-group S1T1=s1-foundation --pr-group S1T2=s1-fixtures --pr-group S1T3=s1-state --format tsv`
  - `plan-tooling split-prs --file crates/plan-issue/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md --scope sprint --sprint 2 --pr-grouping group --pr-group S2T1=s2-core --pr-group S2T2=s2-core --pr-group S2T3=s2-core --format tsv`
  - `plan-tooling split-prs --file crates/plan-issue/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md --scope sprint --sprint 3 --pr-grouping group --pr-group S3T1=s3-core --pr-group S3T2=s3-core --pr-group S3T3=s3-core --format tsv`
  - `plan-tooling split-prs --file crates/plan-issue/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md --scope sprint --sprint 4 --pr-grouping group --pr-group S4T1=s4-adapter --pr-group S4T2=s4-live-plan --pr-group S4T3=s4-live-sprint --format tsv`
  - `plan-tooling split-prs --file crates/plan-issue/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md --scope sprint --sprint 5 --pr-grouping group --pr-group S5T1=s5-parity --pr-group S5T2=s5-guardrails --pr-group S5T3=s5-json --format tsv`
  - `plan-tooling split-prs --file crates/plan-issue/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md --scope sprint --sprint 6 --pr-grouping group --pr-group S6T1=s6-cutover --pr-group S6T2=s6-docs --pr-group S6T3=s6-final-gate --format tsv`

## Sprint execution smoothness check
- Derived from `plan-tooling split-prs` dependency notes:
  - Sprint 1: fan-out after `S1T1`; `S1T2` and `S1T3` can run in parallel.
  - Sprint 2: mostly linear (`S2T1` -> `S2T2` -> `S2T3`); keep single PR to reduce merge churn.
  - Sprint 3: linear (`S3T1` -> `S3T2` -> `S3T3`); keep single PR for flow stability.
  - Sprint 4: fan-out after `S4T1`; `S4T2` and `S4T3` can run in parallel.
  - Sprint 5: three tasks can start once Sprint 4 is accepted; strong parallelization potential.
  - Sprint 6: `S6T1` and `S6T2` can run in parallel; `S6T3` is the final gate.
- Net effect:
  - `--scope plan --pr-grouping per-sprint` yields 6 PR groups.
  - Recommended mixed `group` profile yields 14 PR groups and higher parallel throughput where dependencies permit.

## Testing Strategy
- Unit:
  - Parser/renderer/gate functions in `core` modules.
  - Argument and guardrail validation in command layer.
- Integration:
  - Local flow tests for full sprint loop without GitHub.
  - Live mode tests with mocked GitHub adapter.
  - Shell parity regressions using captured fixtures.
- E2E/manual:
  - End-to-end run with one sample plan in local mode.
  - End-to-end run in live mode on a test repository with temporary issue.
- Workspace gates:
  - Required checks and coverage gate from `DEVELOPMENT.md`.

## Risks & gotchas
- Parity risk: shell behavior has many implicit rules that can be missed without fixture coverage.
- Adapter risk: `gh` stderr/stdout differences across environments can affect diagnostics.
- Gate risk: PR normalization and sprint status transitions are easy to regress if parser assumptions drift.
- Usability risk: two binaries can confuse users unless docs and completion are explicit.
- Contract risk: JSON envelope drift can break agent automation if compatibility tests are weak.

## Rollback plan
- Keep cutover reversible by retaining a compatibility wrapper script until Rust CLI proves stable for at least one full sprint loop.
- If live-mode regressions occur, switch entrypoint back to shell wrapper and continue using Rust local mode for non-live tasks.
- Revert only the routing layer first, keep crate code and tests for rapid patching and reattempt.
- Preserve parity fixtures and acceptance artifacts so rollback and re-cutover use the same objective checks.
- If command shape causes operator confusion, temporarily collapse usage to one binary (`plan-issue`) while keeping `plan-issue-local` as internal/test-only until docs are improved.
