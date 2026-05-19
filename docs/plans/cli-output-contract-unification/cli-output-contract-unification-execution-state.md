# CLI Output Contract Unification Execution State

## Current State

- Status: in-progress (Sprint 1 complete, Sprint 2 and 3 pending)
- Target scope: Sprint 1 (Tasks 1.1–1.4)
- Execution window: Sprint 1 only — confirmed by user; Sprint 2 and Sprint 3 stay open in separate PRs to honor the plan's `parallel-x3` PR strategy
- Staged execution confirmation: confirmed(Sprint 1 only PR; Sprint 2/3 follow separately)
- Current task: Sprint 1 complete
- Next task: Task 2.1 (replace memo-cli exit-code constants with shared module)
- Last updated: 2026-05-19
- Branch/commit: feat/cli-output-contract-v1-foundation (pending)
- Source document:
  docs/plans/cli-output-contract-unification/cli-output-contract-unification-plan.md
- Direct source-doc execution waiver: not applicable

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| Task 1.1 | done | Add `cli_contract` module to `nils-common` | crates/nils-common/src/cli_contract.rs; `cargo test -p nils-common cli_contract` (6 unit tests) | OutputFormat, Envelope, EnvelopeError, exit constants, schema_version_for |
| Task 1.2 | done | Add parse-error JSON helper | crates/nils-common/tests/integration/cli_contract.rs; `cargo test -p nils-common cli_contract` (4 integration tests) | emit_parse_error + emit_parse_error_to writers |
| Task 1.3 | done | Migrate `cli-template` as reference implementation | crates/cli-template/src/main.rs; `cargo test -p nils-cli-template` (13 tests, +9 new) | Hidden `--json` alias; new `status` subcommand emits envelope |
| Task 1.4 | done | Write `cli-output-contract-v1.md` spec | docs/specs/cli-output-contract-v1.md; `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` | Reconciles older `cli-service-json-contract-guideline-v1.md` shape |
| Task 2.1 | pending | Replace memo-cli exit-code constants with shared module | n/a | depends on 1.1 (now satisfied) |
| Task 2.2 | pending | Migrate memo-cli to `OutputFormat` and hidden `--json` alias | n/a | depends on 2.1 |
| Task 2.3 | pending | Migrate memo-cli JSON output to shared envelope and `warnings` | n/a | depends on 2.2 |
| Task 2.4 | pending | Route memo-cli parse and unknown-subcommand errors through helper | n/a | depends on 1.2 (now satisfied), 2.2 |
| Task 2.5 | pending | Add memo-cli exit-code matrix test | n/a | depends on 2.1 |
| Task 3.1 | pending | Migrate `agent-workflow-primitives` binaries | n/a | depends on Sprint 1 (now satisfied) |
| Task 3.2 | pending | Migrate API testing stack | n/a | depends on Sprint 1 (now satisfied) |
| Task 3.3 | pending | Migrate remaining single-binary crates and run the full gate | n/a | depends on Sprint 1 (now satisfied) |
| Task 3.4 | pending | Workspace lint script for legacy contract usage | n/a | depends on 3.1, 3.2, 3.3 |

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `bash scripts/ci/plan-bundle-validate.sh --strict` | pass | wired into docs-only entrypoint; all 5 plan bundles validated | scripts/ci/nils-cli-checks-entrypoint.sh --docs-only output |
| `cargo test -p nils-common cli_contract` | pass | 6 unit + 4 integration tests for envelope, exit constants, parse-error helper | terminal log |
| `cargo test -p nils-cli-template` | pass | 13 integration tests (existing 4 + 9 new contract assertions) | terminal log |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` | pass | docs placement + hygiene + markdownlint + plan-bundle validate green | terminal log |
| `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh` | pass | full required checks (cargo fmt, clippy -D warnings, nextest workspace, doctests, third-party audit) | terminal log |
| `cargo test -p memo-cli` | pending | per Sprint 2 | n/a |
| `cargo test --workspace` | pending | per Sprint 3 | n/a |

## Blockers

- none

## Session Log

### 2026-05-19

- Read:
  - docs/plans/cli-output-contract-unification/cli-output-contract-unification-plan.md
  - docs/plans/cli-output-contract-unification/cli-output-contract-unification-review-source.md
  - crates/nils-common/src/{lib.rs,process.rs}, crates/nils-common/Cargo.toml
  - crates/cli-template/{src/main.rs,Cargo.toml,tests/integration/cli.rs}
  - crates/memo-cli/src/cli.rs (reference for OutputFormat shape)
  - docs/specs/{workspace-shared-crate-boundary-v1.md,cli-service-json-contract-guideline-v1.md}
  - scripts/ci/{nils-cli-checks-entrypoint.sh,docs-hygiene-audit.sh}
  - DEVELOPMENT.md
- Changed:
  - crates/nils-common/Cargo.toml (added clap + serde deps)
  - crates/nils-common/src/{lib.rs,cli_contract.rs} (new module)
  - crates/nils-common/tests/{integration.rs,integration/cli_contract.rs}
  - crates/cli-template/Cargo.toml (added serde + serde_json deps)
  - crates/cli-template/src/main.rs (new contract wiring + `status` subcommand)
  - crates/cli-template/tests/integration/cli.rs (+9 contract assertions)
  - docs/specs/cli-output-contract-v1.md (new durable spec)
  - docs/plans/cli-output-contract-unification/cli-output-contract-unification-execution-state.md
  - THIRD_PARTY_LICENSES.md, THIRD_PARTY_NOTICES.md (regenerated after dep adds)
- Validated:
  - `cargo test -p nils-common cli_contract` (10 tests, 0 fail)
  - `cargo test -p nils-cli-template` (13 tests, 0 fail)
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` (pass)
  - `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh` (pass)
- Blocked by: none
- Next: open feature PR with `/deliver-feature-pr`; Sprint 2 picks up in a follow-up PR per the plan's PR strategy.
