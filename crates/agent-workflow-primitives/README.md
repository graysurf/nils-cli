# agent-workflow-primitives

## Overview

`agent-workflow-primitives` is a multi-binary crate for deterministic, local-first agent workflow records. The binaries are designed for
skills and runbooks that need evidence, validation notes, or lightweight repository inspection without invoking AI providers or requiring
provider credentials.

## Package vs binary names

| Field | Value |
| ----- | ----- |
| Package name | `nils-agent-workflow-primitives` |
| Binary names | `agent-run`, `browser-session`, `canary-check`, `docs-impact`, `heuristic-inbox`, `model-cross-check`, `repo-retro`, `review-evidence`, `review-specialists`, `skill-usage`, `test-first-evidence` |

Each binary supports `--version` and `completion <bash|zsh>`.

## Binary map

| Binary | Primary purpose | Record/artifact written |
| ------ | --------------- | ----------------------- |
| `agent-run` | Run project commands through the selected project environment, using `direnv` for applicable `.envrc` / `.env` files. | stdout/stderr passthrough for `exec`; stdout/JSON only for `doctor` and `env` |
| `browser-session` | Record browser goals, steps, statuses, and evidence artifacts. | `browser-session.json` under `--out DIR` |
| `canary-check` | Run one local command and persist a redacted pass/fail result. | `canary-check.json` under `--out DIR` |
| `docs-impact` | Classify changed Git paths as docs or non-docs and suggest documentation review. | stdout/JSON only |
| `heuristic-inbox` | Manage curated HEURISTIC_SYSTEM inbox + operation-record case folders with redaction-enforced evidence ingestion, plus `deliver` for records-branch PR writeback. | `<inbox-dir>/<slug>/ENTRY.md` + automatic `agent-out` execution logs (`invocation.json`, `before.json`, `after.json`) for write ops |
| `model-cross-check` | Record primary/checker model observations without owning provider calls. | `model-cross-check.json` under `--out DIR` |
| `repo-retro` | Generate deterministic repo-local implementation retrospectives from local Git, HEURISTIC_SYSTEM records, and explicit JSONL inputs. | stdout by default; optional Markdown/raw JSON/index under `--history-dir DIR --write` |
| `review-evidence` | Record review findings and passing validation evidence. | `review-evidence.json` under `--out DIR` |
| `review-specialists` | Validate, merge, render, bundle, and scope specialist review findings without running reviewers or mutating providers. | stdout by default; optional bundle files under `--out-dir DIR` |
| `skill-usage` | Record skill invocation intent, linked records, validation, failures, outcome, and follow-up. | `skill-usage.record.json` under `--out DIR` |
| `test-first-evidence` | Record before-fix failing evidence, explicit waivers, and final validation. | `test-first-evidence.json` under `--out DIR` |

## Common command shape

Most record-oriented binaries use this flow:

1. `init --out DIR ...`
2. one or more `record-* --out DIR ...` commands
3. `verify --out DIR`
4. optional `show --out DIR --format json`

`canary-check` uses `run`, `verify`, and `show`. `docs-impact` uses `scan`.
`agent-run` uses `exec`, `doctor`, and `env`.

Examples:

```bash
docs-impact scan --repo . --include-untracked --format json
agent-run exec --cwd . -- cargo test
agent-run exec --cwd . --direnv require -- npm test
agent-run env --cwd . --format json
repo-retro report --repo . --days 7 --mode team --format json
repo-retro report --repo . --mode maintainer --format markdown
canary-check run --out /tmp/canary --name smoke --command "cargo test smoke"
browser-session init --out /tmp/browser --target http://localhost:3000 --goal "verify checkout flow"
review-evidence init --out /tmp/review --subject "PR #123"
review-specialists validate --input findings.jsonl --format json
review-specialists merge --input findings.jsonl --summary-out review.md
model-cross-check init --out /tmp/cross-check --prompt "review patch" --primary-model gpt-5.4 --checker-model gpt-5.5
skill-usage init --out /tmp/skill --skill tools/devex/review-evidence --intent "record review" --user-request-summary "review this PR"
test-first-evidence init --out /tmp/test-first --classification behavior-change --production-path src/lib.rs
```

## `agent-run` flow

