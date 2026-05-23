# plan-issue State Machine and Gate Invariants v1 (Task Decomposition Runtime)

> **Status:** Prior surface. v1 describes the `start-plan` /
> `start-sprint` Task Decomposition runtime. The default lifecycle for new
> agent-runtime-kit issue-backed records is described in
> [plan-issue state machine v2](plan-issue-state-machine-v2.md), which
> pairs with [issue-backed plan record contract v2](issue-backed-plan-record-contract-v2.md).
> v1 remains normative only for the Task Decomposition commands while
> they continue to ship. `## Task Decomposition` is no longer the default
> runtime-truth surface for agent-runtime-kit consumers; new lifecycles
> live in append-only canonical marker comments.

## Purpose

Define the v1 state machine and gate invariants for the Rust replacement of `plan-issue-delivery-loop.sh`.

This document is normative for lifecycle transitions that must remain compatible with the current shell orchestration flow.

## Scope

- In scope:
  - Plan-level lifecycle states and transition commands.
  - Sprint-level transition gates.
  - Task-row invariants required by issue body validation and close gates.
- Out of scope:
  - Human/JSON output envelope formatting.
  - Internal Rust module structure.

## Canonical State Objects

- Plan issue: one GitHub issue for the full plan lifecycle.
- Task rows: markdown table rows under `## Task Decomposition`.
- Sprint set: tasks associated with sprint `N` by either:
  - `Notes` token `sprint=S<N>`, or
  - task id pattern `S<N>T<k>`.

## Task Decomposition Runtime-Truth Row Model

- `## Task Decomposition` is the only runtime-truth execution table in the issue body for plan/sprint orchestration.
- No second issue-body dispatch table is introduced; task-spec and subagent prompt artifacts are derived views of the same runtime-truth
  rows.
- Column role split:
  - runtime-truth execution metadata: `Owner`, `Branch`, `Worktree`, `Execution Mode`, lane metadata tokens in `Notes`
  - runtime-progress fields: `PR`, `Status`
  - descriptive row identity: `Task`, `Summary`
- `Owner` is a stable dispatch alias (for example `subagent-s1-t1` or a shared-lane `dispatch` alias), not an ephemeral platform-internal
  worker identifier.

## Plan Lifecycle State Machine

States:

- `PLAN_UNSTARTED`: no plan issue exists yet.
- `PLAN_OPEN`: plan issue exists and is open.
- `PLAN_REVIEW_READY`: final plan review has been requested.
- `PLAN_CLOSED`: issue is closed and plan lifecycle is complete.

Transitions:

- `start-plan`: `PLAN_UNSTARTED -> PLAN_OPEN`
  - Creates one issue with full task decomposition initialized to `Status=planned`, `PR=TBD`.
  - Artifact emission (per `plan-issue-cli-contract-v2.md` Canonical Runtime Artifacts (v2)):
    - `MAIN_AGENT_INIT_SNAPSHOT_PATH` copied to `$ISSUE_ROOT/prompts/plan-issue-delivery-main-agent-init.snapshot.md`.
    - `PLAN_BRANCH_REF_PATH` written to `$ISSUE_ROOT/plan/plan-branch.ref` with the canonical plan branch name.
    - Plan-scope `TASK_SPEC` and rendered issue body written under `$ISSUE_ROOT/plan/`.
  - Failure to materialize any of the above is a hard gate failure with exit code `1` (see `plan-issue-gate-matrix-v1.md` `G11`).
- `ready-plan`: `PLAN_OPEN -> PLAN_REVIEW_READY`
  - Records final review intent (label/comment behavior may vary by flags).
- `close-plan`: `PLAN_OPEN|PLAN_REVIEW_READY -> PLAN_CLOSED`
  - Requires final approval comment URL.
  - Delegates close gate checks to `close-after-review`.
  - On success, enforces worktree cleanup.

`close-plan --dry-run` is a non-mutating transition preview and does not change state.

## Sprint Lifecycle State Machine

States per sprint `N`:

- `SPRINT_NOT_STARTED`
- `SPRINT_IN_PROGRESS`
- `SPRINT_REVIEW_READY`
- `SPRINT_ACCEPTED`

Transitions:

- `start-sprint N`: `SPRINT_NOT_STARTED -> SPRINT_IN_PROGRESS`
  - Renders sprint task-spec and subagent prompts.
  - Validates Task Decomposition runtime-truth rows against plan-derived sprint lanes.
  - Derives sprint task-spec and prompt artifacts from runtime-truth issue rows (no row rewrite).
  - For `N > 1`, requires previous sprint merge gate pass (see gate invariants).
  - Artifact emission (per `plan-issue-cli-contract-v2.md` Canonical Runtime Artifacts (v2)):
    - `PLAN_SNAPSHOT_PATH` copied to `$ISSUE_ROOT/plan/plan.snapshot.md`.
    - `SUBAGENT_INIT_SNAPSHOT_PATH` copied to `$SPRINT_ROOT/prompts/plan-issue-delivery-subagent-init.snapshot.md`.
    - `TASK_SPEC_PATH` written to `$SPRINT_ROOT/specs/sprint-task-spec.tsv`.
    - `TASK_PROMPT_PATH` written to `$SPRINT_ROOT/prompts/<TASK_ID>.md` per assigned task.
    - `PROMPT_MANIFEST_PATH` written to `$SPRINT_ROOT/manifests/prompt-manifest.tsv`.
    - `DISPATCH_RECORD_PATH` written to `$SPRINT_ROOT/manifests/dispatch-<TASK_ID>.json` per assigned task with the eleven
      required keys (see contract spec).
  - Failure to materialize any of the above is a hard gate failure with exit code `1` (see `plan-issue-gate-matrix-v1.md` `G11`).
