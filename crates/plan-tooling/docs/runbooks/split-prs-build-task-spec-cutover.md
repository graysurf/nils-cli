# split-prs Build Task-Spec Cutover

## Goal

Replace downstream ad-hoc split generation with `plan-tooling split-prs` grouping primitives while
keeping `plan-issue build-task-spec` as the runtime metadata materialization authority.

## Current Default Usage

For new orchestration callers, prefer the auto strategy with a `group` fallback. This is the
shape that current downstream automation drives by default:

```bash
plan-tooling split-prs \
  --file <plan.md> \
  --scope sprint \
  --sprint <n> \
  --strategy auto \
  --default-pr-grouping group \
  --format json
```

Notes:

- `--strategy auto` resolves grouping per sprint from `PR grouping intent` plan metadata first;
  `--default-pr-grouping group` is the fallback only for sprints whose metadata omits the field.
- `--strategy auto` rejects `--pr-grouping`. Use plan metadata or `--default-pr-grouping` instead.
- `--strategy deterministic` examples below remain valid for parity / regression cases and for
  manual rollback when the auto path is unhealthy.

The CLI flag default is still `--strategy deterministic`; the auto pattern above is the
recommended *operational* default for callers, not the implicit flag default.

## Command Mapping

Prior command shape:

```bash
build-task-spec --plan <plan.md> --sprint <n> --pr-grouping <mode> [--pr-group <task=group>]... --task-spec-out <out.tsv>
```

Equivalent split grouping primitive call (debug/inspection use):

```bash
plan-tooling split-prs \
  --file <plan.md> \
  --scope sprint \
  --sprint <n> \
  --pr-grouping <mode> \
  [--pr-group <task=group>]... \
  --strategy deterministic \
  --format json
```

## Compatibility Contract

`split-prs` no longer emits task-spec-compatible runtime metadata TSV. In the cutover model:

- `split-prs` emits grouping primitives only (`task_id`, `summary`, `pr_group`).
- `plan-issue` materializes `branch`, `worktree`, `owner`, and `notes` from plan content +
  grouping output.

Reduced `split-prs --format tsv` header:

```text
# task_id\tsummary\tpr_group
```

## Group Mode Rules

- `--pr-grouping group` + `--strategy deterministic`: pass `--pr-group` for every selected task.
- `--strategy auto`: omit `--pr-grouping`; sprint metadata decides grouping intent and `--default-pr-grouping` is the fallback.
- `--strategy auto` on group-resolved sprints: `--pr-group` mappings are optional pins; remaining tasks are auto-grouped.
- `--strategy auto` rejects `--pr-group` pins targeting sprints resolved as `per-sprint`.
- Mapping key accepts either generated id (`SxTy`) or plan task id (`Task N.M`).
- Shared group output should map to one PR (`pr-shared` downstream execution mode).

## Parity Checks

### CI-required checks

```bash
mkdir -p "$AGENT_HOME/out/plan-issue-delivery-loop"

plan-tooling split-prs \
  --file crates/plan-tooling/tests/fixtures/split_prs/duck-plan.md \
  --scope sprint \
  --sprint 1 \
  --pr-grouping per-sprint \
  --strategy deterministic \
  --format tsv > "$AGENT_HOME/out/plan-issue-delivery-loop/duck-s1.tsv"

plan-tooling split-prs \
  --file crates/plan-tooling/tests/fixtures/split_prs/duck-plan.md \
  --scope sprint \
  --sprint 2 \
  --pr-grouping group \
  --pr-group S2T1=s2-isolated \
  --pr-group S2T2=s2-shared \
  --pr-group S2T3=s2-shared \
  --strategy deterministic \
  --format tsv > "$AGENT_HOME/out/plan-issue-delivery-loop/duck-s2.tsv"

rg -n '^# task_id\tsummary\tpr_group$' \
  "$AGENT_HOME/out/plan-issue-delivery-loop/duck-s1.tsv" \
  "$AGENT_HOME/out/plan-issue-delivery-loop/duck-s2.tsv"
```

### Local corpus manual regression (optional)

This loop is intentionally no-op safe so CI does not depend on machine-local repos. Use it when
you have a local corpus checkout and want manual regression visibility before changing heuristics.

```bash
LOCAL_CORPUS="$PWD/docs/plans"
OUT_DIR="${AGENT_HOME}/out/plan-tooling-split-prs"

if [ -d "$LOCAL_CORPUS" ]; then
  mkdir -p "$OUT_DIR"
  find "$LOCAL_CORPUS" -name '*-plan.md' -exec plan-tooling validate --file '{}' ';'

  # Deterministic baseline output check for an overlap-heavy plan.
  plan-tooling split-prs \
    --file "$LOCAL_CORPUS/plan-tooling-split-prs-cutover-plan.md" \
    --scope sprint \
    --sprint 2 \
    --pr-grouping per-sprint \
    --strategy deterministic \
    --format json | jq -S . > "$OUT_DIR/split-prs-local-corpus-s2-deterministic.json"

  # Auto-mode smoke check for generated grouping without manual mapping.
  plan-tooling split-prs \
    --file "$LOCAL_CORPUS/plan-tooling-split-prs-cutover-plan.md" \
    --scope sprint \
    --sprint 2 \
    --pr-grouping group \
    --strategy auto \
    --format json | jq -S . > "$OUT_DIR/split-prs-local-corpus-s2-auto.json"

  # Conflict-risk visibility: inspect shared-group concentration by sprint record.
  jq -r '.records[] | [.task_id, .pr_group, .summary] | @tsv' \
    "$OUT_DIR/split-prs-local-corpus-s2-auto.json" | sort
fi
```

## Auto Strategy

`--strategy auto` is implemented with deterministic grouping output semantics:

- scores merge candidates using `Complexity`, dependency topology, and `Location` overlap.
- preserves stable output ordering and reduced record schema.
- allows optional `--pr-group` pins in `pr-grouping=group`.
