# agent-workflow-primitives

## Overview

`agent-workflow-primitives` is a multi-binary crate for deterministic, local-first agent workflow records. The binaries are designed for
skills and runbooks that need evidence, validation notes, or lightweight repository inspection without invoking AI providers or requiring
provider credentials.

## Package vs binary names

| Field | Value |
| ----- | ----- |
| Package name | `nils-agent-workflow-primitives` |
| Binary names | `browser-session`, `canary-check`, `docs-impact`, `heuristic-inbox`, `model-cross-check`, `repo-retro`, `review-evidence`, `skill-usage` |

Each binary supports `--version` and `completion <bash|zsh>`.

## Binary map

| Binary | Primary purpose | Record/artifact written |
| ------ | --------------- | ----------------------- |
| `browser-session` | Record browser goals, steps, statuses, and evidence artifacts. | `browser-session.json` under `--out DIR` |
| `canary-check` | Run one local command and persist a redacted pass/fail result. | `canary-check.json` under `--out DIR` |
| `docs-impact` | Classify changed Git paths as docs or non-docs and suggest documentation review. | stdout/JSON only |
| `heuristic-inbox` | Manage curated HEURISTIC_SYSTEM inbox + operation-record case folders with redaction-enforced evidence ingestion. | `<inbox-dir>/<slug>/ENTRY.md` + automatic `agent-out` execution logs (`invocation.json`, `before.json`, `after.json`) for write ops |
| `model-cross-check` | Record primary/checker model observations without owning provider calls. | `model-cross-check.json` under `--out DIR` |
| `repo-retro` | Generate deterministic repo-local implementation retrospectives from local Git, HEURISTIC_SYSTEM records, and explicit JSONL inputs. | stdout by default; optional Markdown/raw JSON/index under `--history-dir DIR --write` |
| `review-evidence` | Record review findings and passing validation evidence. | `review-evidence.json` under `--out DIR` |
| `skill-usage` | Record skill invocation intent, linked records, validation, failures, outcome, and follow-up. | `skill-usage.record.json` under `--out DIR` |

## Common command shape

Most record-oriented binaries use this flow:

1. `init --out DIR ...`
2. one or more `record-* --out DIR ...` commands
3. `verify --out DIR`
4. optional `show --out DIR --format json`

`canary-check` uses `run`, `verify`, and `show`. `docs-impact` uses `scan`.

Examples:

```bash
docs-impact scan --repo . --include-untracked --format json
repo-retro report --repo . --days 7 --mode team --format json
repo-retro report --repo . --mode maintainer --format markdown
canary-check run --out /tmp/canary --name smoke --command "cargo test smoke"
browser-session init --out /tmp/browser --target http://localhost:3000 --goal "verify checkout flow"
review-evidence init --out /tmp/review --subject "PR #123"
model-cross-check init --out /tmp/cross-check --prompt "review patch" --primary-model gpt-5.4 --checker-model gpt-5.5
skill-usage init --out /tmp/skill --skill tools/devex/review-evidence --intent "record review" --user-request-summary "review this PR"
```

## `skill-usage` flow

`skill-usage` is the broadest recorder in this crate. It links the rest of the evidence records back to one skill invocation.

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

## Output contract

Human-readable text is the default. Service-consumed commands support `--format json` and return a versioned envelope:

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

Exit codes:

- `0`: success
- `1`: runtime failure or incomplete evidence from `verify`
- `64`: usage/configuration error

`repo-retro report --format json` uses the service envelope
`cli.repo-retro.report.v1` and returns a `repo-retro.report.v1` result. The
default `markdown` output is intended for direct review agendas and does not
write files unless `--history-dir <dir> --write` is supplied.

## Secret-safety boundary

The recorders redact common secret assignments and token-like values from command lines, summaries, paths, and previews before writing
records or printing JSON/text output. They do not read linked artifact contents.

## Docs

- [Docs index](docs/README.md)
- [Completion coverage matrix](../../docs/specs/completion-coverage-matrix-v1.md)
- [CLI service JSON contract guideline](../../docs/specs/cli-service-json-contract-guideline-v1.md)
- [New CLI crate development standard](../../docs/runbooks/new-cli-crate-development-standard.md)
