# plan-issue CLI Contract v2

## Purpose

v2 defines the runtime metadata ownership model after split-prs output decoupling.

Current contract boundary:

- `plan-tooling split-prs` provides grouping primitives only (`task_id`, `summary`, `pr_group`).
- `plan-issue-cli` is the authority that materializes executable runtime metadata
  (`Owner`, `Branch`, `Worktree`, `Notes`) for task-spec artifacts and `Task Decomposition` rows.

## Runtime Metadata Materialization (v2)

`plan-issue-cli` runtime metadata is derived from:

- parsed plan tasks (`Task N.M`, dependencies, validation commands)
- split-prs grouping output (`task_id`, `summary`, `pr_group`)
- command grouping/strategy (`--strategy`, deterministic `--pr-grouping`, auto `--default-pr-grouping`)
- prefix options (`--owner-prefix`, `--branch-prefix`, `--worktree-prefix`)

Rules:

- `Owner` / `Branch` / `Worktree` are lane-canonical for shared lanes (`per-sprint`, `pr-shared`).
- `Notes` are task-specific and include shared-lane tokens when applicable.
- Anchor selection for runtime lane materialization is deterministic from lane membership
  (stable task ordering), not passthrough split-prs task placeholders.
- `strategy=deterministic` requires command `--pr-grouping`; if sprint metadata declares `PR grouping intent`, it must match or task-spec
  generation fails before issue/comment artifact writes.
- `strategy=auto` resolves each sprint from plan metadata `PR grouping intent` first and uses `--default-pr-grouping` only when metadata is
  absent.

## Notes Token Contract (v2)

Materialized `Notes` tokens include:

- `sprint=S<n>`
- `plan-task:Task N.M` (or deterministic fallback task id)
- optional `deps=...`
- optional `validate=...`
- `pr-grouping=<mode>`
- `pr-group=<group>`
- optional `shared-pr-anchor=<task_id>` for shared lanes

## Markdown Canonicalization Dependency

`plan-issue-cli` must use shared helpers from `nils-common::markdown` for:

- markdown payload validation (`validate_markdown_payload`)
- markdown-table-safe cell canonicalization (`canonicalize_table_cell`)

This prevents drift caused only by markdown table rendering/parsing normalization (`|`, `\n`, `\r`).

GitHub integration boundary:

- Live issue/pull writes remain crate-local `gh` adapter behavior (`plan-issue-cli` ownership).
- `nils-common` does not provide a shared `github` module for these operations.

## Task Decomposition Runtime-Truth Contract (v2)

- `## Task Decomposition` remains the single runtime-truth execution table.
- Task-spec TSV and subagent prompt files are derived artifacts.
- Drift checks compare issue rows against plan-issue materialized runtime metadata (not split-prs
  runtime placeholders).
- `group + auto|deterministic` single-lane sprints are normalized to `Execution Mode=per-sprint`.

## Task-spec TSV Header (unchanged)

```text
# task_id\tsummary\tbranch\tworktree\towner\tnotes\tpr_group
```

The header remains stable in v2; only the metadata generation authority changed.

## Canonical Runtime Artifacts (v2)

The `start-plan` and `start-sprint` commands materialize runtime artifacts under
the canonical layout defined by
`agent-kit/skills/automation/plan-issue-delivery/references/RUNTIME_LAYOUT.md`.

Layout root and namespacing:

- `RUNTIME_ROOT="$AGENT_HOME/out/plan-issue-delivery"`.
- Repository slug: `<repo-slug>` derived from `owner/repo` as `owner__repo`
  (double underscore separator).
- `ISSUE_ROOT="$RUNTIME_ROOT/<repo-slug>/issue-<ISSUE_NUMBER>"`.
- `SPRINT_ROOT="$ISSUE_ROOT/sprint-<N>"`.

Plan-scoped artifacts (owned by `start-plan`):

- `MAIN_AGENT_INIT_SNAPSHOT_PATH="$ISSUE_ROOT/prompts/plan-issue-delivery-main-agent-init.snapshot.md"`
  copied from `MAIN_AGENT_INIT_SOURCE_PATH="$AGENT_HOME/prompts/plan-issue-delivery-main-agent-init.md"`.