`agent-run` is an environment normalizer for agent-executed project commands. It
is not a task runner and does not replace project scripts such as
`scripts/check.sh`, `cargo test`, `npm test`, or `uv run pytest`.

```bash
agent-run exec --cwd . -- cargo test
agent-run exec --cwd . --direnv require -- npm test
agent-run doctor --cwd . --format json
agent-run env --cwd . --format json
```

`--direnv auto` is the default. When no `.envrc` or `.env` applies, commands run
directly. When `.envrc` applies, `agent-run exec` uses `direnv exec`. When a
bare `.env` applies and `direnv status` does not report it as a loadable RC,
`agent-run` uses `direnv dotenv json` to parse values and runs the child with
those variables. If `direnv` is unavailable or the env file is blocked, `exec`
fails before running the child command. `--direnv off` bypasses direnv
intentionally; `--direnv require` fails when no env file applies.

`agent-run` never runs `direnv allow`, `direnv edit`, or any trust-mutating
command. A blocked `.envrc` or `.env` remains a user decision outside this
primitive.

Successful `agent-run exec` does not print wrapper prefaces. Child stdout,
stderr, and normal exit codes are preserved. Use `agent-run doctor` or
`agent-run env --format json` when a skill needs to explain whether validation
ran directly, through direnv, was bypassed, or was blocked. The v1 JSON status
reports mode, paths, availability, and decision only; it does not emit an
environment variable diff or environment values.

## `review-specialists` flow

`review-specialists` owns the deterministic helper work behind specialist
review workflows. It accepts reviewer-authored JSONL findings, validates the
schema, normalizes severity and confidence, deduplicates by stable fingerprint,
renders local Markdown/JSON profiles, writes small artifact bundles, and
classifies Git diffs for specialist routing. It does not run LLM prompts, spawn
subagents, post comments, open issues, merge PRs, or close issues.

```bash
review-specialists scope --base main --format json
review-specialists validate --input findings.jsonl --format json
review-specialists merge --input findings.jsonl --summary-out review.md
review-specialists render --profile issue-body --input findings.merged.json \
  --repo sympoies/nils-cli --ref HEAD --out issue.md
review-specialists bundle --input findings.jsonl --out-dir target/review-specialists/bundle \
  --profile issue-body
```

## `heuristic-inbox verify` redaction guardrail

`verify` scans both the case body (`ENTRY.md` / `RECORD.md`) and any
`evidence/` files using the same four redaction rules:

1. token-like patterns (Bearer / `sk-` / `api_key=` / `-----BEGIN ...`)
2. body / file byte size against the `--max-bytes` (default 64 KiB) limit
3. raw `skill-usage.record.v1` JSON shape (matches `"schema":"skill-usage.record.v1"`)
4. absolute `$HOME` paths (`/Users/...` or `/home/...`)

Findings on the body are surfaced under the `body_violations` array and a
`body warning:` line in `warnings`. By default they do **not** flip `ok` to
`false` so migrated cases that preserve absolute audit paths keep passing
`verify`. Use `--strict` to escalate body findings to `ok=false`; downstream
tooling can opt in once collaborators have rotated their cases.

`evidence/` files keep the existing strict behaviour: any violation fails
`verify` regardless of `--strict`.

## `heuristic-inbox deliver` flow

`deliver` ships the uncommitted retained-record changes under a Heuristic System
root as a records-branch PR, independently of the current branch or working
directory. It exists because the closeout writeback is deterministic mechanics
(worktree → stage → commit → push → PR) with no judgment, so it belongs in a
machine-checkable command rather than skill prose.

```bash
# Preview the plan (no fetch / worktree / commit / push):
heuristic-inbox deliver --root core/policies/heuristic-system --dry-run --format json

# Open the docs PR off origin/main on a dedicated records branch:
heuristic-inbox deliver --root core/policies/heuristic-system --format json
```

Mechanics:

1. Resolve the canonical repo (`git rev-parse --show-toplevel` from `--root` /
   cwd) and `git fetch origin <base>` — the records branch always forks from
   `origin/<base>`, never the current branch.
