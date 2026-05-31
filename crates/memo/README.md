# memo

## Overview

`memo` is a capture-first memo CLI with an agent enrichment loop.
Default output is human-readable text. JSON is explicit and intended for service/agent callers.
Storage bootstrap is consolidated into one init schema (`schema_v1.sql`); upgrade
compatibility for pre-consolidation databases is not guaranteed after this consolidation.

## Usage

```text
Usage:
  memo [--db <path>] [--format <text|json> | --json] <command> [options]

Commands:
  add <text> [--at <rfc3339>]                Capture a raw memo entry
  update <item_id> <text>                    Update a memo and reset downstream derived data
  delete <item_id> --hard                    Hard-delete a memo and all dependent data
  list [--limit <n>] [--offset <n>]         List entries (default: newest first)
  search <query> [--limit <n>]              Search raw/derived/tags text
         [--field <raw|derived|tags>[,...]]
         [--match <fts|prefix|contains>]
  report <week|month> [--tz <iana-tz>]      Build period summaries
         [--from <rfc3339>] [--to <rfc3339>]
  fetch [--limit <n>] [--cursor <opaque>]   Pull records for enrichment workers
  apply (--input <file> | --stdin)          Apply enrichment payloads
```

## Commands

- `add`: append one raw capture record.
- `update`: replace one raw memo text and clear derivations/workflow extension data for that item.
- `delete --hard`: permanently delete one item and all dependent rows (derivations/search/workflow anchors).
- `item_id` allocation is monotonic per database and IDs are not reused after hard delete.
- Example: if `itm_00000001` is hard-deleted, next `add` will allocate `itm_00000002` or newer.
- `add --at`: optional explicit capture time (RFC3339). Without `--at`, system time is used.
- `list`: show records with deterministic ordering and optional state filters.
- `search`: run keyword/prefix search across capture and active enrichment.
- `search --field`: optional field scope, supports multi-select (example: `--field raw,tags`).
- `search --match`: matching mode (`fts`, `prefix`, `contains`), default `fts`.
- `report`: render weekly/monthly summaries with capture fallback when enrichment is missing.
- `report --from/--to`: optional explicit range (RFC3339). Use both together.
- `fetch`: machine-facing pull for pending enrichment work.
- `apply`: machine-facing write-back for normalized enrichment results.

## JSON

- Text mode is the default for all commands.
- `--json` is shorthand for `--format json`.
- `fetch` and `apply` are machine-facing commands and support both `--json` and `--format json`.
- `list`, `search`, and `report` also support JSON mode for automation and dashboards.
- `list`/`search`/`fetch` may include additive metadata fields:
  `content_type`, `validation_status`; `list` and `search` also expose
  `truncated` pagination metadata.
- `report` may include additive metadata aggregates:
  `top_content_types`, `validation_status_totals`.
- In JSON mode, parse `stdout` only. `stderr` is diagnostic-only and not part of the data contract.
- Exit code policy: `0` success, `64` usage error, `65` input data error, `1` runtime failure.

## Examples

### Capture quickly

```bash
memo add "buy 1tb ssd for mom"
memo add "book pediatric dentist appointment"
memo add --at 2026-02-12T10:00:00+08:00 "backfilled note"
memo update itm_00000001 "buy 2tb ssd for mom"
memo delete itm_00000002 --hard
```

### Agent enrichment loop

```bash
memo fetch --json --limit 50 > inbox-batch.json
memo apply --format json --input enrichment-batch.json
```

Example `enrichment-batch.json` payload:

```json
{
  "agent_run_id": "agent-run-20260212",
  "items": [
    {
      "item_id": "itm_00000001",
      "derivation_hash": "hash-itm-00000001-v1",
      "summary": "buy ssd for mom",
      "category": "shopping",
      "normalized_text": "buy 1tb ssd for mom",
      "confidence": 0.93,
      "content_type": "text",
      "validation_status": "valid",
      "tags": ["family", "shopping"],
      "payload": {
        "source": "memo-agent",
        "notes": "priority this week"
      }
    }
  ]
}
```

Pagination note:

```bash
memo fetch --json --limit 20 --cursor itm_00000042
```

### Human and machine reports

```bash
memo report week
memo report month --json
memo report week --tz Asia/Taipei
memo report month --from 2026-02-01T00:00:00Z --to 2026-02-29T23:59:59Z --json
```

## Runbooks

- Agent workflow: `crates/memo/docs/runbooks/memo-agent-workflow.md`

## Specs

- Command contract: `crates/memo/docs/specs/memo-command-contract-v1.md`
- JSON contract: `crates/memo/docs/specs/memo-json-contract-v1.md`
- Storage schema: `crates/memo/docs/specs/memo-storage-schema-v1.md`
- Workflow extension contract: `crates/memo/docs/specs/memo-workflow-extension-contract-v1.md`
- Release policy: `crates/memo/docs/specs/memo-release-policy.md`

## Docs

- [Docs index](docs/README.md)
