# Plan: CLI Output Contract Unification

## Overview

Publish a single, workspace-wide contract for machine-readable output and exit
codes, then migrate binaries from highest-traffic to lowest. Sprint 1 lands the
shared primitives in `nils-common` and writes the public contract spec. Sprint
2 migrates the two pilot binaries (`memo-cli`, `cli-template`) end to end and
proves the contract is testable. Sprint 3 fans out the migration across the
remaining binaries in three batches grouped by ownership.

## Read First

- Primary source:
  docs/plans/cli-output-contract-unification/cli-output-contract-unification-review-source.md
- Source type: review-to-improvement-doc
- Open questions carried into execution:
  - Whether `cli-template` migrates before or after `memo-cli` (default: `cli-template` first as a contract reference implementation).
  - Whether the parse-error envelope ships as a `nils-common` helper or a per-binary copy (default: shared helper to ensure shape parity).
  - Whether `staged-context` camelCase fields stay as a versioned alias or are
    removed at the same minor bump (default: versioned alias for one minor cycle).

## Scope

- In scope:
  - New `crates/nils-common/src/cli_contract.rs` (or `cli/` submodule) with the
    envelope type, `schema_version` helpers, exit-code constants, parse-error
    JSON helper, and shared `OutputFormat` enum.
  - Updated `crates/cli-template` as the reference implementation.
  - Per-binary clap-layer changes to introduce `--format text|json` and keep `--json` as a hidden alias where it already exists.
  - Per-binary exit-code call-site updates to consume the shared constants.
  - Per-binary JSON serializer updates to embed `schema_version` and use snake_case.
  - One snapshot test per JSON-emitting subcommand pinning `schema_version`.
  - One exit-code matrix test per binary.
  - New durable spec `docs/specs/cli-output-contract-v1.md`.
- Out of scope:
  - Replacing `clap` or the JSON serializer (`serde_json` stays).
  - Adding new subcommands to any binary.
  - Changing the JSON contract of `agent-workflow-primitives` records beyond adding `warnings` (their envelope is already canonical).
  - Help/env discoverability — see `cli-help-and-env-discoverability` plan.
  - Dispatch modernization (hand-rolled → clap) — see `cli-dispatch-modernization` plan.

## Assumptions

1. `nils-common` is allowed to add a new public module without violating
   the shared-helper boundary (it currently exports `env`, `fs`,
   `markdown`, etc. — `cli` fits the same shape).
2. Workspace builds and tests run under
   `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh`.
3. `pretty_assertions` is acceptable for snapshot diffs per `AGENTS.md`.
4. A hidden `--json` alias on `clap` (via `#[arg(hide = true)]`) is
   acceptable for backward compatibility.
5. `serde`'s `rename_all = "snake_case"` does not collide with any existing
   field name (verified by inspection of the four binaries with custom
   serializers).

## Sprint 1: Shared primitives + public spec

