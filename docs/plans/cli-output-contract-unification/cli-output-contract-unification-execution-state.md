# CLI Output Contract Unification Execution State

## Current State

- Status: in-progress (Sprint 1 + Sprint 2 + Task 3.1 complete; Tasks 3.2 / 3.3 / 3.4 pending)
- Target scope: Task 3.1 (agent-workflow-primitives, 8 binaries)
- Execution window: Task 3.1 only — Tasks 3.2 / 3.3 stay in follow-up PRs per
  the plan's `parallel-x3` PR strategy; Task 3.4 (lint script) lands after 3.3
- Staged execution confirmation: confirmed(Sprint 1 PR #375 + Sprint 2 PR #376
  merged; Task 3.1 in this PR; Tasks 3.2 / 3.3 / 3.4 follow-up PRs to come)
- Current task: Task 3.1 complete
- Next task: Task 3.2 (migrate API testing stack)
- Last updated: 2026-05-19
- Branch/commit: feat/cli-output-contract-awp-migration (pending push)
- Source document:
  docs/plans/cli-output-contract-unification/cli-output-contract-unification-plan.md
- Direct source-doc execution waiver: not applicable

## Task Ledger

| ID       | Status  | Task                                                              | Evidence                                                                                                 | Notes                                                                                                                                                                       |
| -------- | ------- | ----------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Task 1.1 | done    | Add `cli_contract` module to `nils-common`                        | crates/nils-common/src/cli_contract.rs; PR #375                                                          | Plus additive `EnvelopeError::details` field in Sprint 2                                                                                                                    |
| Task 1.2 | done    | Add parse-error JSON helper                                       | crates/nils-common/tests/integration/cli_contract.rs; PR #375                                            |                                                                                                                                                                             |
| Task 1.3 | done    | Migrate `cli-template` as reference implementation                | crates/cli-template/src/main.rs; PR #375                                                                 |                                                                                                                                                                             |
| Task 1.4 | done    | Write `cli-output-contract-v1.md` spec                            | docs/specs/cli-output-contract-v1.md; PR #375                                                            | Sprint 2 extended with `details` field                                                                                                                                      |
| Task 2.1 | done    | Replace memo-cli exit-code constants with shared module           | crates/memo-cli/src/errors.rs                                                                            | exit::USAGE/DATA/RUNTIME replaces 64/65/1 literals                                                                                                                          |
| Task 2.2 | done    | Migrate memo-cli to `OutputFormat` and hidden `--json` alias      | crates/memo-cli/src/cli.rs                                                                               | shared `nils_common::cli_contract::OutputFormat`; `--json` `hide=true conflicts_with=format`                                                                                |
| Task 2.3 | done    | Migrate memo-cli JSON output to shared envelope and `warnings`    | crates/memo-cli/src/output/json.rs + commands/*                                                          | schema_version moved to `cli.memo-cli.<cmd>.v1`; collections nest `items` + `pagination`/`meta` under `data`; apply per-item errors surface in `warnings`                   |
| Task 2.4 | done    | Route memo-cli parse and unknown-subcommand errors through helper | crates/memo-cli/src/app.rs                                                                               | argv-scan detect of `--format json` / `--json`; calls `emit_parse_error`                                                                                                    |
| Task 2.5 | done    | Add memo-cli exit-code matrix test                                | crates/memo-cli/tests/integration/exit_codes.rs                                                          | 5 tests covering SUCCESS / USAGE (arg + unknown subcmd) / DATA / RUNTIME                                                                                                    |
| Task 3.1 | done    | Migrate `agent-workflow-primitives` binaries                      | crates/agent-workflow-primitives/src/common.rs + per-binary entrypoints; tests/integration/exit_codes.rs | shared `Envelope<T>` via refactored `common.rs`; `handle_parse_error` routes parse errors; 50 cli tests + 3 matrix tests; `RECORD_SCHEMA_VERSION` literals stay byte-stable |
| Task 3.2 | pending | Migrate API testing stack                                         | n/a                                                                                                      | next sprint                                                                                                                                                                 |
| Task 3.3 | pending | Migrate remaining single-binary crates and run the full gate      | n/a                                                                                                      | next sprint                                                                                                                                                                 |
| Task 3.4 | pending | Workspace lint script for legacy contract usage                   | n/a                                                                                                      | depends on 3.1, 3.2, 3.3                                                                                                                                                    |

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `cargo test -p nils-common cli_contract` | pass | 10 tests (`EnvelopeError::details` additive change covered) | terminal log |
| `cargo test -p nils-memo-cli` | pass | 34 tests (29 integration incl. 5 new exit-code matrix + 32 unit) | terminal log |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` | pass | docs hygiene + plan-bundle validate green | terminal log |
| `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh` | pass | cargo fmt + clippy -D warnings + nextest workspace + doctests + third-party audit green | terminal log |

## Blockers

- none

## Session Log

### 2026-05-19 (Sprint 1 → PR #375 merged at ac87ca2)

- See prior turn's notes. Sprint 1 foundation landed; this entry kept as
  context for Sprint 2.

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
