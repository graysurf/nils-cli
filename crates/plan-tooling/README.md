# plan-tooling

## Overview

plan-tooling works with Plan Format v1 markdown files. It can parse plans to JSON, validate plan files, compute dependency batches for a
sprint, scaffold new plans, and generate task-to-PR split grouping primitives in deterministic or auto strategy modes. Runtime execution
metadata for orchestration is materialized by `plan-issue` from split results plus parsed plan content.

## Usage

```text
Usage:
  plan-tooling <command> [args]

Commands:
  to-json         Parse a plan markdown file into a stable JSON schema
  validate        Lint plan markdown files
  batches         Compute dependency layers (parallel batches) for a sprint
  artifact-audit  Classify durable coordination artifacts without side effects
  split-prs       Build task-to-PR split records (deterministic/auto)
  scaffold        Create a new plan from template
  completion      Export shell completion script
  help            Display help message

Help:
  plan-tooling help
  plan-tooling --help
```

## Commands

### to-json

- `to-json --file <plan.md> [--sprint <n>] [--pretty]`: Parse a plan file and output JSON.

### validate

- `validate [--file <path>]... [--format text|json]`: Validate plan files. With no `--file`, scans tracked `docs/plans/*-plan.md` files.
- `validate` requires a `Read First` section with `Primary source`, `Source type`, and `Open questions carried into execution`. Repo-local
  primary source paths must exist; use `Source type: plan-only waiver` for explicit plan-only exceptions.
- If `Complexity` is present it must parse as a 1-10 integer. Omit the field when complexity is intentionally unspecified.
- For plan-source bundles under `docs/plans/<slug>/`, `validate` also checks the sibling source-doc contract when the plan shape is
  `<slug>-plan.md`. Accepted source docs are `<slug>-discussion-source.md` and `<slug>-review-source.md`.
- Bundle validation accepts a not-yet-started bundle without an execution-state file. When `<slug>-execution-state.md` exists, it must point
  at the plan with `Source document`, or point directly at the source doc only with `Direct source-doc execution waiver`.
- Bundle validation is separate from durable-artifact cleanup audit. `validate` checks hard source/plan/state links; cleanup audit remains
  an advisory classification flow and does not delete, move, or archive files.

### batches

- `batches --file <plan.md> --sprint <n> [--format json|text]`: Compute dependency batches for a sprint.

### artifact-audit

- `artifact-audit --candidate <path>... [--repo <path>] [--format text|json] [--explain]`: Classify durable coordination artifacts
  without deleting or moving files.
- `artifact-audit --candidate-file <path> [--repo <path>] [--format text|json] [--explain]`: Read candidates from a newline-delimited
  file.
- Classifications are `delete`, `keep`, `rehome`, and `manual-review`.
- This helper is audit-only. Treat output as cleanup evidence, then use the owning workflow for review, approval, and any filesystem
  changes.

### split-prs

- `split-prs --file <plan.md> --scope <plan|sprint> [--sprint <n>]`
  `[--pr-grouping <per-sprint|group>] [--default-pr-grouping <per-sprint|group>]`
  `[--pr-group <task-or-plan-id>=<group>]... [--strategy deterministic|auto] [--explain] [--format json|tsv]`
- compatibility flags accepted by the CLI parser: `--owner-prefix`, `--branch-prefix`, `--worktree-prefix`
- value options accept both `--key value` and `--key=value`.
- `--owner-prefix`, `--branch-prefix`, and `--worktree-prefix` are accepted for compatibility with older automation, but v2 `split-prs`
  output is grouping-only (`task_id`, `summary`, `pr_group`).
- deterministic mode:
  - `--pr-grouping` is required.
  - `--pr-grouping per-sprint`: one shared `pr_group` per sprint (`s<n>`).
  - `--pr-grouping group`: pass `--pr-group` for every selected task.
