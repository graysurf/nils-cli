# CLI Output Contract Unification Execution State

## Current State

- Status: in-progress (Sprint 1 + Sprint 2 + Tasks 3.1 + 3.2 + 3.3 complete; Task 3.4 pending)
- Target scope: Task 3.3 (remaining 14 single-binary crates)
- Execution window: Task 3.3 only — Task 3.4 (workspace lint script) lands in a follow-up PR
- Staged execution confirmation: confirmed(Sprint 1 PR #375, Sprint 2 PR #376,
  Task 3.1 PR #377, Task 3.2 PR #378 merged; Task 3.3 in this PR; Task 3.4 to come)
- Current task: Task 3.3 complete
- Next task: Task 3.4 (workspace lint script for legacy contract usage)
- Last updated: 2026-05-20
- Branch/commit: feat/cli-output-contract-fan-out-3-3 (pending push)
- Source document:
  docs/plans/cli-output-contract-unification/cli-output-contract-unification-plan.md
- Direct source-doc execution waiver: not applicable

## Task Ledger

| ID       | Status  | Task                                                              | Evidence                                                                                                                                                                                                                                              | Notes                                                                                                                                                                                                                                                                                                                                                                                        |
| -------- | ------- | ----------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Task 1.1 | done    | Add `cli_contract` module to `nils-common`                        | crates/nils-common/src/cli_contract.rs; PR #375                                                                                                                                                                                                       | Plus additive `EnvelopeError::details` field in Sprint 2                                                                                                                                                                                                                                                                                                                                     |
| Task 1.2 | done    | Add parse-error JSON helper                                       | crates/nils-common/tests/integration/cli_contract.rs; PR #375                                                                                                                                                                                         |                                                                                                                                                                                                                                                                                                                                                                                              |
| Task 1.3 | done    | Migrate `cli-template` as reference implementation                | crates/cli-template/src/main.rs; PR #375                                                                                                                                                                                                              |                                                                                                                                                                                                                                                                                                                                                                                              |
| Task 1.4 | done    | Write `cli-output-contract-v1.md` spec                            | docs/specs/cli-output-contract-v1.md; PR #375                                                                                                                                                                                                         | Sprint 2 extended with `details` field                                                                                                                                                                                                                                                                                                                                                       |
| Task 2.1 | done    | Replace memo-cli exit-code constants with shared module           | crates/memo-cli/src/errors.rs                                                                                                                                                                                                                         | exit::USAGE/DATA/RUNTIME replaces 64/65/1 literals                                                                                                                                                                                                                                                                                                                                           |
| Task 2.2 | done    | Migrate memo-cli to `OutputFormat` and hidden `--json` alias      | crates/memo-cli/src/cli.rs                                                                                                                                                                                                                            | shared `nils_common::cli_contract::OutputFormat`; `--json` `hide=true conflicts_with=format`                                                                                                                                                                                                                                                                                                 |
| Task 2.3 | done    | Migrate memo-cli JSON output to shared envelope and `warnings`    | crates/memo-cli/src/output/json.rs + commands/*                                                                                                                                                                                                       | schema_version moved to `cli.memo-cli.<cmd>.v1`; collections nest `items` + `pagination`/`meta` under `data`; apply per-item errors surface in `warnings`                                                                                                                                                                                                                                    |
| Task 2.4 | done    | Route memo-cli parse and unknown-subcommand errors through helper | crates/memo-cli/src/app.rs                                                                                                                                                                                                                            | argv-scan detect of `--format json` / `--json`; calls `emit_parse_error`                                                                                                                                                                                                                                                                                                                     |
| Task 2.5 | done    | Add memo-cli exit-code matrix test                                | crates/memo-cli/tests/integration/exit_codes.rs                                                                                                                                                                                                       | 5 tests covering SUCCESS / USAGE (arg + unknown subcmd) / DATA / RUNTIME                                                                                                                                                                                                                                                                                                                     |
| Task 3.1 | done    | Migrate `agent-workflow-primitives` binaries                      | crates/agent-workflow-primitives/src/common.rs + per-binary entrypoints; tests/integration/exit_codes.rs                                                                                                                                              | shared `Envelope<T>` via refactored `common.rs`; `handle_parse_error` routes parse errors; 50 cli tests + 3 matrix tests; `RECORD_SCHEMA_VERSION` literals stay byte-stable                                                                                                                                                                                                                  |
| Task 3.2 | done    | Migrate API testing stack                                         | crates/api-testing-core/src/cli_contract.rs + per-binary main.rs; cli_smoke matrix tests                                                                                                                                                              | shared `handle_parse_error` helper in api-testing-core; api-websocket envelope drops `command` / renames `result` → `data`; api-test stdout + `--out` both ship `cli.api-test.run.v1` envelope; `render_summary_from_json_str` accepts both wrapped + raw shapes                                                                                                                             |
| Task 3.3 | done    | Migrate remaining single-binary crates and run the full gate      | 14 crates: semantic-commit / git-scope / git-summary / git-lock / agent-out / agent-docs / agent-scope-lock / web-evidence / test-first-evidence / image-processing / codex-cli / gemini-cli / plan-tooling / plan-issue-cli; matrix tests per binary | semantic-commit staged-context emits `schema_version: "cli.semantic-commit.staged-context.v2"` plus all camelCase fields (`schemaVersion`, `generatedAt`, `fileCount`, `oldPath`, …) as deprecated aliases; agent-docs / image-processing / plan-issue-cli usage exits realigned 2 → 64; remaining crates wire `nils_common::cli_contract::exit::*` constants; 14 new exit-code matrix tests |
| Task 3.4 | pending | Workspace lint script for legacy contract usage                   | n/a                                                                                                                                                                                                                                                   | depends on 3.1, 3.2, 3.3 (all done)                                                                                                                                                                                                                                                                                                                                                          |

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `cargo test -p nils-common cli_contract` | pass | 10 tests (`EnvelopeError::details` additive change covered) | terminal log |
| `cargo test -p nils-memo-cli` | pass | 34 tests (29 integration incl. 5 new exit-code matrix + 32 unit) | terminal log |
| `cargo nextest run --workspace` | pass | 2801/2801 pass after Task 3.3 fan-out | terminal log |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass | zero warnings after Task 3.3 | terminal log |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` | pass | docs hygiene + plan-bundle validate green | terminal log |
| `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh` | pass | cargo fmt + clippy -D warnings + nextest workspace + doctests + third-party audit green | terminal log |

## Blockers

- none

## Session Log

### 2026-05-19 (Sprint 1 → PR #375 merged at ac87ca2)

- See prior turn's notes. Sprint 1 foundation landed; this entry kept as
  context for Sprint 2.

### 2026-05-20 (Task 3.3 → feat/cli-output-contract-fan-out-3-3)

- Read:
  - docs/plans/cli-output-contract-unification/cli-output-contract-unification-plan.md
  - crates/cli-template/src/main.rs (reference impl)
  - crates/semantic-commit/{src/lib.rs, src/usage.rs, src/staged_context.rs,
    src/commit.rs, tests/integration/staged_context.rs}
  - crates/git-scope/src/main.rs; crates/git-summary/{src/main.rs, src/app.rs};
    crates/git-lock/src/main.rs
  - crates/agent-out/{Cargo.toml, src/lib.rs}; crates/agent-docs/{Cargo.toml,
    src/lib.rs}; crates/agent-scope-lock/{Cargo.toml, src/lib.rs}
  - crates/web-evidence/{Cargo.toml, src/lib.rs};
    crates/test-first-evidence/{Cargo.toml, src/lib.rs};
    crates/image-processing/{Cargo.toml, src/main.rs}
  - crates/codex-cli/src/main.rs; crates/gemini-cli/src/main.rs
  - crates/plan-tooling/{src/lib.rs, src/usage.rs};
    crates/plan-issue-cli/src/lib.rs
- Changed:
  - crates/semantic-commit/src/staged_context.rs (new `schema_version:
    "cli.semantic-commit.staged-context.v2"`; duplicates camelCase fields —
    `schemaVersion`, `generatedAt`, `fileCount`, `insertions`, `deletions`,
    `binaryFileCount`, `lockfileCount`, `rootFileCount`, `topLevelDirCount`,
    `statusCounts`, `topLevelDirs`, `oldPath` — as deprecated aliases for one
    minor cycle; switches inline EXIT_ERROR=1 onto shared `exit::RUNTIME`)
  - crates/semantic-commit/tests/integration/exit_codes.rs (new — 6 tests:
    unknown subcommand → 1 / staged-context outside repo → 1 / no staged → 2
    / commit missing message → 3 / commit invalid message → 4 / staged-context
    v2 schema + camelCase alias assertions)
  - crates/git-scope/src/main.rs (process::exit(1|2) → exit::RUNTIME / USAGE
    via `nils_common::cli_contract::exit`); new exit-code matrix test
  - crates/git-summary/src/app.rs (invalid usage → exit::USAGE; runtime →
    exit::RUNTIME); new exit-code matrix test
  - crates/git-lock/src/main.rs (unknown command → exit::USAGE; runtime →
    exit::RUNTIME); new exit-code matrix test
  - crates/agent-out/{Cargo.toml, src/lib.rs} (add nils-common dep + nils-test-support dev-dep;
    inline EXIT_USAGE/RUNTIME/AUDIT_VIOLATIONS now point at shared `exit::*`); new exit-code
    matrix test
  - crates/agent-docs/src/lib.rs (EXIT_USAGE realigned 2 → 64 via shared
    constant; clap parse-error path now routes non-help/version errors through
    EXIT_USAGE explicitly); new exit-code matrix test
  - crates/agent-scope-lock/{Cargo.toml, src/lib.rs} (add nils-common dep;
    inline constants reuse `exit::*`); new exit-code matrix test
  - crates/web-evidence/{Cargo.toml, src/lib.rs} (add nils-common dep; reuse
    `exit::*`); new exit-code matrix test
  - crates/test-first-evidence/{Cargo.toml, src/lib.rs} (add nils-common dep;
    reuse `exit::*`); new exit-code matrix test
  - crates/image-processing/src/main.rs (usage_error realigned 2 → 64 via
    `exit::USAGE`; clap parse-error path narrows USAGE mapping for non-help/
    version errors); existing edge_cases tests updated 2 → 64; new exit-code
    matrix test
  - crates/codex-cli/src/main.rs; crates/gemini-cli/src/main.rs (inline 64
    literals replaced with `exit::USAGE`; help-path uses `exit::SUCCESS`); new
    exit-code matrix tests per binary
  - crates/plan-tooling/src/usage.rs (unknown command → `exit::USAGE`; unit
    test renamed `dispatch_unknown_command_exits_usage`, asserts
    `exit::USAGE`); new exit-code matrix test
  - crates/plan-issue-cli/src/lib.rs (EXIT_USAGE realigned 2 → 64 via shared
    constant; EXIT_FAILURE / EXIT_SUCCESS routed through `exit::*`); existing
    parity_guardrails tests updated 2 → 64; new exit-code matrix test
  - THIRD_PARTY_LICENSES.md / THIRD_PARTY_NOTICES.md (regenerated for new
    nils-common dep edges in agent-out / agent-scope-lock / web-evidence /
    test-first-evidence)
- Validated:
  - `cargo build --workspace` (clean)
  - `cargo nextest run --workspace` (2801 / 2801 pass)
  - `cargo clippy --workspace --all-targets -- -D warnings` (zero warnings)
  - Per-binary `cargo test -p <crate> --test integration exit_codes` for all
    14 crates: green
- Blocked by: none
- Next: open feature PR with `/deliver-feature-pr`; Task 3.4 (workspace lint
  script for legacy contract usage) lands as the final follow-up PR.

### 2026-05-19 (Task 3.2 → feat/cli-output-contract-api-testing-stack)

- Read:
  - docs/plans/cli-output-contract-unification/cli-output-contract-unification-plan.md
  - crates/api-testing-core/{Cargo.toml, src/lib.rs, src/suite/summary.rs}
  - crates/api-rest/src/main.rs; crates/api-gql/src/main.rs;
    crates/api-grpc/src/main.rs; crates/api-websocket/{src/main.rs, src/cli.rs,
    src/commands/call.rs, src/commands/history.rs}; crates/api-test/src/main.rs
- Changed:
  - crates/api-testing-core/Cargo.toml (added `clap` dep)
  - crates/api-testing-core/src/cli_contract.rs (new — shared `handle_parse_error`
    helper + envelope re-exports)
  - crates/api-testing-core/src/lib.rs (register new `cli_contract` module)
  - crates/api-testing-core/src/suite/summary.rs (`render_summary_from_json_str`
    now accepts both wrapped and raw `SuiteRunResults` JSON for backwards
    compatibility)
  - crates/api-{rest,gql,grpc,websocket}/src/main.rs (route parse errors
    through `api_testing_core::cli_contract::handle_parse_error`; drop
    `clap::error::ErrorKind` import; `argv_with_default_command` output
    converted to `OsString` for the helper)
  - crates/api-websocket/src/commands/{call,history}.rs (drop `command` field
    from success/failure envelopes; rename `result` → `data` to match the
    shared contract)
  - crates/api-test/src/main.rs (wrap stdout + `--out` JSON in
    `Envelope::success("cli.api-test.run.v1", &results)`; route parse errors
    through shared helper)
  - crates/api-{rest,gql,grpc,websocket,test}/tests/integration/cli_smoke.rs
    (+2 matrix tests per binary: unknown flag → exit 64 in text mode; same
    flag with `--format json` → JSON parse-error envelope on stdout)
  - crates/api-websocket/tests/integration/{cli_smoke.rs, integration.rs,
    json_contract.rs} (bulk transform `["result"]` → `["data"]`; drop
    `command` assertions)
  - crates/api-test/tests/integration/{e2e.rs, grpc_integration.rs,
    progress_contract.rs} (results JSON now lives under `data.*` path)
  - THIRD_PARTY_LICENSES.md / THIRD_PARTY_NOTICES.md (regenerated for the new
    `clap` dependency edge in api-testing-core)
- Validated:
  - `cargo test -p nils-api-rest` (51 tests pass)
  - `cargo test -p nils-api-gql` (38 tests pass)
  - `cargo test -p nils-api-grpc` (10 tests pass)
  - `cargo test -p nils-api-websocket` (38 tests pass)
  - `cargo test -p nils-api-test` (12 tests pass)
  - `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh`
    (pass)
- Blocked by: none
- Next: open feature PR with `/deliver-feature-pr`; Task 3.3 (remaining
  single-binary crates) picks up in a follow-up PR.

### 2026-05-19 (Task 3.1 → feat/cli-output-contract-awp-migration)

- Read:
  - docs/plans/cli-output-contract-unification/cli-output-contract-unification-plan.md
  - crates/agent-workflow-primitives/{Cargo.toml, src/common.rs, src/lib.rs,
    src/canary_check.rs, src/repo_retro.rs}
  - crates/agent-workflow-primitives/tests/integration/{cli.rs, integration.rs}
- Changed:
  - crates/agent-workflow-primitives/Cargo.toml (new `nils-common` dep)
  - crates/agent-workflow-primitives/src/common.rs (re-export shared
    `OutputFormat`; rewrite `render_success` / `render_error` around shared
    `Envelope<T>` / `EnvelopeError`; add `CliError::data` + `EXIT_DATA`; add
    `handle_parse_error` helper with raw-argv format detection)
  - crates/agent-workflow-primitives/src/{browser_session,canary_check,
    docs_impact,heuristic_inbox,model_cross_check,repo_retro,review_evidence,
    skill_usage}.rs (parse-error block → `handle_parse_error(<bin>, argv, err)`;
    drop `use clap::error::ErrorKind` / `EXIT_USAGE` from imports)
  - crates/agent-workflow-primitives/src/repo_retro.rs (drop local
    `SuccessEnvelope`; emit via shared `Envelope::success`)
  - crates/agent-workflow-primitives/tests/integration/{cli.rs, integration.rs}
    (bulk transform `["result"]` → `["data"]`; drop `["command"]` assertions;
    register new `exit_codes` test module)
  - crates/agent-workflow-primitives/tests/integration/exit_codes.rs (new — 3
    matrix tests over all 8 binaries)
  - THIRD_PARTY_LICENSES.md / THIRD_PARTY_NOTICES.md (regenerated for the
    new nils-common dependency edge in awp)
- Validated:
  - `cargo test -p nils-agent-workflow-primitives` (53 tests pass — 50 cli +
    3 new exit-code matrix)
  - `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh`
    (pass)
- Blocked by: none
- Next: open feature PR with `/deliver-feature-pr`; Task 3.2 (api-* migration)
  picks up in a follow-up PR per the plan's `parallel-x3` strategy.

### 2026-05-19 (Sprint 2 → feat/cli-output-contract-memo-cli-pilot)

- Read:
  - docs/plans/cli-output-contract-unification/cli-output-contract-unification-plan.md
  - crates/memo-cli/{src/cli.rs, src/errors.rs, src/app.rs, src/output/*.rs, src/commands/*.rs}
  - crates/memo-cli/tests/integration/{json_contract.rs, support/mod.rs, integration.rs}
- Changed:
  - crates/nils-common/src/cli_contract.rs (additive: `EnvelopeError::details: Option<Value>` + `with_details`)
  - docs/specs/cli-output-contract-v1.md (spec text mentions `error.details`)
  - crates/memo-cli/src/errors.rs (shared exit constants; drop `JsonError` struct; expose `details()` accessor)
  - crates/memo-cli/src/cli.rs (`OutputFormat` re-export from nils-common;
    hidden `--json` alias; clap `conflicts_with`; schema_version prefixed
    `cli.memo-cli.*.v1`; runtime conflict test → clap parse-time test)
  - crates/memo-cli/src/app.rs (route parse / unknown-subcommand errors through `emit_parse_error`; preserves clap help/version exits)
  - crates/memo-cli/src/output/{json.rs, mod.rs} (new `emit_data` / `emit_data_with_warnings` / `emit_json_error` wrapping shared `Envelope<T>`)
  - crates/memo-cli/src/commands/{add,update,delete,list,search,fetch,report,apply}.rs
    (schema_version prefix; nest `items`/`pagination`/`meta` under `data`;
    apply collects per-item warnings into envelope)
  - crates/memo-cli/tests/integration/*.rs (bulk transform: schema_version
    prefix; `result`→`data`; `results`→`data.items`;
    `pagination`→`data.pagination`; `meta`→`data.meta`; drop `command` field;
    replace runtime conflict test with clap parse-time)
  - crates/memo-cli/tests/integration/exit_codes.rs (new — 5 tests)
  - docs/plans/cli-output-contract-unification/cli-output-contract-unification-execution-state.md (Sprint 2 ledger)
- Validated:
  - `cargo test -p nils-common cli_contract` (10/10 pass — Sprint 1 module survives additive change)
  - `cargo test -p nils-memo-cli` (66 tests: 32 unit + 29 integration + 5 new exit-code matrix; 0 fail)
  - `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh` (pass)
- Blocked by: none
- Next: open feature PR with `/deliver-feature-pr`; Sprint 3 picks up in follow-up PR(s) per the plan's `parallel-x3` strategy.
