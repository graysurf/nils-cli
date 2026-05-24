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
- `restore-backups` — restore a runtime home from a recorded backup snapshot.
- `purge-state` — purge runtime-managed state (use with caution).
- `pr-body render` — render standardized feature / bug PR or MR bodies for
  `forge-cli pr create` / `forge-cli pr deliver` flows.

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