- `PLAN_SNAPSHOT_PATH="$ISSUE_ROOT/plan/plan.snapshot.md"` copied from the
  source plan path. `start-sprint` may also rewrite this file when invoked
  before the snapshot exists; the canonical contract treats it as
  immutable-per-issue but does not forbid rewriting on resume.
- `PLAN_BRANCH_REF_PATH="$ISSUE_ROOT/plan/plan-branch.ref"` containing the
  canonical plan branch name (for example `plan/issue-<n>`), no trailing
  newline, UTF-8.
- Plan-scope task-spec TSV at `$ISSUE_ROOT/plan/tasks.tsv`.
- Plan-scope rendered issue body at `$ISSUE_ROOT/plan/issue-body.md`.

Sprint-scoped artifacts (owned by `start-sprint`):

- `SUBAGENT_INIT_SNAPSHOT_PATH="$SPRINT_ROOT/prompts/plan-issue-delivery-subagent-init.snapshot.md"`
  copied from `SUBAGENT_INIT_SOURCE_PATH="$AGENT_HOME/prompts/plan-issue-delivery-subagent-init.md"`.
- `TASK_PROMPT_PATH="$SPRINT_ROOT/prompts/<TASK_ID>.md"` (one file per
  dispatched task; replaces the prior `<anchor>-subagent-prompt.md` flat
  filename).
- `PROMPT_MANIFEST_PATH="$SPRINT_ROOT/manifests/prompt-manifest.tsv"` with
  header `task_id\tprompt_path\texecution_mode\tworkflow_role` and one row
  per task.
- `TASK_SPEC_PATH="$SPRINT_ROOT/specs/sprint-task-spec.tsv"` (sprint-scope
  task-spec TSV).

Per-task dispatch record (owned by `start-sprint`):

- `DISPATCH_RECORD_PATH="$SPRINT_ROOT/manifests/dispatch-<TASK_ID>.json"`.
- Required keys (snake_case, ten total):
  - `task_id`
  - `task_prompt_path`
  - `subagent_init_snapshot_path`
  - `plan_snapshot_path`
  - `worktree`
  - `branch`
  - `execution_mode`
  - `pr_group`
  - `base_branch`
  - `workflow_role`
- Default `workflow_role` is `"implementation"`. Review and monitor records
  are dispatched ad-hoc by the main agent and have no per-task record at
  `start-sprint` time (per `AGENT_ROLE_MAPPING.md`).
- Optional adapter keys (`runtime_name`, `runtime_role`,
  `runtime_role_fallback_reason`) are intentionally **absent** from the
  binary's emission. The active runtime adapter (claude-code, codex,
  opencode) injects them at dispatch time per canonical
  `agent-kit/skills/automation/plan-issue-delivery/SKILL.md` L81-82 +
  L138-144.

Worktree path rules:

- `WORKTREE_ROOT="$ISSUE_ROOT/worktrees"`.
- Mode mapping:
  - `pr-isolated`: `"$WORKTREE_ROOT/pr-isolated/<TASK_ID>"`.
  - `pr-shared`: `"$WORKTREE_ROOT/pr-shared/<PR_GROUP>"`.
  - `per-sprint`: `"$WORKTREE_ROOT/per-sprint/sprint-<N>"`.

`AGENT_HOME` requirement:

- The canonical layout hard-requires `AGENT_HOME` to be set and non-empty.
  `start-plan` and `start-sprint` fail with exit code `1` and a runtime
  error referencing the `AGENT_HOME` env var when it is missing.

### Breaking Change: Retired Flat Layout

This contract **retires** the prior flat artifact layout entirely. The
retired filenames and the migration note are catalogued in the crate
[`CHANGELOG.md`](../../CHANGELOG.md) under the `BREAKING` section.
Downstream consumers must read artifacts from the canonical
`$ISSUE_ROOT` / `$SPRINT_ROOT` paths above.

Migration delivered by
[`docs/plans/plan-issue-cli-canonical-runtime-artifacts-plan.md`](../../../../docs/plans/plan-issue-cli-canonical-runtime-artifacts-plan.md).