**Goal**: Land the workspace-wide contract primitives in `nils-common` plus
the durable spec, with `cli-template` as the working example. Nothing else
migrates until the primitives are stable and snapshot-tested.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-common cli_contract`
  - `cargo run -p cli-template -- --format json status`
  - `cargo run -p cli-template -- bogus-subcommand` (exit code 64)
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
- Verify: `cli-template` emits a snake_case JSON envelope with
  `schema_version: "cli.cli-template.status.v1"`; parse errors emit a JSON
  envelope when `--format json` is set; exit codes match
  `nils_common::cli_contract::exit::{USAGE, DATA, RUNTIME, SOFTWARE}`.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 1.1: Add `cli_contract` module to `nils-common`

- **Location**:
  - crates/nils-common/src/cli_contract.rs
  - crates/nils-common/src/lib.rs
  - crates/nils-common/Cargo.toml
- **Description**: Introduce `cli_contract` with: (a) `OutputFormat` enum
  (`Text`, `Json`) implementing `clap::ValueEnum`; (b) `Envelope<T>` struct
  with `schema_version: String`, `ok: bool`, `data: Option<T>`,
  `warnings: Vec<String>`, `error: Option<EnvelopeError>` plus a builder;
  (c) `EnvelopeError { code, message, hint }`; (d) `exit` submodule with
  the BSD sysexits constants we need (`SUCCESS=0`, `RUNTIME=1`, `USAGE=64`,
  `DATA=65`, `UNAVAILABLE=69`, `SOFTWARE=70`); (e) `schema_version_for`
  helper that builds the `cli.<binary>.<command>.v<N>` string.
- **Dependencies**:
  - none
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - `nils_common::cli_contract::{OutputFormat, Envelope, exit}` are publicly exported.
  - `OutputFormat` round-trips through `clap::ValueEnum`.
  - Envelope serializes with snake_case via `serde`.
  - Exit-code constants match BSD sysexits values.
  - Unit tests cover envelope success, envelope error, exit constants, and `schema_version_for` building.
- **Validation**:
  - `cargo test -p nils-common cli_contract`
  - `cargo build -p nils-common`

### Task 1.2: Add parse-error JSON helper

- **Location**:
  - crates/nils-common/src/cli_contract.rs
  - crates/nils-common/tests/integration/cli_contract.rs (new)
- **Description**: Add `emit_parse_error(binary, format, code, message)`
  that writes the JSON envelope to stdout when format is `Json` and the
  plain message to stderr when format is `Text`, and returns the right exit
  code. Intercepting `clap::Error::exit_code()` is the caller's
  responsibility; helper only formats and writes.
- **Dependencies**:
  - Task 1.1
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - JSON branch emits an envelope with `ok: false`, `error.code`, `error.message`, no `data`.
  - Text branch matches the historical `error: <msg>` prefix.
  - Helper returns `exit::USAGE` for `code = "parse-error"` and `"unknown-subcommand"`.
  - Integration test asserts byte-stable JSON output.
- **Validation**:
  - `cargo test -p nils-common cli_contract`

### Task 1.3: Migrate `cli-template` as reference implementation

- **Location**:
  - crates/cli-template/src/main.rs
  - crates/cli-template/src/lib.rs (if needed for testability)
  - crates/cli-template/tests/integration/json_contract.rs (new)
- **Description**: Wire `cli-template` to use `OutputFormat` from
  `nils-common`, emit a JSON envelope when `--format json`, intercept
  unknown-subcommand and parse errors through the new helper, and exit with
  the shared constants. Keep `--json` as a hidden alias.
- **Dependencies**:
  - Task 1.1
  - Task 1.2
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - `cli-template --format json status` prints the envelope.
  - `cli-template --json status` prints the same envelope.
  - `cli-template bogus` exits `64` and prints the JSON envelope when `--format json` was also supplied.
  - `cli-template --help` lists `--format` (not `--json`).
  - Integration test pins the literal `schema_version` string.
- **Validation**:
  - `cargo test -p cli-template`
  - Manual: `cargo run -p cli-template -- --format json status`

### Task 1.4: Write `cli-output-contract-v1.md` spec

- **Location**:
  - docs/specs/cli-output-contract-v1.md (new)
  - docs/specs/workspace-shared-crate-boundary-v1.md (cross-reference only)
- **Description**: Capture the contract as durable spec: envelope shape,
  field casing rule, `schema_version` naming, exit code table, parse-error
  behaviour, deprecation path for the `--json` bool, and the migration
  policy ("new binary MUST adopt; existing binaries SHOULD migrate within
  one minor cycle"). Cross-link to `nils-common`'s `cli_contract` module
  and `cli-template` as the worked example.
- **Dependencies**:
  - Task 1.3
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - Spec lives at `docs/specs/cli-output-contract-v1.md`.
  - All four sections (envelope, exit codes, parse errors, deprecation) are present.
  - Spec references the `cli-template` example and the `nils-common` helper.
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` passes.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`

## Sprint 2: Pilot migration (memo-cli)

**Goal**: Move `memo-cli` end to end onto the new contract and prove the
migration shape is repeatable. `memo-cli` already has the most integration
tests for `--format` and `schema_version`, so any drift surfaces fastest
here.

**Demo/Validation**:

- Commands:
  - `cargo test -p memo-cli`
  - `memo-cli --format json list`
  - `memo-cli --json list` (alias still works, no warning when hidden)
  - `memo-cli bogus --format json` (JSON parse-error envelope)
  - `memo-cli list --limit -1` (exit 64 with usage error envelope)
- Verify: `memo-cli` returns `64` for all usage errors, `65` for data
  errors, `1` for runtime errors; every JSON output contains a
  snake_case envelope with `schema_version: "cli.memo-cli.<cmd>.v1"`.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 2.1: Replace memo-cli exit-code constants with shared module

- **Location**:
  - crates/memo-cli/src/errors.rs
  - crates/memo-cli/src/main.rs
  - crates/memo-cli/src/app.rs
- **Description**: Delete the local `AppError::exit_code` magic numbers
  and call `nils_common::cli_contract::exit::{USAGE, DATA, RUNTIME}`
  instead. Leave the existing `AppError` enum shape but route its codes
  through the shared constants.
- **Dependencies**:
  - Task 1.1
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - `AppError::exit_code` reads from the shared constants only (no inline numeric literals).
  - Existing `cargo test -p memo-cli` passes without functional change.
- **Validation**:
  - `cargo test -p memo-cli`

### Task 2.2: Migrate memo-cli to `OutputFormat` and hidden `--json` alias

- **Location**:
  - crates/memo-cli/src/cli.rs
- **Description**: Replace the local `OutputFormat`/`OutputMode` pair with
  the shared `nils_common::cli_contract::OutputFormat`. Keep `--json`
  available as a hidden boolean alias via `#[arg(long, hide = true,
  global = true)]`. Move `--json` ↔ `--format` conflict to a clap
  `conflicts_with` declaration so parse-time errors fire instead of the
  runtime check in `resolve_output_mode`.
- **Dependencies**:
  - Task 2.1
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - `memo-cli --help` lists `--format` and not `--json` (alias hidden).
  - `memo-cli --json --format text list` fails at parse time with exit code `64`.
  - All existing `cli` tests still pass; the runtime `resolve_output_mode` test is replaced by a clap parse-time test.
- **Validation**:
  - `cargo test -p memo-cli`
  - Manual: `memo-cli --json --format text list`

### Task 2.3: Migrate memo-cli JSON output to shared envelope and `warnings`

- **Location**:
  - crates/memo-cli/src/output/json.rs
  - crates/memo-cli/src/output/mod.rs
  - crates/memo-cli/src/commands/
- **Description**: Wrap each subcommand's JSON output in the shared
  `Envelope<T>`. Move every `eprintln!` that conveys a per-record warning
  (e.g. `output/text.rs:135-140`) into the envelope's `warnings` array
  when `OutputFormat::Json`. Keep text-mode behaviour unchanged. Pin the
  literal `schema_version` per subcommand in integration tests.
- **Dependencies**:
  - Task 2.2
- **Complexity**:
  - 6
- **Acceptance criteria**:
  - Every `memo-cli --format json <cmd>` output is a single envelope object (no streaming changes).
  - The envelope's `schema_version` matches `cli.memo-cli.<cmd>.v1` for every subcommand.
  - Apply warnings appear in `warnings` when JSON, on stderr when text.
  - Snapshot tests under `tests/integration/json_contract.rs` cover every subcommand's `schema_version`.
- **Validation**:
  - `cargo test -p memo-cli json_contract`
  - `cargo test -p memo-cli`

### Task 2.4: Route memo-cli parse and unknown-subcommand errors through helper

- **Location**:
  - crates/memo-cli/src/app.rs
  - crates/memo-cli/src/main.rs
- **Description**: When clap parsing fails, detect whether `--format
  json` or `--json` appeared in argv and call
  `nils_common::cli_contract::emit_parse_error` accordingly before
  exiting. Otherwise let clap render its native error.
- **Dependencies**:
  - Task 1.2
  - Task 2.2
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - `memo-cli bogus --format json` prints the JSON envelope to stdout and exits `64`.
  - `memo-cli bogus` (no `--format`) prints clap's text error to stderr and exits `64`.
  - `memo-cli --json bogus` matches the JSON path.
  - New integration test covers both paths.
- **Validation**:
  - `cargo test -p memo-cli`

### Task 2.5: Add memo-cli exit-code matrix test

- **Location**:
  - crates/memo-cli/tests/integration/exit_codes.rs (new)
- **Description**: One test per exit-code path: success (0), usage error
  (64, missing arg), data error (65, malformed payload), runtime error
  (1, simulated I/O failure on a temp DB). Use the existing test
  harness; do not introduce new fixtures unless required.
- **Dependencies**:
  - Task 2.1
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - Four tests cover four exit codes.
  - Each test asserts the literal exit-code constant from `nils_common::cli_contract::exit`.
- **Validation**:
  - `cargo test -p memo-cli exit_codes`

## Sprint 3: Fan-out migration

**Goal**: Migrate every remaining JSON-emitting binary in three parallel
batches. Each batch is its own PR so blast radius stays bounded and
binaries with shared crates can be reviewed together.

**Demo/Validation**:

- Commands:
  - `cargo test --workspace`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh`
  - Per binary: `<binary> --format json <cmd>` (envelope present),
    `<binary> bogus --format json` (parse-error envelope), `<binary> bogus`
    (exit 64).