- `ready-sprint N`: `SPRINT_IN_PROGRESS -> SPRINT_REVIEW_READY`
  - Posts or prints sprint-ready review artifact.
- `accept-sprint N`: `SPRINT_REVIEW_READY -> SPRINT_ACCEPTED`
  - Requires approval comment URL and merge gate pass.
  - Syncs all sprint `N` rows to `Status=done`.
  - Plan issue remains open after sprint acceptance.

## Task Row Status Machine

Allowed statuses:

- `planned`
- `in-progress`
- `blocked`
- `done`

Expected transitions:

- `planned -> in-progress` when implementation PR work starts.
- `in-progress -> done` only after acceptance/merge gates for that sprint pass.
- `planned|in-progress -> blocked` when execution is paused by external constraint.
- `blocked -> in-progress` when blocker is cleared.

Mutation command notes:

- `link-pr` records a concrete `PR` reference and updates selected row `Status` to one of `{planned, in-progress, blocked}`
  (default `in-progress`); `done` is reserved for sprint accept/close gates and is not selectable here.
- `link-pr --task <id>` expands to all rows in the same runtime PR lane for `per-sprint` / `pr-shared` rows to keep shared-lane `PR` values
  consistent.
- `link-pr --sprint <N>` must target a single runtime PR lane (or use `--pr-group`) to avoid ambiguous multi-lane updates.
- `link-pr` re-runs the issue body structural invariants on the post-mutation table before persisting (live binary writes the issue body;
  body-file mode rewrites the file unless `--dry-run`).

Row-level status rules:

- `Status in {in-progress, done}` requires non-placeholder values for:
  - `Owner`
  - `Branch`
  - `Worktree`
  - `Execution Mode`
  - `PR`
- `Status in {planned, blocked}` may keep placeholders.

## Gate Invariants

### Issue Body Structural Invariants

- `## Task Decomposition` table must exist with at least one task row.
- Required columns:
  - `Task`, `Summary`, `Owner`, `Branch`, `Worktree`, `Execution Mode`, `PR`, `Status`, `Notes`
- `Status` must be one of `{planned, in-progress, blocked, done}`.
- `Execution Mode` must be one of `{per-sprint, pr-isolated, pr-shared}` or `TBD`.
- Execution Mode derivation rule:
  - `group + auto|deterministic` that resolves to one shared PR lane for a sprint is represented as `per-sprint` (single-lane execution).
  - `group + auto|deterministic` with multiple resolved PR groups keeps `pr-shared` / `pr-isolated` per group size.
  - rows that share a `per-sprint` or `pr-shared` lane must keep canonical runtime-truth execution metadata aligned (`Owner`, `Branch`,
    `Worktree`).
  - `Notes` may remain task-specific, but shared-lane tokens used for lane parsing (for example `pr-group` and `shared-pr-anchor`) must stay
    compatible across rows in the same lane.
- Plan metadata grouping invariant:
  - `strategy=deterministic`: when sprint metadata includes `PR grouping intent`, it must match command `--pr-grouping`.
  - `strategy=auto`: sprint metadata decides grouping intent; `--default-pr-grouping` may fill metadata gaps.
  - `strategy=auto` without sprint metadata and without `--default-pr-grouping` is a hard gate failure for split/task-spec generation
    commands.
- Owner policy for non-planned/non-blocked rows:
  - must be a stable dispatch alias (`subagent-*` and shared-lane `dispatch` alias are valid shapes)
  - must not reference main-agent identity.
- `pr-isolated` rows must have unique `Branch` and unique `Worktree`.

### PR Normalization and Presence Invariants

- PR references are normalized to canonical `#<number>` when possible.
- Placeholder PR values (`TBD`, `-`, empty, etc.) are invalid for:
  - sprint merge/accept gates
  - close gate
  - rows with `Status in {in-progress, done}`.

### Sprint Progression Gate (`start-sprint N`, `N > 1`)

- Previous sprint `N-1` must pass merge gate:
  - every previous-sprint row has `Status=done`
  - every previous-sprint row has concrete PR reference
  - every referenced PR is merged.

### Sprint Acceptance Gate (`accept-sprint N`)

- Requires `--approved-comment-url` in valid GitHub issue/pull comment URL format.
- For sprint `N` rows:
  - every row has concrete PR reference
  - every referenced PR is merged.
- On pass, sprint `N` row statuses are synchronized to `done`.

### Plan Close Gate (`close-plan`)

- Requires final `--approved-comment-url` in valid GitHub issue/pull comment URL format.
- Delegated `close-after-review` invariants:
  - all task rows are `Status=done` unless `--allow-not-done` is explicitly used
  - every task row has non-placeholder PR
  - every referenced PR is merged
  - dispatch-owner alias policy passes (including subagent alias compatibility checks).
- After close gate success, strict task worktree cleanup is enforced.

### Worktree Cleanup Invariants

- Cleanup targets are resolved from Task Decomposition `Branch` and `Worktree`.
- Main repository worktree must never be removed.
- Cleanup succeeds only when:
  - targeted linked worktrees are removed
  - `git worktree prune` succeeds
  - no targeted residual linked worktree/path remains.

## Dry-Run Contract

- `--dry-run` commands print intended write actions and gate traces.
- Dry-run mode must not mutate GitHub issue/PR state.
- `close-plan --dry-run` requires `--body-file` to evaluate gates locally.

## Failure Contract

- Gate violations fail the command (`exit 1`) with explicit task-row diagnostics.
- Usage/argument errors return `exit 2`.
- Successful transitions return `exit 0`.
