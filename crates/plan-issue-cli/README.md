# plan-issue-cli

## Overview

`plan-issue-cli` provides the Rust command contract for plan/issue delivery orchestration. It is the typed replacement lane for
`plan-issue-delivery-loop.sh` behavior and is built around deterministic task-spec generation, issue-body rendering, and gate-enforced
sprint transitions. `Task Decomposition` is the runtime-truth execution table for the existing plan/sprint command family; sprint
task-spec/prompt artifacts are derived from those issue rows. `plan-tooling split-prs` provides grouping primitives only in the current
model; `plan-issue-cli` materializes runtime `Owner/Branch/Worktree/Notes` metadata from plan content plus grouping results.

For issue-backed tracking and dispatch workflows whose provider issue body is a mutable dashboard, use `plan-issue record ...`. The record
surface opens provider issues, posts append-only lifecycle comments, audits lifecycle markers, repairs dashboards, and closes records through
the strict lifecycle gate. Future state payload replacements are new-format-only: the active audit, dashboard repair, tracking status, and
close-ready paths target the current payload contract, with old provider issues handled by one-off migration/repair rather than permanent
old-format readers.

The crate ships two binaries with the same command surface:

- `plan-issue`: live GitHub-backed mode
- `plan-issue-local`: local-first rehearsal mode (offline/dry-run friendly)

Shell wrapper scripts are deprecated for this crate path. Use `plan-issue` / `plan-issue-local` directly.

## Command surface

### Build and preparation

- `build-task-spec`: build sprint-scoped task-spec TSV from a plan.
- `build-plan-task-spec`: build plan-scoped task-spec TSV (all sprints).

### Plan-level flow

- `start-plan`: open one plan issue and emit plan artifacts.
- `status-plan`: summarize Task Decomposition status from issue body/body file.
- `link-pr`: link a concrete PR to task rows and update row status (default `in-progress`).
- `ready-plan`: apply review-ready markers and optional review summary comment.
- `close-plan`: enforce final close gate and close the plan issue.
- `cleanup-worktrees`: enforce cleanup of all issue-assigned task worktrees.

### Sprint-level flow

- `start-sprint`: open sprint execution loop after previous sprint gate passes, validate runtime-truth rows against plan lanes, and render
  artifacts without rewriting issue rows.
- `ready-sprint`: post sprint-ready signal for main-agent review.
- `accept-sprint`: enforce merged-PR gate and mark sprint accepted.
- `multi-sprint-guide`: print repeated command flow for a whole plan.

### Shell completion

- `completion <bash|zsh>`: export completion script for each binary.

### Issue-backed records

- `record open`: open a provider issue from a validated plan bundle and seed source, plan, and initial state lifecycle comments.
- `record post`: append a canonical state, session, validation, or review lifecycle comment after validating the role-specific payload schema.
- `record audit`: inspect issue body Markdown plus provider comments JSON for recognized lifecycle markers and reject malformed typed payloads.
- `record repair-dashboard`: recompute and update the mutable dashboard from valid audit evidence.
- `record close`: run strict closeout, post closeout evidence, repair the final dashboard, and close the issue.

## Global flags

- `--repo <owner/repo>`: pass-through repo target for GitHub operations.
- `--dry-run`: print write actions without mutating GitHub state.
- `-f, --force`: bypass markdown payload guard for body/comment writes.
- `--json` or `--format json`: machine-readable contract output.
- `--format text`: human-readable output.

## Local-mode constraints

- `plan-issue-local` does not support live `--issue` paths that require GitHub reads/writes.
- Use `plan-issue <command>` for live operations.
- Use `--body-file` + `--dry-run` flows for local rehearsal where supported.
- `start-plan` in local mode emits deterministic placeholder issue number `999`.

## Task Decomposition schema

- Canonical table columns are fixed to:
  - `Task | Summary | Owner | Branch | Worktree | Execution Mode | PR | Status | Notes`
- Writer and parser share the same schema contract.
- Writer sanitizes cell values (including `|`) via `nils-common::markdown::canonicalize_table_cell` so parser column count remains
  deterministic and drift checks stay stable.
- Shared runtime lanes (`per-sprint`, `pr-shared`) must keep a consistent `PR` value across rows.

## Grouping and strategy rules

- `--strategy deterministic` requires `--pr-grouping` for split-dependent commands:
  - `build-task-spec`, `build-plan-task-spec`, `start-plan`, `start-sprint`, `ready-sprint`, `accept-sprint`.