- Verify: every JSON-emitting subcommand has a `schema_version` snapshot
  test; every binary has an exit-code matrix test; no binary still ships
  ad-hoc `1`/`2` exit codes for usage errors.

**PR grouping intent**: `group`
**Execution Profile**: `parallel-x3`

### Task 3.1: Migrate `agent-workflow-primitives` binaries

- **Location**:
  - crates/agent-workflow-primitives/src/
  - crates/agent-workflow-primitives/tests/integration/
- **Description**: Cover all eight binaries (`browser-session`,
  `canary-check`, `docs-impact`, `heuristic-inbox`, `model-cross-check`,
  `repo-retro`, `review-evidence`, `skill-usage`). Replace the existing
  `schema_version` constants with the shared envelope wrapper, swap exit
  codes onto the shared constants, and add the parse-error helper.
  Existing `RECORD_SCHEMA_VERSION` literals stay byte-stable; only the
  surrounding envelope changes.
- **Dependencies**:
  - Task 1.4
- **Complexity**:
  - 6
- **Acceptance criteria**:
  - All eight binaries route `--format json` through the shared envelope.
  - All eight binaries return `64` for usage errors and `65` for data errors.
  - Existing JSON-byte-stable contract tests still pass (no field name or `schema_version` literal changes).
  - One new exit-code matrix test per binary.
