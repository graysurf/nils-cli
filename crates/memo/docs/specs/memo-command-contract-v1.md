# memo Command Contract v1

## Purpose

This document defines the command-level behavior contract for `memo` MVP commands:
`add`, `update`, `delete`, `list`, `search`, `report`, `fetch`, and `apply`.

It aligns with:

- `docs/runbooks/new-cli-crate-development-standard.md`
- `docs/specs/cli-service-json-contract-guideline-v1.md`

## Scope

- Human-readable output is the default mode.
- JSON output is opt-in via `--json` or `--format json`.
- Raw memo capture supports explicit mutation commands (`update`, `delete --hard`);
  agent enrichment remains a derived layer.

## Command Surface

```text
Usage:
  memo [--db <path>] [--format <text|json> | --json] <command> [options]

Commands:
  add <text>
  update <item_id> <text>
  delete <item_id> --hard
  list
  search <query>
  report <week|month>
  fetch
  apply
```

### Shared flags

- `--db <path>`: SQLite file path.
  Default: `$XDG_DATA_HOME/nils-cli/memo.db` or `~/.local/share/nils-cli/memo.db`.
- `--format <text|json>`: output mode selector. Default: `text`.
- `--json`: shorthand for `--format json`.

### `--json` and `--format json` rules

- `--json` and `--format json` are equivalent.
- `--format text` is equivalent to not setting JSON flags.
- If both are present, they must resolve to JSON mode.
  Example invalid usage: `--json --format text` (usage error).
- Machine-facing commands `fetch` and `apply` MUST support both `--json` and `--format json`.
- `list`, `search`, and `report` also support `--json` and `--format json` for automation parity.
- `add` supports JSON mode for script-driven capture acknowledgements.
- `update` and `delete` support JSON mode for script-driven maintenance workflows.

## stdout/stderr boundary

| Channel | Text mode | JSON mode |
| --- | --- | --- |
| `stdout` | primary command result (tables, lists, summaries) | exactly one JSON object (`result` or `results`, or `error` when `ok=false`) |
| `stderr` | warnings, validation/runtime errors, diagnostics | optional diagnostics only; never required for machine parsing |

Contract notes:

- In JSON mode, consumers must determine outcome from JSON `ok` plus process exit code, not by parsing prose.
- Help output (`--help`) is written to `stdout`.

## usage and exit code behavior

- `0`: command completed successfully.
- `64`: usage error (invalid flags, missing required arg, incompatible option combination).
- `65`: input data error (for example malformed `apply` payload).
- `1`: runtime/operational failure (I/O, DB open/query failure, unexpected internal failure).

Rules:

- Parsing/validation failures print brief diagnostic to `stderr` and return the documented exit code.
- In JSON mode, command-level failures still emit a JSON error envelope to `stdout`.

## JSON envelope baseline

For commands using JSON mode, response envelope follows
`docs/specs/cli-service-json-contract-guideline-v1.md`:

- required top-level keys: `schema_version`, `command`, `ok`
- success: use `result` (single) or `results` (collection)
- failure: `ok=false` with `error.code`, `error.message`, and optional `error.details`

Planned schema identifiers for v1:

- `add`: `memo.add.v1`
- `update`: `memo.update.v1`
- `delete`: `memo.delete.v1`
- `list`: `memo.list.v1`
- `search`: `memo.search.v1`
- `report`: `memo.report.v1`
- `fetch`: `memo.fetch.v1`
- `apply`: `memo.apply.v1`

## Phase 1 metadata taxonomy (additive)

For derivation-aware machine workflows (`apply` payload/result and any additive
JSON fields that expose derivation metadata), the taxonomy is:

- `content_type`: `url`, `json`, `yaml`, `xml`, `markdown`, `text`, `unknown`.
- `validation_status`: `valid`, `invalid`, `unknown`, `skipped`.
- `validation_errors[]`: structured objects with required `code`, required
  `message`, and optional `path`.

