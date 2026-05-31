# split-prs Contract v1

> Status: **Deprecated, retained for historical reference.** The active contract is
> `split-prs-contract-v2.md`. v1 documents the pre-v2 output shape where `split-prs` emitted
> runtime execution metadata fields (`branch`, `worktree`, `owner`, `notes`). Those fields are now
> produced by `plan-issue` from grouping primitives plus parsed plan content. Do not author new
> tooling against v1.

## Purpose

`plan-tooling split-prs` converts plan tasks into executable PR slices while preserving a stable schema and deterministic ordering.

v1 runtime behavior:

- `strategy=deterministic` is fully implemented.
- `strategy=auto` is fully implemented with deterministic heuristics.

## CLI

```text
plan-tooling split-prs \
  --file <plan.md> \
  --scope <plan|sprint> \
  [--sprint <n>] \
  --pr-grouping <per-sprint|group> \
  [--pr-group <task-or-plan-id>=<group>]... \
  [--strategy <deterministic|auto>] \
  [--explain] \
  [--owner-prefix <text>] \
  [--branch-prefix <text>] \
  [--worktree-prefix <text>] \
  [--format <tsv|json>]
```

Value options accept both `--key value` and `--key=value`.

Defaults:

- `--scope sprint`
- `--strategy deterministic`
- `--owner-prefix subagent`
- `--branch-prefix issue`
- `--worktree-prefix issue__`
- `--format json`

## Scope Rules

- `scope=sprint` requires `--sprint <n>`.
- `scope=plan` includes all sprints and ignores `--sprint`.

## Strategy + Grouping Matrix

- `strategy=deterministic`, `pr-grouping=per-sprint`:
  - one `pr_group` per sprint (`s<n>`).
- `strategy=deterministic`, `pr-grouping=group`:
  - every selected task must have explicit `--pr-group` mapping.
  - mapping key can be generated task id (`SxTy`) or plan task id (`Task N.M`).
- `strategy=auto`, `pr-grouping=per-sprint`:
  - generated grouping is still deterministic and anchored by sprint key.
- `strategy=auto`, `pr-grouping=group`:
  - explicit `--pr-group` mappings are optional pins.
  - unmapped tasks are auto-assigned by rubric.
  - when sprint metadata provides `Execution Profile` parallel width hints, auto grouping targets that lane count with deterministic
    tie-break fallback.
  - output still preserves deterministic ordering and stable anchor semantics.

## Deterministic Normalization

- Generated task id: `S<sprint>T<index-within-sprint>` (1-based).
- Summary: normalized whitespace from task heading.
- Branch slug:
  - lowercase
  - non `[a-z0-9]` replaced with `-`
  - trimmed `-`
  - fallback `task-<index>`
  - max length 48
- Group key:
  - lowercase
  - non `[a-z0-9]` replaced with `-`
  - trimmed `-`
  - fallback from scope context
  - max length 48

## Deterministic Ordering

- Records are emitted by sprint number, then task appearance order in plan markdown.
- Group anchors are the first emitted record in each `pr_group`.
- Auto assignment tie-break rules are stable and deterministic:
  - primary stable key: `Task N.M`
  - secondary key: generated `SxTy`
  - tertiary key: lexical summary

## Auto Scoring Rubric (Runtime)

When `strategy=auto` is used in `pr-grouping=group`, grouping decisions use three scored signals:

- `Complexity`:
  - complexity is summed per candidate group.
  - merges are capped at total complexity `<= 20`.
  - missing/invalid complexity falls back to `5`.
- `Dependencies`:
  - dependency links between candidate groups increase merge affinity.
  - dependency layering contributes a serial-span penalty to reduce long-chain merges.
  - unresolved/external dependencies are ignored for intra-scope topology.
- `Location`:
  - same-layer tasks with normalized path overlap are unioned first.
  - overlap count contributes to merge affinity scoring.
  - missing locations simply contribute no overlap signal.

Deterministic tie-break algorithm for equal scores:

1. Prefer explicit pinned `--pr-group` entries.
2. Prefer highest scored merge candidates.
3. Prefer lexical `Task N.M`.
4. Prefer lexical generated `SxTy`.

## TSV Output (format=tsv)

Header:

```text
# task_id\tsummary\tbranch\tworktree\towner\tnotes\tpr_group
```

Column contract:

- `task_id`: generated stable id (`SxTy`)
- `summary`: task summary
- `branch`: target branch name
- `worktree`: target worktree name
- `owner`: subagent owner id
- `notes`: semicolon-separated metadata tokens
- `pr_group`: resolved group key

`notes` contains:

- `sprint=S<n>`
- `plan-task:Task N.M` (or fallback task id)
- optional `deps=...`
- optional `validate=<first validation command>`
- `pr-grouping=<mode>`
- `pr-group=<group>`
- optional `shared-pr-anchor=<task_id>` when group has multiple tasks

## JSON Output (format=json)

Object shape:

- `file`: input plan path
- `scope`: `plan` or `sprint`
- `sprint`: integer or null
- `pr_grouping`: `per-sprint` or `group`
- `strategy`: `deterministic` or `auto`
- `records`: array of records with the same fields as TSV columns
- optional `explain` (present only with `--explain`): per-sprint grouping breakdown including selected target parallel width and grouped
  task ids.

## Strategy Runtime Status

- `strategy=deterministic`: enabled in v1 runtime.
- `strategy=auto`: enabled in v1 runtime.
  - deterministic ordering and notes schema are preserved.
  - in `pr-grouping=group`, explicit pins are optional and validated when provided.

## Error Matrix

Core errors:

- missing `--file`
- plan file not found
- invalid/unknown `--scope`
- `scope=sprint` without valid `--sprint`
- invalid `--pr-grouping`
- `--pr-group` used when not in `group` mode
- empty selected scope (no tasks)

Deterministic/group-mode errors:

- `pr-grouping=group` without mappings
- unknown mapping key in `--pr-group`
- missing mapping for any selected task in `group` mode

Auto-mode errors:

- `strategy=auto` with unknown pinned mapping key
- `strategy=auto` with invalid pin syntax in `--pr-group`
- `strategy=auto` request where selected scope has no tasks

## Compatibility Guarantees

- TSV and JSON field names remain unchanged across strategies.
- `notes` key vocabulary stays backward compatible with downstream orchestration.
- Group naming remains normalized and deterministic to keep diff noise low.

## Exit Codes

- `0`: success
- `1`: runtime/validation failure
- `2`: usage error
