<!-- execute-from-tracking-issue:state:v1 -->
# plan-issue record restore — Execution State

## Execution State

- Status: ready-to-start; tracking issue not yet opened
- Target scope: whole plan
- Execution window: whole plan (two sprints)
- Current task: none (tracking issue not yet opened)
- Next task: Task 1.1 — snapshot parser (inverse of the renderer)
- Last updated: 2026-05-30 Asia/Taipei
- Branch/commit/PR/release: implementation branch `feat/plan-issue-record-restore`; commit/PR pending
- Source document: docs/plans/plan-issue-record-restore/plan-issue-record-restore-plan.md
- Discussion source document: docs/plans/plan-issue-record-restore/plan-issue-record-restore-discussion-source.md
- Source issue: none
- Tracking issue: pending — opened by `create-plan-tracking-issue`
- Source snapshot: pending
- Plan snapshot: pending
- Initial execution state snapshot: pending
- Direct source-doc execution waiver: not applicable

## Task Ledger

| ID       | Status  | Task                                       | Evidence | Notes                                                                          |
| -------- | ------- | ------------------------------------------ | -------- | ------------------------------------------------------------------------------ |
| Task 1.1 | pending | Snapshot parser (inverse of the renderer)  | —        | decode hex payload for path; extract content from `<details>`; latest per role |
| Task 1.2 | pending | `record restore` subcommand                | —        | reuse audit read path + offline JSON; `--out`, `--force`, `--format json`      |
| Task 1.3 | pending | Round-trip and edge tests                  | —        | open->restore round-trip; latest-state; missing-role; overwrite                |
| Task 2.1 | pending | Help, completion, and full required checks | —        | completion audits; `nils-cli-checks-entrypoint.sh --local-fast`                |

## Validation

| Command                                                      | Status  | Summary                            | Artifact |
| ------------------------------------------------------------ | ------- | ---------------------------------- | -------- |
| `cargo test -p nils-plan-issue-cli`                          | pending | parser, restore, round-trip, edges | —        |
| `bash scripts/ci/completion-flag-parity-audit.sh --strict`   | pending | new subcommand flag parity         | —        |
| `bash scripts/ci/completion-asset-audit.sh --strict`         | pending | completion asset audit             | —        |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` | pending | workspace local-fast gate          | —        |

## Blockers

- none

## Session Log

- 2026-05-30: Bundle drafted from the 2026-05-30 plan-bundle durability
  discussion. Confirmed gap: `plan-issue` embeds full bundle snapshots
  in the tracking issue but has no inverse to re-materialize them, and
  the hex payload carries only path/commit (content lives in the
  `<details>` block). `record restore` makes the issue a durable
  source-of-truth. Implementation not started; this bundle drives
  `record open` of the tracking issue, then the standard execute /
  deliver / closeout flow.