- auto mode:
  - `--pr-grouping` is rejected.
  - sprint metadata `PR grouping intent` is authoritative when present.
  - `--default-pr-grouping` fills gaps only for sprints that omit grouping intent.
  - if a selected sprint has neither metadata nor `--default-pr-grouping`, the command fails.
  - scoring inputs are `Complexity`, dependency topology, and `Location` overlap.
  - for auto-resolved `group` sprints, `--pr-group` mappings are optional pins and remaining tasks are auto-grouped.
  - pins targeting auto-resolved `per-sprint` sprints are rejected.
  - when sprint metadata provides `Execution Profile` parallel width hints, auto grouping targets that lane count (deterministic fallback
    merges apply when needed).
  - parser metadata gates are strict; non-canonical field names (for example `PR Grouping Intent`) are rejected.
  - `--explain` includes `pr_grouping_intent_source` (`plan-metadata`, `default-pr-grouping`, or `command-pr-grouping`) for traceability.
  - ordering and tie-breakers stay deterministic (`Task N.M`, then `SxTy`, then lexical summary).
  - emitted grouping primitives (`task_id`, `summary`, `pr_group`, optional `--explain`) are consumed by `plan-issue` runtime
    materialization and runtime-truth validation.
- deterministic examples:
  - `split-prs --file docs/plans/example-plan.md --scope sprint --sprint 1 --pr-grouping per-sprint --format tsv`
  - `split-prs --file docs/plans/example-plan.md --scope sprint --sprint 2 --pr-grouping group --pr-group S2T1=isolated`
    `--pr-group S2T2=shared --pr-group S2T3=shared --format json`
- auto example:
  - `split-prs --file docs/plans/example-plan.md --scope sprint --sprint 2 --strategy auto --default-pr-grouping group --format json`
- rollback switchback:
  - if auto rollout is unhealthy, pin orchestration calls to `--strategy deterministic` until follow-up fixes land.

### scaffold

- `scaffold --slug <kebab-case> [--title <title>] [--force]`: Write to `docs/plans/<slug>/<slug>-plan.md` (or
  `docs/plans/<slug>/<slug>.md` if the slug already ends with `-plan`).
- `scaffold --file <path> [--title <title>] [--force]`: Write to a specific `-plan.md` path.

### completion

- `completion <bash|zsh>`: Export completion script for shell integration. Shell argument is
  positional and required (the subcommand does not honor `--help`).

### Sprint metadata hints (Plan markdown)

- Supported sprint metadata fields are case-sensitive and parser-enforced:
  - `**PR grouping intent**: per-sprint|group`
  - `**Execution Profile**: serial|parallel-xN`
- Parse flows fail fast on invalid metadata keys/values across `to-json`, `validate`, `batches`,
  and `split-prs`. Non-canonical casings (for example `PR Grouping Intent`) are rejected with
  `invalid metadata field <name>; use '<canonical>'`.
- `validate` additionally enforces metadata coherence per sprint:
  - if either field is present, both must be present
    (`sprint metadata must include both \`PR grouping intent\` and \`Execution Profile\``).
  - `PR grouping intent: per-sprint` cannot be combined with `Execution Profile` parallel width
    `> 1`.

## Quick examples

```bash
# Parse one plan to JSON
plan-tooling to-json --file docs/plans/example-plan.md --pretty

# Validate all tracked plan docs (default discovery)
plan-tooling validate

# Compute sprint batches in text mode
plan-tooling batches --file docs/plans/example-plan.md --sprint 2 --format text

# Audit a completed coordination artifact candidate
plan-tooling artifact-audit \
  --candidate docs/plans/example/example-plan.md \
  --format json

# Split sprint tasks with deterministic groups
plan-tooling split-prs \
  --file docs/plans/example-plan.md \
  --scope sprint \
  --sprint 2 \
  --pr-grouping group \
  --pr-group S2T1=isolated \
  --pr-group S2T2=shared \
  --strategy deterministic \
  --format json

# Export completion
plan-tooling completion zsh > completions/zsh/_plan-tooling
```

## Template

- Plan template: `crates/plan-tooling/plan-template.md`.

## Exit codes

- `0`: success and help output.
- `1`: validation or runtime errors.
- `2`: usage errors.

## Docs

- [Docs index](docs/README.md)
- [plan-source bundle contract v1](docs/specs/plan-source-bundle-contract-v1.md) — active contract
  for sibling source docs, plans, and optional execution state.
- [split-prs contract v2](docs/specs/split-prs-contract-v2.md) — active contract.
- [split-prs contract v1](docs/specs/split-prs-contract-v1.md) — deprecated; historical reference
  only.
- [split-prs build-task-spec cutover runbook](docs/runbooks/split-prs-build-task-spec-cutover.md)
