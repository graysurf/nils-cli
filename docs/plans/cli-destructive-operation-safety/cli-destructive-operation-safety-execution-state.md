# CLI Destructive Operation Safety Execution State

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
  docs/plans/cli-destructive-operation-safety/cli-destructive-operation-safety-plan.md
- Direct source-doc execution waiver: not applicable

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| Task 1.1 | pending | Add `--dry-run` to `git-lock unlock` | n/a | highest blast-radius safety fix |
| Task 1.2 | pending | Strengthen `git-lock delete` warning and add `--force` | n/a | |
| Task 1.3 | pending | Wrap `git-lock` errors with remediation hints | n/a | |
| Task 2.1 | pending | Redefine `memo-cli delete` semantics | n/a | breaking change; needs release note |
| Task 2.2 | pending | Fail fast on `memo-cli apply --stdin` with TTY stdin | n/a | |
| Task 2.3 | pending | Add change preview to `memo-cli apply --dry-run` | n/a | depends on 2.2 (same PR scope) |
| Task 3.1 | pending | TTY confirmation for `heuristic-inbox archive` | n/a | |
| Task 3.2 | pending | Move `codex-rate-limits --watch` guard into clap | n/a | |
| Task 3.3 | pending | `codex-remove` non-interactive without `--yes` is a usage error | n/a | |
| Task 3.4 | pending | Add `nils-term::prompt::confirm` shared helper | n/a | depends on 1.2, 2.1, 3.1 |

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `bash scripts/ci/plan-bundle-validate.sh --strict` | pending | run before first commit | n/a |
| `cargo test -p git-lock` | pending | per Sprint 1 | n/a |
| `cargo test -p memo-cli` | pending | per Sprint 2 | n/a |
| `cargo test -p agent-workflow-primitives heuristic_inbox` | pending | per Task 3.1 | n/a |
| `cargo test -p codex-cli` | pending | per Tasks 3.2, 3.3 | n/a |
| `cargo test -p nils-term prompt` | pending | per Task 3.4 | n/a |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh` | pending | end of Sprint 3 | n/a |

## Blockers

- none

## Session Log

(none yet)
