<!-- execute-from-tracking-issue:state:v1 -->
# forge-cli pr deliver preflight & DX — Execution State

## Execution State

- Status: complete; both sprints implemented and validated, ready for delivery
- Target scope: whole plan
- Execution window: whole plan (two sprints)
- Current task: none (all tasks done)
- Next task: deliver implementation PR + closeout
- Last updated: 2026-05-30 Asia/Taipei
- Branch/commit/PR/release: implementation branch `feat/forge-cli-pr-deliver-preflight-dx`; PR pending delivery
- Source document: docs/plans/forge-cli-pr-deliver-preflight-dx/forge-cli-pr-deliver-preflight-dx-plan.md
- Discussion source document: docs/plans/forge-cli-pr-deliver-preflight-dx/forge-cli-pr-deliver-preflight-dx-discussion-source.md
- Source issue: none
- Tracking issue: sympoies/nils-cli#650
- Source snapshot: posted at open
- Plan snapshot: posted at open
- Initial execution state snapshot: posted at open
- Direct source-doc execution waiver: not applicable

## Task Ledger

| ID       | Status | Task                                                   | Evidence                                                         | Notes                                                                     |
| -------- | ------ | ------------------------------------------------------ | ---------------------------------------------------------------- | ------------------------------------------------------------------------- |
| Task 1.1 | done   | Non-short-circuiting local-preflight runner            | `run_local_preflight` + `RuleVerdict` in `validations.rs`        | reuses Rules 1a-5; 7 verdicts, no early-exit; pure string/git             |
| Task 1.2 | done   | Report verdicts in `pr deliver --dry-run`              | `local_preflight[]` added to `emit_dry_run` envelope             | additive field; resolves branch/body; no provider backend in dry-run      |
| Task 1.3 | done   | Regression guard + `--dry-run` help text               | `pr_deliver_dry_run_reports_local_preflight_without_backend`     | FORBIDDEN_STUB proves no backend call; deliver after_help documents it    |
| Task 2.1 | done   | Aggregate body-section validation                      | `body_sections` in `validations.rs`; create call sites rerouted  | both-missing -> `body_missing_sections`; one-missing keeps canonical code |
| Task 2.2 | done   | pr-body kind parity + `body_missing_*` cross-reference | `PrBodyKind` + `render_generic`; `BODY_SCAFFOLD_HINT` in details | pr-body covers six deliver kinds; body errors point at the scaffold       |
| Task 2.3 | done   | Full required checks + completion audits               | flag-parity + asset audits PASS; `--local-fast` PASS             | no new dependency; no `Cargo.lock` drift                                  |

## Validation

| Command                                                      | Status | Summary                                           | Artifact |
| ------------------------------------------------------------ | ------ | ------------------------------------------------- | -------- |
| `cargo test -p nils-forge-cli`                               | pass   | preflight runner + dry-run + body aggregation     | —        |
| `cargo test -p agent-runtime-cli`                            | pass   | pr-body render kind parity (generic kinds)        | —        |
| `bash scripts/ci/completion-flag-parity-audit.sh --strict`   | pass   | required completion flag parity                   | —        |
| `bash scripts/ci/completion-asset-audit.sh --strict`         | pass   | required completion asset audit                   | —        |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` | pass   | workspace local-fast gate (fmt/clippy/build/test) | —        |

## Blockers

- none

## Session Log

- 2026-05-30: Bundle drafted from the 2026-05-30 forge-cli `pr deliver`
  DX discussion. Root cause confirmed in source: `pr_deliver.rs:109-110`
  early-returns `emit_dry_run` before any of the Rules 1a-5 local
  validations run, so `--dry-run` cannot predict the real run's local
  gates. The one open question (aggregated body-error code shape) was
  resolved at the source doc (keep per-section codes in `details`).
  Implementation not started; this bundle drives `record open` of the
  tracking issue, then the standard execute / deliver / closeout flow.
- 2026-05-30: Implemented all six tasks on
  `feat/forge-cli-pr-deliver-preflight-dx`. Sprint 1: added
  `run_local_preflight` (non-short-circuiting over Rules 1a-5 -> seven
  `RuleVerdict`s) and an additive `local_preflight[]` block on the
  `pr deliver --dry-run` envelope; the dry-run resolves branch/body and
  runs only local string/git checks, never a provider backend (pinned by
  a FORBIDDEN_STUB regression test), and the deliver `after_help`
  documents it. Sprint 2: added `body_sections` (both-missing ->
  `body_missing_sections` aggregate, one-missing -> canonical code) and
  routed the `pr create` chain through it; extended `pr-body render`
  `--kind` to the six deliver kinds via a generic skeleton; and added a
  `BODY_SCAFFOLD_HINT` pointer to the body-missing error details. All
  required checks green; no new dependency; no `Cargo.lock` drift.