Rules:

- Metadata is derivation-layer data (Approach A), not raw capture mutation.
- In v1, these metadata fields are additive-only compatibility extensions.

## Command Semantics

### `add`

Capture one raw inbox record.

```text
memo add <text> [--source <label>] [--at <rfc3339>] [--json|--format json]
```

Behavior:

- Persists raw text as a new inbox item.
- Allocates `item_id` from a monotonic per-database sequence; hard delete does not recycle IDs.
- Sequence example: if `itm_00000001` is hard-deleted, the next `add` allocates
  `itm_00000002` or newer.
- By default, `created_at` is system-generated at write time.
- `--at` allows explicit RFC3339 timestamp input and stores the normalized UTC instant.

Text output (`stdout`):

- single-line acknowledgement including item id and created timestamp.

JSON output:

- `result` includes at least: `item_id`, `created_at`, `source`, `text`.

### `update`

Update one raw inbox record and reset downstream derived workflow state.

```text
memo update <item_id> <text> [--json|--format json]
```

Behavior:

- Replaces `raw_text` for one item.
- Clears active derivations and extension workflow anchors for that item.
- Item state returns to `pending` for future `fetch/apply` processing.

Text output (`stdout`):

- single-line acknowledgement including item id, update timestamp, and cleared counters.

JSON output:

- `result` includes at least:
  - `item_id`
  - `updated_at`
  - `text`
  - `state` (always `pending`)
  - `cleared_derivations`
  - `cleared_workflow_anchors`

### `delete`

Hard-delete one inbox record and all dependent data.

```text
memo delete <item_id> --hard [--json|--format json]
```

Behavior:

- Requires `--hard`; soft-delete is not supported in v1.
- Permanently removes raw row plus dependent derivations/search/workflow rows.
- Deleted item is no longer addressable in `list/search/fetch/report`.

Text output (`stdout`):

- single-line acknowledgement including item id, delete timestamp, and removed counters.

JSON output:

- `result` includes at least:
  - `item_id`
  - `deleted` (`true`)
  - `deleted_at`
  - `removed_derivations`
  - `removed_workflow_anchors`

### `list`

List captured items in deterministic order.

```text
memo list [--limit <n>] [--offset <n>] [--state <all|pending|enriched>] [--json|--format json]
```

Behavior:

- Default ordering: `created_at DESC`, then `item_id DESC` as tie-breaker.
- `--state pending` means items without active enrichment.
- `--state enriched` means items with active enrichment.

Text output (`stdout`):

- table/list with id, timestamp, state, short preview.
- when the returned row count reaches `--limit`, a footer names the active
  limit and suggests `--limit` / `--offset` for follow-up paging.

JSON output:

- `results[]` includes stable pagination/state fields plus additive metadata
  fields when available:
  - `content_type`
  - `validation_status`
- `pagination.truncated` is `true` when the returned row count reaches
  `--limit`.

### `search`

Search inbox and active derived fields.

```text
memo search <query> [--limit <n>] [--state <all|pending|enriched>]
                [--field <raw|derived|tags>[,<raw|derived|tags>...]]
                [--match <fts|prefix|contains>] [--json|--format json]
```

Behavior:

- Uses SQLite FTS-backed matching for raw capture and active enrichment text.
- `--field` narrows the FTS search surface; default is all fields (`raw,derived,tags`).
- `--field` accepts comma-separated multi-select (for example: `--field raw,tags`).
- `--match` controls query semantics:
  - `fts` (default): pass query as FTS syntax.
  - `prefix`: convert whitespace-separated terms to FTS prefix tokens (`term*`).
  - `contains`: perform case-insensitive substring contains matching.
- Ranking is deterministic for score ties (`created_at DESC`, `item_id DESC`).

Text output (`stdout`):

