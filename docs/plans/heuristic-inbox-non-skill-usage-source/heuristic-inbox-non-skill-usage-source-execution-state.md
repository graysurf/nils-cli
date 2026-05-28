<!-- execute-from-tracking-issue:state:v1 -->
# heuristic-inbox `new` non-skill-usage source mode — Execution State

## Execution State

- Status: complete
- Target scope: whole plan
- Execution window: whole plan (single sprint)
- Current task: complete — PR #595 merged
- Next task: close-ready handoff to `plan-tracking-issue-closeout`
- Last updated: 2026-05-28 Asia/Taipei
- Branch/commit/PR/release: `feat/585-heuristic-inbox-non-skill-usage-source`; PR sympoies/nils-cli#595 squash-merged as `0669ad2`
- Source document: docs/plans/heuristic-inbox-non-skill-usage-source/heuristic-inbox-non-skill-usage-source-plan.md
- Discussion source document: docs/plans/heuristic-inbox-non-skill-usage-source/heuristic-inbox-non-skill-usage-source-discussion-source.md
- Source issue: sympoies/nils-cli#585
- Tracking issue: pending (this bundle drives `plan-issue record open`)
- Source snapshot: pending
- Plan snapshot: pending
- Initial execution state snapshot: pending
- Direct source-doc execution waiver: not applicable

## Task Ledger

| ID       | Status   | Task                                                            | Evidence       | Notes                                                           |
| -------- | -------- | --------------------------------------------------------------- | -------------- | --------------------------------------------------------------- |
| Task 1.1 | complete | Add `--from-evidence` / `--manual` sources and exclusivity gate | commit 34cd315 | `new_source` ArgGroup; `from_skill_usage` now `Option<PathBuf>` |
| Task 1.2 | complete | Resolve each source and compose the entry                       | commit 34cd315 | per-source resolvers + shared `compose_entry`; reuses redaction |
| Task 1.3 | complete | Help, completions, and tests                                    | commit 34cd315 | regenerated zsh/bash assets; 3 new integration tests            |

## Validation

| Command                                                           | Status | Summary                                            | Artifact                      |
| ----------------------------------------------------------------- | ------ | -------------------------------------------------- | ----------------------------- |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`      | pass   | fmt + clippy -D warnings + 4189 nextest tests pass | 585-validation/local-fast.log |
| `cargo test -p nils-agent-workflow-primitives --test integration` | pass   | heuristic_inbox suite incl. 3 new tests green      | 585-validation/local-fast.log |
| `bash scripts/ci/completion-flag-parity-audit.sh --strict`        | pass   | 38 required binaries, 0 failures with new flags    | —                             |
| `bash scripts/ci/completion-asset-audit.sh --strict`              | pass   | asset matrix coverage intact                       | —                             |

## Blockers

- none

## Session Log

- 2026-05-28: Bundle drafted retroactively against an already-implemented and
  locally-validated change (commit `34cd315` on
  `feat/585-heuristic-inbox-non-skill-usage-source`). All three Sprint 1 tasks
  are complete; local-fast, completion flag-parity, and asset audits are green
  with no Cargo.lock drift (no new dependencies). Remaining work is provider
  delivery: `record open` of the tracking issue closing #585, lifecycle
  checkpoints, PR open + squash-merge, and close-ready handoff.