- `--pr-grouping per-sprint`: one shared group per sprint.
- `--pr-grouping group --strategy deterministic`: requires explicit `--pr-group <task>=<group>` mappings.
- `--strategy auto` rejects `--pr-grouping`.
- `--strategy auto` resolves each sprint from plan metadata `PR grouping intent` first, then `--default-pr-grouping` for metadata gaps.
- `--strategy auto` allows optional pins only for sprints resolved to `group`; pins targeting `per-sprint` lanes fail fast.
- Use `plan-tooling validate` before orchestration when sprint metadata is present; invalid/partial metadata is blocked there.
- When a sprint resolves to a single shared PR group, `Execution Mode` is normalized to `per-sprint` (instead of `pr-shared`) to reflect
  single-lane execution semantics.
- Runtime lane metadata is materialized locally in `plan-issue-cli` (not read from split-prs runtime placeholders).

## Quick examples

```bash
# 1) Build plan-scoped task spec locally
plan-issue-local build-plan-task-spec \
  --plan docs/plans/example-plan.md \
  --pr-grouping per-sprint

# 2) Start plan issue in live mode
plan-issue start-plan \
  --repo owner/repo \
  --plan docs/plans/example-plan.md \
  --pr-grouping per-sprint

# 3) Local rehearsal start-plan (deterministic placeholder issue_number=999)
plan-issue-local --format json --dry-run start-plan \
  --plan docs/plans/example-plan.md \
  --pr-grouping per-sprint

# 4) Export completion
plan-issue completion zsh > completions/zsh/_plan-issue
plan-issue-local completion bash > completions/bash/plan-issue-local

# 5) Auto grouping with metadata fallback
plan-issue-local build-plan-task-spec \
  --plan docs/plans/example-plan.md \
  --strategy auto \
  --default-pr-grouping group

# 6) Open an issue-backed tracking record from a plan bundle
plan-issue --repo owner/repo record open \
  --bundle docs/plans/example

# 7) Post validation evidence to the tracking record
plan-issue --repo owner/repo record post \
  --issue 123 \
  --kind validation \
  --payload-file validation.json \
  --summary-file validation.md
```

## Exit codes

- `0`: success
- `1`: runtime/validation failure
- `2`: usage failure

## Canonical runtime artifact layout

`start-plan` and `start-sprint` materialize artifacts under
`<state-dir>/out/plan-issue-delivery/<repo-slug>/issue-<n>/...`, where
`<repo-slug>` is `owner__repo`. Plan-scope artifacts live under
`<issue-root>/plan/`; sprint-scope artifacts live under
`<issue-root>/sprint-<n>/{prompts,manifests,specs}/`. Per-task dispatch
records (`dispatch-<TASK_ID>.json`) carry the canonical nine-key shape;
runtime-adapter fields (`runtime_name`, `runtime_role`,
`runtime_role_fallback_reason`) are added by the active wrapper at
dispatch time and are intentionally absent from the binary's emission.

The state directory is resolved (in order) from `--state-dir <PATH>`
(global flag), the `PLAN_ISSUE_HOME` environment variable, or the
`${XDG_STATE_HOME:-$HOME/.local/state}/plan-issue` default. The
`AGENT_HOME` env var the binary previously consumed was retired in 0.8 —
adapters that drove plan-issue via `AGENT_HOME` should rename to
`PLAN_ISSUE_HOME` (or pass `--state-dir`) when upgrading.

See [`CLI contract v2`](docs/specs/plan-issue-cli-contract-v2.md)
"Canonical Runtime Artifacts (v2)" for the full path catalogue and
[`agent-kit RUNTIME_LAYOUT.md`](https://github.com/sympoies/agent-kit/blob/main/skills/automation/plan-issue-delivery/references/RUNTIME_LAYOUT.md)
for the upstream contract.

## Docs

- [Docs index](docs/README.md)

## Specifications

- [CLI contract v2](docs/specs/plan-issue-cli-contract-v2.md)
- [Issue-backed plan record contract v2](docs/specs/issue-backed-plan-record-contract-v2.md)
- [State machine and gate invariants v1](docs/specs/plan-issue-state-machine-v1.md)
- [Gate matrix v1](docs/specs/plan-issue-gate-matrix-v1.md)

## Fixtures

- Shell parity fixtures live under `tests/fixtures/shell_parity/`.
- Use `tests/fixtures/shell_parity/regenerate.sh` to refresh fixture snapshots when shell behavior intentionally changes.