- **Validation**:
  - `cargo test -p agent-workflow-primitives`

### Task 3.2: Migrate API testing stack (api-rest / api-gql / api-grpc / api-websocket / api-test)

- **Location**:
  - crates/api-testing-core/src/
  - crates/api-rest/src/
  - crates/api-gql/src/
  - crates/api-grpc/src/
  - crates/api-websocket/src/
  - crates/api-test/src/
- **Description**: Hoist the shared `OutputFormat`/exit-code handling into
  `api-testing-core` so the five binaries inherit it once. Add
  `schema_version` where missing (notably `api-gql/src/commands/report.rs`,
  which currently emits no version field). Migrate the suite runner's
  JSON output last so reporting tests stay byte-stable.
- **Dependencies**:
  - Task 1.4
- **Complexity**:
  - 8
- **Acceptance criteria**:
  - Each of the five binaries emits a `schema_version` field on every JSON output.
  - `api-test` suite reports keep their existing JUnit shape but JSON runs through the shared envelope.
  - Exit codes match the workspace contract.
  - Snapshot tests cover one subcommand per binary.
- **Validation**:
  - `cargo test -p api-rest`
  - `cargo test -p api-gql`
  - `cargo test -p api-grpc`
  - `cargo test -p api-websocket`
  - `cargo test -p api-test`

### Task 3.3: Migrate remaining single-binary crates and run the full gate

- **Location**:
  - crates/semantic-commit/src/
  - crates/git-scope/src/
  - crates/git-summary/src/
  - crates/git-lock/src/
  - crates/agent-out/src/
  - crates/agent-docs/src/
  - crates/agent-scope-lock/src/
  - crates/web-evidence/src/
  - crates/test-first-evidence/src/
  - crates/image-processing/src/
  - crates/codex-cli/src/
  - crates/gemini-cli/src/
  - crates/plan-tooling/src/
  - crates/plan-issue-cli/src/
