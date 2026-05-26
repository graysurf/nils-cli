# agent-runtime-cli

## Overview

`agent-runtime-cli` is the Rust crate that ships the `agent-runtime` binary
for [`graysurf/agent-runtime-kit`](https://github.com/graysurf/agent-runtime-kit).
The binary owns deterministic runtime-kit tooling: render / install /
audit / doctor flows, runtime state maintenance, and workflow-oriented helpers
that should not live in provider wrappers such as `forge-cli`.

## Package vs binary name

| Field        | Value               |
| ------------ | ------------------- |
| Package name | `agent-runtime-cli` |
| Binary name  | `agent-runtime`     |

Use the package name (`-p agent-runtime-cli`) for cargo commands and the
binary name (`agent-runtime`) for installed-binary invocations.

## Subcommands

Core runtime-kit operations:

- `render` — render `core/` + `targets/<product>/` into `build/<product>/`.
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
      "warnings": []
    }
  ]
}
```

- `skills` is sorted by `id` for deterministic diffs.
- `discoverable` is `true`/`false` for `--product codex`; omitted for other
  products.
- `warnings` mirrors `doctor::skill_surface` warning codes (currently
  `codex.active-skill.file-symlink`) and is always present in JSON output.
  Pass `--include-warnings` to surface them inline in text output too.

Pipe the JSON to `jq -r '.skills[].id' | sort` to recover the canonical
`<domain>.<skill>` list used by sandbox install rehearsal pins under
`agent-runtime-kit/tests/sandbox/<product>/expected-skills.txt`.

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

## Docs

- [Docs index](docs/README.md)
- [Determinism contract](docs/determinism.md)