- ranked matches with score, id, timestamp, and preview.
- when the returned match count reaches `--limit`, a footer names the active
  limit and suggests increasing `--limit`.

JSON output:

- `results[]` includes score/match metadata and additive derivation metadata
  fields when available:
  - `content_type`
  - `validation_status`
- JSON `meta` includes `fields[]` reflecting the effective field scope.
- JSON `meta` includes `match` reflecting the effective match mode.
- JSON `meta.truncated` is `true` when the returned match count reaches
  `--limit`.

### `report`

Generate period summaries from capture + enrichment data.

```text
memo report <week|month> [--tz <iana-tz>] [--from <rfc3339>] [--to <rfc3339>] [--json|--format json]
```

Behavior:

- `week` and `month` are canonical report windows.
- `--tz` shifts canonical `week|month` window calculations to the provided IANA timezone.
- `--from` and `--to` must be provided together and use RFC3339 input.
- Precedence: explicit `--from/--to` range overrides canonical period window boundaries.
- Uses capture totals and enrichment-derived categories/tags when present.
- Works even if enrichment is absent (falls back to capture-only aggregates).

Text output (`stdout`):

- summary sections for counts, top categories/tags, and open pending items.

JSON output:

- `result` contains period metadata plus aggregate fields suitable for dashboards.
- Additive aggregate sections may include:
  - `top_content_types[]`
  - `validation_status_totals[]`

### `fetch`

Machine-facing read for agent enrichment jobs.

```text
memo fetch [--limit <n>] [--cursor <opaque>] [--state <pending>] [--json|--format json]
```

Behavior:

- Returns records eligible for enrichment (default state: pending).
- Cursor-based pagination for stable batch processing.
- Does not mutate data.

Text output (`stdout`):

- human summary for manual inspection (batch size and ids).

JSON output:

- `results[]` includes source fields required by enrichment workers.
- `results[]` may include additive metadata fields (`content_type`,
  `validation_status`) and are `null` when unavailable for pending rows.
- `result.next_cursor` (or equivalent) for continuation.

### `apply`

Machine-facing write-back for agent enrichment results.

```text
memo apply (--input <file> | --stdin) [--dry-run] [--json|--format json]
```

Behavior:

- Accepts structured enrichment payload generated from `fetch`.
- Writes derivations as new versions.
- Active derivation selection follows latest accepted version per item.
- `--dry-run` validates payload and reports changes without committing writes.
- When metadata is present, `content_type`, `validation_status`, and
  `validation_errors` are attached to derivation metadata, not raw rows.

Text output (`stdout`):

- apply summary: accepted, skipped, failed counts.

JSON output:

- `result` includes counts and per-item status entries.
- Per-item metadata may include additive `content_type`, `validation_status`,
  and `validation_errors[]` fields.
- invalid payload returns `ok=false` and exits with input/usage error code.

## End-to-end flow: capture -> maintenance -> agent enrichment -> report

1. Quick capture:
   - `memo add "buy 1tb ssd for mom"`
   - `memo add "book pediatric dentist appointment"`
2. Optional maintenance:
   - `memo update itm_00000001 "buy 2tb ssd for mom"`
   - `memo delete itm_00000002 --hard`
3. Agent pull (machine mode):
   - `memo fetch --json --limit 50 > inbox-batch.json`
4. Agent enrichment writes payload for apply:
   - Each record includes normalized fields (for example category, priority, due hints, confidence).
5. Apply enrichment:
   - `memo apply --format json --input enrichment-batch.json`
6. Human summary report:
   - `memo report week`
7. Service/dashboard report:
   - `memo report month --json`

Expected contract outcome:

- capture supports explicit correction/removal commands with deterministic cleanup;
- enrichment is versioned and replaceable;
- report reflects latest active enrichment with capture fallback.

## Non-goals for this contract version

- Cloud sync or multi-device conflict resolution.
- Embedding/vector semantic search.
- Background daemon mode.
