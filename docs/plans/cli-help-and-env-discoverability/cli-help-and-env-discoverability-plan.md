# Plan: CLI Help and Env Discoverability

## Overview

Land a help-quality style guide for the workspace, then roll it out across
clap-derive binaries in a single batched sprint. Sprint 1 publishes the
style guide and migrates `memo-cli` end to end as the reference
implementation. Sprint 2 fans the same shape out to every other clap-derive
binary and resolves the `api-gql` implicit-default UX decision.

## Read First

- Primary source:
  docs/plans/cli-help-and-env-discoverability/cli-help-and-env-discoverability-review-source.md
- Source type: review-to-improvement-doc
- Open questions carried into execution:
  - Are `EXIT CODES` blocks mandatory on every binary? Default: yes, consistency outweighs the small extra cost.
  - For `api-gql`: document the implicit `call` default, or remove the fallback entirely? Default: document the default (no breaking change).

## Scope

- In scope:
  - `docs/runbooks/cli-help-style-guide.md` (new) — the style guide.
  - Per-binary clap-layer updates: `#[arg(env = ...)]` on env-backed flags, `long_about`, `after_help` (with EXAMPLES and EXIT CODES / ENVIRONMENT sections), and `conflicts_with` for `--json` ↔ `--format` overlap.
  - Per-binary help-structure snapshot tests under `tests/integration/help_snapshot.rs` for clap-derive binaries.
  - `api-gql` implicit-default UX fix.
- Out of scope:
  - Hand-rolled-dispatch binaries (`semantic-commit`, `git-cli`, `plan-tooling`, `git-summary`, `fzf-cli`) — covered by the `cli-dispatch-modernization` plan.
  - JSON envelope / exit-code consolidation — covered by the `cli-output-contract-unification` plan.
  - Adding new subcommands or behaviours.

## Assumptions

1. Clap's `env = "..."` attribute renders the env-var name in `--help`
   automatically when the flag is shown.
2. `--help` snapshot tests can use a structural matcher (section
   headings, env var names) instead of exact wording.
3. The dispatch-modernization plan migrates hand-rolled binaries onto
   clap derive before this plan needs to cover them.

## Sprint 1: Style guide and pilot migration

**Goal**: Publish the style guide and prove the shape with one
moderately-complex binary, so the rest of the fan-out becomes mechanical.

**Demo/Validation**:

- Commands:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
  - `cargo test -p memo-cli help_snapshot`
  - Manual: `memo-cli --help` (shows ABOUT, USAGE, COMMANDS, FLAGS with env vars annotated, EXAMPLES, ENVIRONMENT, EXIT CODES).
  - Manual: `memo-cli --json --format text list` (fails at parse time with exit 64).
- Verify: the style guide describes every required section, and
  `memo-cli`'s help output matches that shape end to end.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 1.1: Draft `cli-help-style-guide.md`

- **Location**:
  - docs/runbooks/cli-help-style-guide.md
- **Description**: Define the canonical `--help` shape: short `about`
  (one sentence), `long_about` (paragraph), `after_help` with EXAMPLES
  (at least one bash invocation per major flow), ENVIRONMENT (every env
  var the binary reads or writes), and EXIT CODES (table). Document the
  required clap attributes for env-backed flags (`#[arg(env = "...")]`)
  and for flag-conflict declarations (`conflicts_with`). Include the
  contract for the help-snapshot tests (lock structure, not wording).
  Cross-link the `cli-output-contract-unification` plan as the source
  for the exit-code table.
- **Dependencies**:
  - none
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - Runbook lives at `docs/runbooks/cli-help-style-guide.md`.
  - All required sections are documented with one example each.
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` passes.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`

### Task 1.2: Migrate `memo-cli` to the style guide

- **Location**:
  - crates/memo-cli/src/cli.rs
  - crates/memo-cli/src/commands/
- **Description**: Add `long_about` to the binary and every subcommand,
  `after_help` with EXAMPLES / ENVIRONMENT / EXIT CODES sections on the
  root binary, `#[arg(env = ...)]` on any env-backed flag (e.g. the
  default DB path can move from `default_value_os_t` to env-backed
  fallback when appropriate), and `conflicts_with = "format"` on
  `--json`. Delete the runtime conflict check in `resolve_output_mode`
  once clap takes over (or convert it to a parse-time test).
