<!-- execute-from-tracking-issue:state:v1 -->
# agent-run Direnv Exec Execution State

## Execution State

- Status: implementation-complete-delivery-ready
- Target scope: whole issue
- Execution window: whole issue
- Current task: Task 3.3
- Next task: update PR, post final review outcome, merge, and close out issue
- Last updated: 2026-05-24 20:27 Asia/Taipei
- Branch/commit/PR/release: `feat/agent-run-direnv-exec`; implementation PR
  [#469](https://github.com/sympoies/nils-cli/pull/469) opened as draft
- Source document:
  docs/plans/agent-run-direnv-exec/agent-run-direnv-exec-plan.md
- Discussion source document:
  docs/plans/agent-run-direnv-exec/agent-run-direnv-exec-discussion-source.md
- Source issue: [#467](https://github.com/sympoies/nils-cli/issues/467)
- Tracking issue: [#468](https://github.com/sympoies/nils-cli/issues/468)
- Source snapshot:
  [source snapshot](https://github.com/sympoies/nils-cli/issues/468#issuecomment-4528326114)
- Plan snapshot:
  [plan snapshot](https://github.com/sympoies/nils-cli/issues/468#issuecomment-4528326962)
- Initial execution state snapshot:
  [initial state snapshot](https://github.com/sympoies/nils-cli/issues/468#issuecomment-4528327593)
- Direct source-doc execution waiver: not applicable

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| Task 1.1 | done | Scaffold `agent-run` binary and clap surface | `cargo run -p nils-agent-workflow-primitives --bin agent-run -- --help` passed; `bash scripts/workspace-bins.sh \| grep '^agent-run$'` passed | Added binary target, root parser, subcommands, completion, and version support. |
| Task 1.2 | done | Implement env-file discovery and direnv decision model | `cargo test -p nils-agent-workflow-primitives agent_run` passed | Covers direct, required-missing, off/bypassed, active, blocked, and missing-direnv decisions. |
| Task 1.3 | done | Detect direnv availability and blocked environment state | `cargo test -p nils-agent-workflow-primitives --test integration agent_run` passed | Uses `direnv status --json` for status decisions without executing env loading during `doctor`/`env`. |
| Task 2.1 | done | Implement direct and off-mode `exec` | `cargo test -p nils-agent-workflow-primitives --test integration agent_run` passed | Preserves cwd, argv, stdout, stderr, and child exit code. |
| Task 2.2 | done | Implement fail-closed direnv `exec` | `cargo test -p nils-agent-workflow-primitives --test integration agent_run` passed | `.envrc` files run through `direnv exec`; bare `.env` files use `direnv dotenv json` when status has no loadable RC; blocked/missing/required-missing paths fail before child execution. |
| Task 2.3 | done | Implement `doctor` and `env` JSON contracts | `cargo run -p nils-agent-workflow-primitives --bin agent-run -- doctor --cwd . --format json` passed | Uses service envelopes with snake_case payload fields and no environment diff/value output. |
| Task 3.1 | done | Document agent-facing usage and binary dependency behavior | `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` passed | README/docs cover normalizer scope, quiet success path, no env diff, and no trust mutation. |
| Task 3.2 | done | Generate and validate completions | `zsh -n completions/zsh/_agent-run`; `bash -n completions/bash/agent-run` passed | Added tracked bash and zsh completion assets. |
| Task 3.3 | done | Run full gate and prepare PR delivery | `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage` passed | PR delivery and specialist review remain issue-lifecycle steps after commit. |

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `AGENT_DOCS_HOME=$HOMEProject/graysurf/agent-runtime-kit agent-docs resolve --context startup --strict --format checklist` | pass | Required startup preflight passed. | terminal log |
| `AGENT_DOCS_HOME=$HOMEProject/graysurf/agent-runtime-kit agent-docs resolve --context project-dev --strict --format checklist` | pass | Project-dev preflight passed. | terminal log |
| `AGENT_DOCS_HOME=$HOMEProject/graysurf/agent-runtime-kit agent-docs resolve --context task-tools --strict --format checklist` | pass | Task-tools preflight passed before GitHub issue work. | terminal log |
| `plan-issue record audit --profile tracking --body-file <body-live.md> --comments-json <comments-live.json> --format json` | pass | Tracking issue audit passed with source, plan, and state markers recognized. | `agent-out` run directory |
| `plan-tooling validate --file docs/plans/agent-run-direnv-exec/agent-run-direnv-exec-plan.md --format text --explain` | pass | Plan-bundle validation passed after resolving format findings. | terminal log |
| `cargo test -p nils-agent-workflow-primitives agent_run` | pass | Unit and integration filtered tests passed for the `agent-run` surface. | terminal log |
| `cargo test -p nils-agent-workflow-primitives --test integration agent_run` | pass | Direct, off, require, allowed `.envrc`, bare `.env`, blocked, env JSON, and doctor JSON integration tests passed. | terminal log |
| `cargo clippy -p nils-agent-workflow-primitives --all-targets -- -D warnings` | pass | Focused clippy pass for the changed crate. | terminal log |
| `cargo run -p nils-agent-workflow-primitives --bin agent-run -- --help` | pass | Help shows `exec`, `doctor`, `env`, `completion`, and `-V, --version`. | `agent-out` run directory |
| `cargo run -p nils-agent-workflow-primitives --bin agent-run -- doctor --cwd . --format json` | pass | Emits `cli.agent-run.doctor.v1` envelope with direct/absent decision in this repo. | `agent-out` run directory |
| `cargo run -p nils-agent-workflow-primitives --bin agent-run -- exec --cwd . -- sh -c 'pwd'` | pass | Direct execution preserved child stdout and printed the repo path. | `agent-out` run directory |
| `cargo run -q -p nils-agent-workflow-primitives --bin agent-run -- exec --cwd <agent-out dotenv probe> -- sh -c 'printf "%s" "$FROM_DOTENV"'` | pass | Real `direnv 2.37.1` `.env` probe printed `real-dotenv` through the `direnv dotenv json` fallback. | `agent-out` run directory |
| `bash scripts/workspace-bins.sh \| grep '^agent-run$'` | pass | Workspace binary inventory includes `agent-run`. | terminal log |
| `zsh -n completions/zsh/_agent-run` | pass | Zsh completion syntax passed. | terminal log |
| `bash -n completions/bash/agent-run` | pass | Bash completion syntax passed. | terminal log |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` | pass | Docs, plan-bundle, CLI output contract, and fixture lint checks passed. | terminal log |
| `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage` | pass | Required checks and coverage gate passed after review repair; nextest ran 3621 tests and coverage was 85.03%. | `target/coverage/lcov.info` |

## Runtime Findings

- `cli-output-contract-lint.sh` rejected initial camelCase payload fields in
  the new JSON structs; fixed by switching `agent-run` payloads to snake_case
  before rerunning docs-only and full gates.
- The implementation avoids running `direnv exec true` during `doctor` and
  `env`; status surfaces use `direnv status --json` so diagnostics do not load
  project env files.
- Delivery specialist review found that real `direnv 2.37.1` did not load a
  bare `.env` through `direnv exec` in the local probe even though the source
  scope includes `.env`; fixed by routing status-unknown `.env` execution
  through `direnv dotenv json` while keeping `doctor` / `env` value-free.

## Blockers

- none

## Session Log

### 2026-05-24 19:13 Asia/Taipei

- Converted the three source open questions into explicit v1 decisions:
  status/path-only `agent-run env`, quiet successful `agent-run exec`, and
  deferred `agent-runtime doctor --check-project` integration.
- Created the issue-backed implementation plan for source issue #467.
- Validation passed with `plan-tooling validate --file
  docs/plans/agent-run-direnv-exec/agent-run-direnv-exec-plan.md --format text
  --explain` and `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`.
- Opened tracking issue [#468](https://github.com/sympoies/nils-cli/issues/468)
  and posted source, plan, and state snapshots.
- Repaired the issue dashboard with exact snapshot URLs and verified it with
  `plan-issue record audit --profile tracking`.

### 2026-05-24 19:50 Asia/Taipei

- Implemented `agent-run` under `nils-agent-workflow-primitives` with `exec`,
  `doctor`, `env`, and `completion`.
- Added direnv-aware decision handling for `auto`, `require`, and `off`, with
  fail-closed behavior for missing or blocked project env.
- Added targeted integration coverage for direct execution, quiet child output,
  off-mode bypass, require failures, allowed direnv execution, blocked env
  files, and JSON status surfaces.
- Generated bash/zsh completions and updated docs plus binary dependency and
  completion matrix references.
- Ran focused and full repository validation. Required checks and coverage gate
  passed; next step is PR delivery, mandatory `code-review-specialists`, merge,
  and issue closeout.

### 2026-05-24 20:22 Asia/Taipei

- Opened draft PR [#469](https://github.com/sympoies/nils-cli/pull/469).
- Ran the mandatory delivery specialist gate and found one API-contract/testing
  blocker: fake-direnv coverage treated bare `.env` as loaded through
  `direnv exec`, but a real local `direnv 2.37.1` probe did not.
- Fixed bare `.env` execution by using `direnv dotenv json` as the fallback when
  `direnv status --json` has no loadable RC, without parsing `.env` values in
  `doctor` / `env` status paths.
- Reran focused tests, clippy, a real `.env` probe, docs-only checks, and the
  full `--with-coverage` workspace gate. Next: update PR #469, post the final
  review outcome, merge, and close issue #468.
