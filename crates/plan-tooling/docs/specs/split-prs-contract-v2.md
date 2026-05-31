# split-prs Contract v2

> Status: **Active contract.** Supersedes `split-prs-contract-v1.md`, which is retained for
> historical reference only.

## Purpose

`plan-tooling split-prs` emits deterministic grouping primitives for downstream orchestrators.

v2 intentionally removes task-level runtime execution metadata from `split-prs` output.
`plan-issue` materializes runtime lane metadata from:

- parsed plan task content
- `split-prs` grouping results (`task_id`, `summary`, `pr_group`)
- command prefixes / grouping strategy

## CLI

```text
plan-tooling split-prs \
  --file <plan.md> \
  --scope <plan|sprint> \
  [--sprint <n>] \
  [--pr-grouping <per-sprint|group>] \
  [--default-pr-grouping <per-sprint|group>] \
  [--pr-group <task-or-plan-id>=<group>]... \
  [--strategy <deterministic|auto>] \
  [--explain] \
  [--format <tsv|json>]
```

Value options accept both `--key value` and `--key=value`.

Compatibility note:

- Runtime-prefix compatibility options are still accepted by the CLI parser for older
  automation, but v2 `split-prs` output is grouping-only. Runtime execution metadata is generated in
  `plan-issue`.

## Determinism and Grouping Rules

- Output ordering remains deterministic: sprint number, then task appearance order.
- `pr_group` naming remains normalized and deterministic.
- `strategy=deterministic` requires `--pr-grouping`.
- `strategy=deterministic` + `pr-grouping=group` requires explicit `--pr-group` mapping for every task.
- `strategy=auto` rejects `--pr-grouping`.
- `strategy=auto` resolves grouping per sprint from `PR grouping intent` metadata first, then `--default-pr-grouping` if metadata is absent.
- `strategy=auto` fails when a selected sprint has neither `PR grouping intent` metadata nor `--default-pr-grouping`.
- `strategy=auto` allows optional pin mappings only for sprints resolved to `group`; pins targeting `per-sprint` lanes are rejected.
- `explain[].pr_grouping_intent_source` reports whether grouping intent came from:
  - command grouping (`command-pr-grouping`)
  - plan metadata (`plan-metadata`)
  - auto fallback (`default-pr-grouping`)

## Sprint Metadata Gate

The plan parser is strict about sprint metadata field names and values. These rules apply uniformly
to `to-json`, `validate`, `batches`, and `split-prs`:

- Canonical field names are case-sensitive: `**PR grouping intent**: per-sprint|group` and
  `**Execution Profile**: serial|parallel-xN`. Non-canonical casings (for example
  `PR Grouping Intent`) are rejected at parse time with `invalid metadata field <name>; use '<canonical>'`.
- Invalid values are rejected (`invalid PR grouping intent (expected per-sprint|group)`,
  `invalid Execution Profile (expected serial|parallel-xN)`).
- `validate` additionally enforces metadata coherence per sprint:
  - if either `PR grouping intent` or `Execution Profile` is present, both must be present
    (failure: `sprint metadata must include both \`PR grouping intent\` and \`Execution Profile\``).
  - `PR grouping intent: per-sprint` cannot be combined with `Execution Profile` parallel width
    `> 1` (failure: `\`PR grouping intent\` is per-sprint but \`Execution Profile\` indicates parallel
    width N; use \`PR grouping intent: group\` for multi-lane execution`).

## TSV Output (format=tsv)

Header:

```text
# task_id\tsummary\tpr_group
```

Columns:

- `task_id`: generated stable id (`SxTy`)
- `summary`: normalized task summary
- `pr_group`: resolved deterministic group key

## JSON Output (format=json)

Top-level object:

- `file`
- `scope`
- `sprint`
- `pr_grouping`
- `strategy`
- `records`
- optional `explain` (only with `--explain`)

`pr_grouping` reports the resolved grouping summary for the selected scope:

- `per-sprint` when every selected sprint resolves to `per-sprint`
- `group` when every selected sprint resolves to `group`
- `mixed` when the selected scope contains both resolved modes

`records[]` fields:

- `task_id`
- `summary`
- `pr_group`

`explain` continues to expose per-sprint grouping breakdown (`groups[].task_ids`, deterministic
anchor task id, optional sprint metadata hints, and `pr_grouping_intent_source`).

## Migration Notes (v1 -> v2)

Removed `split-prs` runtime metadata fields are now produced inside `plan-issue` from plan task
metadata, grouping results, and prefix inputs.

## Exit Codes

- `0`: success
- `1`: runtime / validation failure
- `2`: usage error