2. Create an isolated worktree off `origin/<base>` on a `<prefix>/<slug>` branch
   whose prefix matches `--kind` (e.g. `--kind docs` → `docs/...`), under the
   same managed path scheme as `git-cli worktree` so it is removable via
   `git-cli worktree remove <slug>`. The default date slug is auto-suffixed
   (`-2`, `-3`, ...) when an earlier local records branch or worktree already
   uses it; an explicit `--slug` collision fails with `records-target-exists`.
3. Copy only the changed files under the heuristic-system root into the
   worktree, `git add` that path, and **refuse** (`dirty-records-worktree`) if
   anything outside it is dirty.
4. Commit via `semantic-commit`, push the records branch, and open the PR via
   `forge-cli pr create --kind <kind>`.

Pass `--label <NAME>` (repeatable) to tag the records PR; each label is
forwarded verbatim to `forge-cli pr create --label` so the PR is identifiable by
taxonomy (e.g. a `heuristic-session-closeout` records PR carries
`--label workflow::heuristic-records`).

Output envelope (`cli.heuristic-inbox.deliver.v1`) carries `branch`, `pr_url`,
`committed_paths`, plus `worktree_path` for cleanup and (on `--dry-run`) the
ordered `plan`. The `git` / `semantic-commit` / `forge-cli` binaries are
overridable via `HEURISTIC_INBOX_GIT_BIN`, `HEURISTIC_INBOX_SEMANTIC_COMMIT_BIN`,
and `FORGE_CLI_BIN`.

## `skill-usage` flow

`skill-usage` is the broadest recorder in this crate. It links the rest of the evidence records back to one skill invocation.

`init` stamps an additive `producer` block (`tool` + `nils_cli_version`) into the
record so archived evidence always carries the producing nils-cli version,
independent of the host's current version-pin. The field is backward compatible:
records written before it existed deserialize with `producer` absent.

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

## `test-first-evidence` flow

`test-first-evidence` keeps the public binary contract that was previously
implemented by a standalone package. It records one JSON file under the caller's
artifact directory and preserves the schema versions
`test-first-evidence.record.v1` and `cli.test-first-evidence.*.v1`.

```bash
test-first-evidence init \
  --out "$AGENT_HOME/out/projects/acme__app/test-first" \
  --classification behavior-change \
  --production-path src/lib.rs
test-first-evidence record-failing \
  --out "$AGENT_HOME/out/projects/acme__app/test-first" \
  --command "cargo test bug_repro" \
  --exit-code 101 \
  --summary "bug reproduced before fix"
test-first-evidence record-final \
  --out "$AGENT_HOME/out/projects/acme__app/test-first" \
  --command "cargo test bug_repro" \
  --status pass
test-first-evidence verify --out "$AGENT_HOME/out/projects/acme__app/test-first" --format json
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
- `69`: required external tool or project environment unavailable

`repo-retro report --format json` uses the service envelope
`cli.repo-retro.report.v2` and returns a `repo-retro.report.v2` result. The v2
report adds a deterministic pre-digestion layer so process-doc churn cannot
dominate the derived insight: `git.churnByClass` (source / tests / productDocs /
processArtifacts / other, reconciling to the summary total), `git.archival`
(net-deleted files, the primary archival signal), and commit-frequency
`fileHotspots` entries carrying `class` and `netDeleted`. The analysis layer
reads that split instead of raw line churn and never nominates a net-deleted
file for review. Path classification uses built-in defaults overridable with
`--path-class-config <file.json>` (a `{ "<class>": ["<path-prefix>", ...] }`
map merged over the defaults). The default `markdown` output is intended for
direct review agendas and does not write files unless `--history-dir <dir>
--write` is supplied.

## Secret-safety boundary

The recorders redact common secret assignments and token-like values from command lines, summaries, paths, and previews before writing
records or printing JSON/text output. They do not read linked artifact contents.

## Docs

- [Docs index](docs/README.md)
- [Completion coverage matrix](../../docs/specs/completion-coverage-matrix-v1.md)
- [CLI service JSON contract guideline](../../docs/specs/cli-service-json-contract-guideline-v1.md)
- [New CLI crate development standard](../../docs/runbooks/new-cli-crate-development-standard.md)
- [Review specialists primitive runbook](../../docs/runbooks/review-specialists-primitive.md)
