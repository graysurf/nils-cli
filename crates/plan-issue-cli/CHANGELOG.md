# Changelog

All notable changes to `nils-plan-issue-cli` are documented here. The
format follows Keep a Changelog and the project follows semantic
versioning.

## [Unreleased]

### BREAKING

- `start-plan` and `start-sprint` retire the previous flat artifact
  layout under `$AGENT_HOME/out/plan-issue-delivery/<plan-slug>-...`
  and now materialize every required artifact under the canonical
  nested layout
  `$AGENT_HOME/out/plan-issue-delivery/<repo-slug>/issue-<n>/...`
  defined by
  [`agent-kit RUNTIME_LAYOUT.md`](https://github.com/sympoies/agent-kit/blob/main/skills/automation/plan-issue-delivery/references/RUNTIME_LAYOUT.md)
  and [`docs/plans/plan-issue-cli-canonical-runtime-artifacts-plan.md`](../../docs/plans/plan-issue-cli-canonical-runtime-artifacts-plan.md).
  Retired filenames:
  - `<plan-slug>-plan-tasks.tsv`
  - `<plan-slug>-plan-issue-body.md`
  - `<plan-slug>-sprint-<N>-tasks.tsv`
  - `<plan-slug>-sprint-<N>-subagent-prompts/<anchor>-subagent-prompt.md`
  Migration: the only known consumer is the `plan-issue-delivery`
  wrapper (claude-kit / codex / opencode adapters), which already
  expects the canonical layout. Direct callers should switch to the
  new `task_spec_path`, `issue_body_path`, `sprint_root`,
  `plan_snapshot_path`, `subagent_init_snapshot_path`,
  `prompt_manifest_path`, and `dispatch_record_paths` fields in the
  command JSON output instead of computing flat paths from the plan
  slug.
- `start-plan` and `start-sprint` now hard-require `AGENT_HOME` to be
  set; missing or empty `AGENT_HOME` fails fast with exit code `1` and
  the `runtime-layout-failed` error code.
- `start-sprint` writes one `<TASK_ID>.md` prompt per dispatched task
  under `$SPRINT_ROOT/prompts/`. The retired
  `<anchor>-subagent-prompt.md` flat filename is no longer produced.

### Added

- `runtime_layout` module exposing `runtime_root`, `repo_slug`,
  `IssueRoot`, `SprintRoot`, and `RuntimeLayoutError` for canonical
  path math.
- `dispatch_record` module with the ten-key `DispatchRecord`
  serializer (`task_id`, `task_prompt_path`,
  `subagent_init_snapshot_path`, `plan_snapshot_path`, `worktree`,
  `branch`, `execution_mode`, `pr_group`, `base_branch`,
  `workflow_role`) and `write_dispatch_record` helper. Optional
  adapter fields (`runtime_name`, `runtime_role`,
  `runtime_role_fallback_reason`) are intentionally absent from the
  binary's emission and are added post-emission by the wrapper /
  main-agent.
- `start-plan` JSON gains `issue_root`,
  `main_agent_init_snapshot_path`, and `plan_branch_ref_path`.
- `start-sprint` JSON gains `sprint_root`, `plan_snapshot_path`,
  `subagent_init_snapshot_path`, `prompt_manifest_path`, and
  `dispatch_record_paths`.
- Gate matrix `G11` (canonical runtime artifact emission) wired into
  the spec.
