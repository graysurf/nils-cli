# agent workflow primitives

This crate ships small, deterministic CLIs used by agent-kit skills and runbooks.
The binaries are intentionally local-first: they create structured evidence,
run safe local checks, or inspect the current repository without requiring
provider credentials.

## Binaries

- `docs-impact`: scan a Git worktree for changed docs and non-docs paths.
- `canary-check`: run one local canary command and persist a redacted result.
- `review-evidence`: record review findings and validation status.
- `browser-session`: record browser-session goals, steps, and evidence artifacts.
- `model-cross-check`: record primary/checker model observations without owning
  provider invocation.
- `skill-usage`: record skill invocation intent, linked evidence, validation,
  outcome, and failure handling.

Each binary supports `--version` and `completion <bash|zsh>`.

## skill-usage

`skill-usage` writes `skill-usage.record.json` under `--out DIR`.

```bash
skill-usage init --out <dir> --skill <skill-path> \
  --intent <intent> --user-request-summary <summary>
skill-usage link-record --out <dir> --type review-evidence --path review-evidence.json
skill-usage record-failure --out <dir> --phase validation \
  --classification project-state --symptom <text> --diagnosis <text> \
  --handling <text> --result fixed
skill-usage record-validation --out <dir> --command <command> \
  --status pass --summary <summary>
skill-usage record-outcome --out <dir> --status pass --summary <summary>
skill-usage verify --out <dir> --format json
skill-usage show --out <dir> --format json
```

## JSON contract

Service-consumed commands support `--format json` and return a versioned
envelope:

```json
{
  "schema_version": "cli.<binary>.<command>.v1",
  "command": "<binary> <command>",
  "ok": true,
  "result": {}
}
```

Errors use the same envelope with `ok=false` and an `error` object containing
`code`, `message`, and optional `details`.
