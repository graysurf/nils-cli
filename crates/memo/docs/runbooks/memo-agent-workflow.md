# memo Agent Workflow

## Purpose

This runbook defines a minimal capture -> fetch -> apply -> report loop for automation scripts.

## 1. Capture raw items

```bash
memo add "buy 1tb ssd for mom"
memo add "book pediatric dentist appointment"
memo add --at 2026-02-12T10:00:00+08:00 "backfilled note"
```

## 2. Fetch pending items for agents

```bash
memo fetch --json --limit 50 > inbox-batch.json
```

Optional maintenance before fetch:

```bash
memo update itm_00000001 "buy 2tb ssd for mom"
memo delete itm_00000002 --hard
```

Expected JSON shape:

- top-level: `schema_version`, `command`, `ok`, `results`
- `results[]`: `item_id`, `created_at`, `source`, `text`, `state`,
  `content_type`, `validation_status`
- optional `pagination`: `limit`, `returned`, `next_cursor`, `has_more`

When `pagination.has_more=true`, continue with:

```bash
memo fetch --json --limit 50 --cursor <next_cursor>
```

## 3. Apply agent derivations

Prepare `enrichment-batch.json`:

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
      "tags": ["shopping", "family"],
      "payload": {
        "source": "memo-agent"
      }
    }
  ]
}
```

Apply:

```bash
memo apply --json --input enrichment-batch.json
```

Notes:

- `derivation_hash` drives idempotency; same hash on same `item_id` becomes `skipped`.
- `content_type`, `validation_status`, and `validation_errors` are optional.
- When metadata fields are omitted, apply infers them from raw capture text and
  stores them in derivation metadata.
- `--dry-run` validates and returns predicted versions without writing rows.

## 4. Validate with search and report

```bash
memo search "ssd" --json
memo search "sharedterm" --field raw,tags --json
memo report week
memo report month --json
memo report week --tz Asia/Taipei
memo report month --from 2026-02-01T00:00:00Z --to 2026-02-29T23:59:59Z --json
```

## 5. Failure handling

- Invalid payload returns `ok=false` with `error.code=invalid-apply-payload`.
- Cursor mismatch returns `ok=false` with `error.code=invalid-cursor`.
- Invalid temporal arguments return `invalid-time`, `invalid-timezone`, or
  `invalid-time-range`.
- Per-item conflicts are reported inside `result.items[].error` with `code=apply-item-conflict`.
- In text mode, warnings are sent to `stderr`; `stdout` remains primary result output.

## 6. Fallback behavior on apply validation failures

When `apply` fails validation or conflict rates spike:

1. Pause automation writes:
   - stop all `memo apply` jobs.
2. Keep capture and read workflows active:
   - continue `memo add`, `memo list`, `memo search`, `memo report`.
3. Use dry-run diagnostics before re-enable:
   - `memo apply --json --dry-run --stdin < enrichment-batch.json`
4. Re-enable writes only after:
   - payloads pass validation,
   - contract tests pass,
   - and repository required checks are green.
