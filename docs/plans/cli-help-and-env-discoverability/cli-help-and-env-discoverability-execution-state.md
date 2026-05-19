# CLI Help and Env Discoverability Execution State

## Current State

- Status: not started
- Target scope: whole plan
- Execution window: undecided
- Staged execution confirmation: not applicable
- Current task: Task 1.1
- Next task: Task 1.1
- Last updated: 2026-05-19
- Branch/commit: not started
- Source document:
  docs/plans/cli-help-and-env-discoverability/cli-help-and-env-discoverability-plan.md
- Direct source-doc execution waiver: not applicable

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| Task 1.1 | pending | Draft `cli-help-style-guide.md` | n/a | foundation |
| Task 1.2 | pending | Migrate `memo-cli` to the style guide | n/a | depends on 1.1 |
| Task 1.3 | pending | Add structural help-snapshot test for `memo-cli` | n/a | depends on 1.2 |
| Task 2.1 | pending | Apply style guide to `agent-workflow-primitives` binaries | n/a | depends on 1.1 |
| Task 2.2 | pending | Apply style guide to API testing binaries and surface env vars | n/a | depends on 1.1 |
| Task 2.3 | pending | Apply style guide to remaining clap-derive binaries | n/a | depends on 1.1 |
| Task 2.4 | pending | Fix `api-gql` implicit default subcommand documentation | n/a | depends on 2.2 |

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `bash scripts/ci/plan-bundle-validate.sh --strict` | pending | run before first commit | n/a |
| `cargo test -p memo-cli help_snapshot` | pending | per Sprint 1 | n/a |
| `cargo test --workspace help_snapshot` | pending | per Sprint 2 | n/a |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` | pending | after style guide lands | n/a |

## Blockers

- none

## Session Log

(none yet)