- **Description**: Migrate each remaining binary onto the shared
  contract. Pay special attention to `semantic-commit/src/staged_context.rs`
  which uses camelCase (`schemaVersion`, `oldPath`) — emit a versioned
  alias (`schema_version: "cli.semantic-commit.staged-context.v2"`) and
  keep the camelCase fields as deprecated aliases for one minor cycle.
  `git-cli` participates here for exit-code consolidation but its
  dispatch refactor stays in the `cli-dispatch-modernization` plan.
- **Dependencies**:
  - Task 1.4
- **Complexity**:
  - 8
- **Acceptance criteria**:
  - Every listed binary returns sysexit-aligned codes.
  - Every JSON-emitting subcommand has `schema_version`.
  - `staged-context` ships `v2` schema and a documented alias path.
  - One exit-code matrix test per binary.
- **Validation**:
  - `cargo test --workspace`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh`

### Task 3.4: Workspace lint script for legacy contract usage

- **Location**:
  - scripts/ci/cli-output-contract-lint.sh (new)
  - scripts/ci/nils-cli-checks-entrypoint.sh
  - DEVELOPMENT.md
- **Description**: Add a small grep-based lint script that fails on:
  (a) any new `--json` bool flag without `hide = true`; (b) any
  `std::process::exit(1|2)` for usage errors in `main.rs` files; (c)
  any JSON serializer that emits camelCase outside the documented
  aliases. Wire it into the docs-only required-check path so drift gets
  caught on PRs that only touch CLIs.
- **Dependencies**:
  - Task 3.1
  - Task 3.2
  - Task 3.3
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - Lint script returns 0 on the post-migration workspace.
  - Lint script fails on a synthetic regression fixture (a new binary that re-introduces `--json` without `hide = true`).
  - Script appears in docs-only checks and `DEVELOPMENT.md`.
- **Validation**:
  - `bash scripts/ci/cli-output-contract-lint.sh`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`

## Testing Strategy

- Unit: cover the `nils-common::cli_contract` envelope, helpers, and exit
  constants.
- Integration (per binary): one snapshot test per JSON-emitting subcommand
  pinning `schema_version`; one exit-code matrix test per binary.
- Cross-binary: a workspace integration test that builds every binary and
  asserts the shared exit-code constants resolve to the same value at the
  ELF boundary (sanity check that no binary linked an older copy of the
  constants).
- Workspace gate: `NILS_CLI_TEST_RUNNER=nextest bash
  scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage` must remain
  green after each sprint.
- Docs-only gate: `bash scripts/ci/nils-cli-checks-entrypoint.sh
  --docs-only` after the spec and lint script land.

## Risks & gotchas

- `agent-workflow-primitives` records ship in user state directories; the
  envelope wrapper must not change the byte-stable `RECORD_SCHEMA_VERSION`
  string or persisted records will fail validation. Treat their tests as
  the contract.
- `staged-context` is consumed by other agents in the wider toolchain —
  the camelCase alias must remain accepted for one minor cycle; bumping
  `schema_version` is the breaking signal.
- Clap parse errors are emitted before our helper runs; the parse-error
  helper has to detect `--format json` / `--json` from raw argv (not
  parsed flags) to render the JSON envelope. Cover this with a test that
  feeds a deliberately malformed flag.
- `nils-common` adds a new public module; downstream crates may have
  already named a local `cli_contract` symbol. Confirm before the rename
  lands.
- `cargo test --workspace` reuses one process for all binaries; tests
  that mutate the environment must use `EnvGuard` from
  `nils-test-support` to avoid cross-test bleed.

## Rollback plan

- Sprint 1 rollback: revert the new `cli_contract` module; `cli-template`
  reverts to its prior shape. No production binary depends on it yet.
- Sprint 2 rollback: revert `memo-cli` migration commits one task at a
  time; the shared module stays in place and is exercised only by
  `cli-template`.
- Sprint 3 rollback (per task): each binary lands as its own PR, so
  reverting a single binary's migration leaves the rest intact. The lint
  script can be marked non-required until the workspace re-converges.
