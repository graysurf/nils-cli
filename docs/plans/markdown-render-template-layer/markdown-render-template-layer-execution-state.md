# Markdown Render Template Layer Execution State

## Execution State

- Status: in-progress (Sprint 1 amended; Task 1.1 next)
- Target scope: whole plan
- Execution window: Sprint 1 → Sprint 2 (9 PRs) → Sprint 3 (4 PRs)
- Staged execution confirmation: Sprint 1 scope reconciled with
  agent-runtime-cli reality (commit e297751)
- Current task: none (preflight + plan amendment complete)
- Next task: Task 1.1 — Scaffold `nils-markdown` crate
- Last updated: 2026-05-26
- Branch/commit: feat/markdown-render-template-layer-plan @ e297751
- Source document:
  docs/plans/markdown-render-template-layer/markdown-render-template-layer-plan.md
- Direct source-doc execution waiver: not applicable

## Task Ledger

| ID       | Status | Task                                                          | Evidence | Notes |
| -------- | ------ | ------------------------------------------------------------- | -------- | ----- |
| Task 1.1 | todo   | Scaffold `nils-markdown` crate                                |          |       |
| Task 1.2 | todo   | Engine builder and `RenderError`                              |          |       |
| Task 1.3 | todo   | `md_cell` filter and helper bridge                            |          |       |
| Task 1.4 | todo   | Relocate `agent-runtime-cli` helpers                          |          |       |
| Task 1.5 | todo   | Golden harness lift                                           |          |       |
| Task 1.6 | todo   | Workspace gate                                                |          |       |
| Task 2.1 | todo   | Migrate `plan-issue-cli/src/issue_body.rs`                    |          |       |
| Task 2.2 | todo   | Migrate `plan-issue-cli/src/task_spec.rs`                     |          |       |
| Task 2.3 | todo   | Migrate `plan-issue-cli/src/lifecycle_vnext/templates.rs`     |          |       |
| Task 2.4 | todo   | Migrate `plan-issue-cli/src/render.rs`                        |          |       |
| Task 2.5 | todo   | Migrate `plan-issue-cli/src/lifecycle_record.rs`              |          |       |
| Task 2.6 | todo   | Migrate `plan-issue-cli/src/execute.rs`                       |          |       |
| Task 2.7 | todo   | Migrate `agent-workflow-primitives/src/review_specialists.rs` |          |       |
| Task 2.8 | todo   | Migrate `agent-workflow-primitives/src/repo_retro.rs`         |          |       |
| Task 2.9 | todo   | Migrate `agent-workflow-primitives/src/heuristic_inbox.rs`    |          |       |
| Task 3.1 | todo   | Implement `md-render` binary main                             |          |       |
| Task 3.2 | todo   | Completion assets and workspace registration                  |          |       |
| Task 3.3 | todo   | README + CHANGELOG + agent-docs entry                         |          |       |
| Task 3.4 | todo   | Promote design principle to runbook                           |          |       |

## Validation

| Command | Status | Summary | Artifact |
| ------- | ------ | ------- | -------- |
|         |        |         |          |

## Blockers

- none

## Session Log

- 2026-05-26: Pre-execution scoping found two factual gaps between
  the plan and `agent-runtime-cli/src/render/`: Task 1.4 framed the
  four helpers (`cli_ref / script / skill_ref / state_out`) as
  relocatable, but each binds `ManifestSet / Skill / StateOutMode /
  CliToolsManifest` and cannot move into the lowest layer; Task 1.5
  named `agent-runtime-cli/src/render/golden.rs` as a byte-equality
  helper, but that file is the `--update-golden` fixture-refresh
  mode. Both tasks rewritten on `feat/markdown-render-template-layer-plan`
  (commit e297751): Sprint 1 now exposes
  `Engine::register_helper(name, F)` for consumers and adds a new
  `nils_markdown::golden::assert_render` helper; the four
  agent-runtime-cli helpers stay in place. Plan validates with
  `plan-tooling validate` exit 0.
