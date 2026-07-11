# agent-runtime

## Overview

`nils-agent-runtime` is the Rust crate (in `crates/agent-runtime/`) that
ships the `agent-runtime` binary
for [`graysurf/agent-runtime-kit`](https://github.com/graysurf/agent-runtime-kit).
The binary owns deterministic runtime-kit tooling: render / install /
audit / doctor flows, runtime state maintenance, and workflow-oriented helpers
that should not live in provider wrappers such as `forge-cli`.

## Package vs binary name

| Field        | Value                  |
| ------------ | ---------------------- |
| Directory    | `crates/agent-runtime` |
| Package name | `nils-agent-runtime`   |
| Binary name  | `agent-runtime`        |

Use the package name (`-p nils-agent-runtime`) for cargo commands and the
binary name (`agent-runtime`) for installed-binary invocations.

## Subcommands

Core runtime-kit operations:

- `render` — render `core/` + `targets/<product>/` into `build/<product>/`;
  `--target home-prompt` renders only `AGENT_HOME.md`.
- `install` — activate rendered output against a product's runtime home.
- `uninstall` — remove installed renderer output from a product's runtime home.
- `doctor` — diagnose host setup, runtime roots, and required CLI floors.
- `audit-drift` — detect source-vs-rendered, rendered-vs-live, and unsafe drift.
- `gc-backups` — prune old backups under `<state_home>/backups/`.
- `list-skills` — enumerate the skills `install` would activate for a
  product, in deterministic text or JSON.
- `restore-backups` — restore a runtime home from a recorded backup snapshot.
- `purge-state` — purge runtime-managed state (use with caution).
- `pr-body render` — render standardized feature / bug PR or MR bodies for
  `forge-cli pr create` / `forge-cli pr deliver` flows.

Successful `install --apply` writes
`<state-home>/receipts/<product>.json`. The portable receipt contains only the
product, source revision/dirty status, normalized install-plan and managed-entry
digests, producer version, and timestamp; it excludes source/home paths,
remotes, accounts, and host details. Reinstalling unchanged content preserves
the plan and entry digests. Receipt parsing rejects fields outside this
allowlist so accidental path or account metadata cannot silently become part of
the accepted contract.

`doctor --class installed-runtime` is the focused acceptance gate for that
receipt. It blocks on a missing receipt from an older install, dirty or mismatched source,
plan/content drift, and live managed-target drift. Normal doctor mode reports a
missing receipt as a warning so older installs remain diagnosable until
they are reinstalled.

## list-skills

`agent-runtime list-skills` enumerates the skills that `agent-runtime install`
would activate for a given source-root + product. It is read-only and does
not mutate any live home; passing `--live-home` is accepted for parity with
`install` but is not required.

```bash
# Diff-friendly text output (one skill per line, tab-separated).
agent-runtime list-skills --source-root <kit> --product codex

# Machine-readable JSON. `schema` pins the v1 contract.
agent-runtime list-skills --source-root <kit> --product claude --format json
```

JSON v1 schema (`cli.agent-runtime.list-skills.v1`):

```json
{
  "schema": "cli.agent-runtime.list-skills.v1",
  "product": "codex",
  "source_root": "/abs/path/agent-runtime-kit",
  "live_home": null,
  "skills": [
    {
      "id": "reporting.daily-brief",
      "source": "build/codex/plugins/reporting/skills/daily-brief",
      "destination": "skills/reporting/daily-brief",
      "link_mode": "directory",
      "discoverable": true,
      "invocation": {
        "role": "workflow",
        "intents": ["daily-brief"],
        "example_request": "Prepare my daily brief",
        "admission_rationale": "Produces a direct user-requested information brief."
      },
      "exposure": {
        "profile": "default",
        "replacement": null,
        "retire_after": null
      },
      "pending_disposition": false,
      "warnings": []
    }
  ]
}
```

- `skills` is sorted by `id` for deterministic diffs.
- `discoverable` is `true`/`false` for `--product codex`; omitted for other
  products.
- `invocation` and `exposure` are populated from a skills manifest v2 entry.
  They are `null` for v1 entries and for v2 entries explicitly listed under
  `migration.pending_disposition`.
- `pending_disposition` is always present. `true` means the skill remains
  honestly installed and discoverable while the manifest owner completes its
  migration review; it is not a hidden/internal exposure class.
- `warnings` mirrors `doctor::skill_surface` warning codes (currently
  `codex.active-skill.file-symlink`) and is always present in JSON output.
  Pass `--include-warnings` to surface them inline in text output too.

Pipe the JSON to `jq -r '.skills[].id' | sort` to recover the canonical
`<domain>.<skill>` list used by sandbox install rehearsal pins under
`agent-runtime-kit/tests/sandbox/<product>/expected-skills.txt`.

### Skills manifest compatibility

`agent-runtime` accepts `manifests/skills.yaml` schema versions 1 and 2 while
other runtime manifest families remain on version 1. Version 2 adds typed
invocation, exposure, and pending-disposition metadata. Active retained entries
support only honest `default` exposure in this release; unsupported `opt-in`,
permanent `internal`, and `advanced` plus default combinations fail closed.
Compatibility entries must declare both a canonical replacement and a
time-bounded `retire_after` value.

## PR body rendering

`agent-runtime pr-body render` takes agent-authored section files and renders
the fixed Markdown scaffolding. It intentionally does not infer PR narrative
from git history or diffs.

Feature body example:

```bash
agent-runtime pr-body render \
  --kind feature \
  --summary-file summary.md \
  --changes-file changes.md \
  --test-first-file test-first.md \
  --test-plan-file test-plan.md \
  --risk-file risk.md \
  --out pr-body.md
```

Bug body example:

```bash
agent-runtime pr-body render \
  --kind bug \
  --summary-file summary.md \
  --problem-file problem.md \
  --reproduction-file reproduction.md \
  --issues-file issues.md \
  --fix-approach-file fix-approach.md \
  --test-first-file test-first.md \
  --test-plan-file test-plan.md \
  --risk-file risk.md \
  --out pr-body.md
```

The rendered body always uses `## Summary` and `## Test plan` so it satisfies
the body-section gate enforced by `forge-cli`.

`--issues-file` is required for `bug` (rendered as `## Issues Found`) and
optional for every other kind, where it renders an `## Issues` references
section (for example `Refs #804`) right after `## Summary`. The remaining
kind-specific files are rejected when passed with a non-owning kind
(`--changes-file` is feature-only; `--problem-file`, `--reproduction-file`,
and `--fix-approach-file` are bug-only) instead of being silently dropped.

## Docs

- [Docs index](docs/README.md)
- [Determinism contract](docs/determinism.md)