- **Dependencies**:
  - Task 1.1
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - `memo-cli --help` contains ABOUT, USAGE, COMMANDS, FLAGS, EXAMPLES, ENVIRONMENT, EXIT CODES sections.
  - `memo-cli --json --format text list` fails at parse time with exit `64` (clap usage error).
  - Existing `memo-cli` integration tests still pass.
- **Validation**:
  - `cargo test -p memo-cli`
  - Manual: `memo-cli --help` and `memo-cli list --help`

### Task 1.3: Add structural help-snapshot test for `memo-cli`

- **Location**:
  - crates/memo-cli/tests/integration/help_snapshot.rs (new)
- **Description**: Capture `memo-cli --help` output and assert the
  presence (not exact wording) of every required section heading and
  every env var name the style guide requires. Use a contains-style
  matcher; do not lock the line-by-line text.
- **Dependencies**:
  - Task 1.2
- **Complexity**:
  - 2
- **Acceptance criteria**:
  - Snapshot test passes for `memo-cli --help`.
  - Adding a new env var that the binary reads but does not document fails the test.
- **Validation**:
  - `cargo test -p memo-cli help_snapshot`

## Sprint 2: Fan-out and `api-gql` implicit default fix

**Goal**: Apply the style guide to every other clap-derive user-facing
binary and resolve the `api-gql` implicit-default behaviour.

**Demo/Validation**:

- Commands:
  - `cargo test --workspace help_snapshot`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh`
  - Manual: `<binary> --help` on every user-facing binary; verify all sections are present and env vars are annotated.
- Verify: every clap-derive binary's help has the canonical shape; the
  workspace gate stays green.

**PR grouping intent**: `group`
**Execution Profile**: `parallel-x3`

### Task 2.1: Apply style guide to `agent-workflow-primitives` binaries

- **Location**:
  - crates/agent-workflow-primitives/src/
  - crates/agent-workflow-primitives/tests/integration/help_snapshot.rs (new)
- **Description**: All eight binaries already use `after_help`; bring
  their `long_about`, ENVIRONMENT, and EXIT CODES sections up to the
  style guide and add structural help-snapshot tests.
- **Dependencies**:
  - Task 1.1
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - Every binary in the crate has the four sections.
  - Help-snapshot tests cover every binary.
  - `cargo test -p agent-workflow-primitives` passes.
- **Validation**:
  - `cargo test -p agent-workflow-primitives help_snapshot`

### Task 2.2: Apply style guide to API testing binaries and surface env vars

- **Location**:
  - crates/api-rest/src/cli.rs
  - crates/api-gql/src/cli.rs
  - crates/api-grpc/src/cli.rs
  - crates/api-websocket/src/cli.rs
  - crates/api-test/src/main.rs
  - crates/api-testing-core/src/
  - crates/api-rest/tests/integration/
  - crates/api-gql/tests/integration/
  - crates/api-grpc/tests/integration/
  - crates/api-websocket/tests/integration/
  - crates/api-test/tests/integration/
- **Description**: Add `long_about`, `after_help`, env annotations, and
  `EXIT CODES` blocks. Audit every `std::env::var` /
  `std::env::var_os` read in `api-gql/src/commands/*.rs` (`GQL_*`
  family) and `api-testing-core` and surface them either via
  `#[arg(env = ...)]` (for per-flag overrides) or via the
  ENVIRONMENT section (for binary-wide knobs).
- **Dependencies**:
  - Task 1.1
- **Complexity**:
  - 6
- **Acceptance criteria**:
  - Every API binary's `--help` lists every env var it reads.
  - Help-snapshot tests cover every binary.
  - `cargo test -p api-rest`, `-p api-gql`, `-p api-grpc`, `-p api-websocket`, `-p api-test` all pass.
- **Validation**:
  - `cargo test -p api-gql help_snapshot`
  - `cargo test -p api-rest help_snapshot`
  - `cargo test -p api-grpc help_snapshot`
  - `cargo test -p api-websocket help_snapshot`
  - `cargo test -p api-test help_snapshot`

### Task 2.3: Apply style guide to remaining clap-derive binaries

- **Location**:
  - crates/agent-out/src/cli.rs
  - crates/agent-docs/src/cli.rs
  - crates/agent-scope-lock/src/cli.rs
  - crates/web-evidence/src/cli.rs
  - crates/test-first-evidence/src/cli.rs
  - crates/image-processing/src/cli.rs
  - crates/codex-cli/src/cli.rs
  - crates/gemini-cli/src/cli.rs
  - crates/plan-issue-cli/src/cli.rs
  - crates/screen-record/src/cli.rs
  - crates/macos-agent/src/cli.rs
  - crates/git-scope/src/main.rs
  - crates/git-lock/src/main.rs
  - corresponding `tests/integration/help_snapshot.rs` files (new)
- **Description**: Mechanical pass — `long_about`, `after_help`
  (EXAMPLES, ENVIRONMENT, EXIT CODES), `#[arg(env = ...)]` annotations
  on env-backed flags, structural help-snapshot test. Surface every
  env var the binary reads (notably `CODEX_SECRET_CACHE_DIR`,
  `ZSH_CACHE_DIR`).
- **Dependencies**:
  - Task 1.1
- **Complexity**:
  - 7
- **Acceptance criteria**:
  - Every listed binary's `--help` follows the style guide.
  - Every env var read by these binaries appears in `--help`.
  - Help-snapshot tests pass.
- **Validation**:
  - `cargo test --workspace help_snapshot`

### Task 2.4: Fix `api-gql` implicit default subcommand documentation

- **Location**:
  - crates/api-gql/src/main.rs
  - crates/api-gql/src/cli.rs
  - crates/api-gql/tests/integration/cli_contract.rs
- **Description**: Default direction (per review source): document the
  implicit `call` fallback in `--help`. Add it to the binary's `about`
  / `long_about` (e.g. `"GraphQL runner (default subcommand: call)"`)
  and to `after_help` USAGE examples. Audit the root-level flags vs.
  call-specific flags: if any root-level flag is in fact specific to
  `call`, either re-scope it under the `call` subcommand or document
  it as call-only.
- **Dependencies**:
  - Task 2.2
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - `api-gql --help` clearly states `call` is the default.
  - Root-level flags advertised on the root help apply equally to all subcommands, OR are documented as call-only.
  - Existing `api-gql <operation>` invocations still work (no behavioural break).
- **Validation**:
  - `cargo test -p api-gql`
  - Manual: `api-gql --help` and `api-gql operation.graphql`

## Testing Strategy

- Unit: none specific to this plan (clap attributes are integration-only
  surfaces).
- Integration: per-binary structural help-snapshot tests. Snapshot
  matcher locks section headings and env-var names, not full wording.
- Workspace: `cargo test --workspace help_snapshot` becomes a routine
  check.
- Docs: `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
  after the style guide and per-task changes land.

## Risks & gotchas

- `--help` output is `String`-formatted by clap; tests must accept
  ANSI-stripped output or set `NO_COLOR=1`. Use the existing
  `nils-test-support` helpers if available.
- Adding `#[arg(env = "...")]` changes the precedence: env > CLI flag
  if `default_value` is not set carefully. Verify on at least one
  binary (memo-cli `--db`) before fanning out.
- Some env vars are read in deeply nested modules (e.g. `GQL_*` in
  command modules). Surfacing them in `after_help` is acceptable for
  binary-wide knobs; using `#[arg(env = ...)]` is only appropriate for
  per-flag overrides.
- Snapshot tests are deliberately structural to avoid blocking PRs on
  prose tweaks; resist the urge to lock wording.

## Rollback plan

- Sprint 1 rollback: revert `memo-cli` clap changes; the style guide
  stays as an aspirational doc until a different binary migrates first.
- Sprint 2 rollback (per task): each binary migrates in its own PR, so
  reverting one binary leaves the rest intact. The help-snapshot test
  follows the same revert.
