# agent-runtime-cli

## Overview

`agent-runtime-cli` is the Rust crate that ships the `agent-runtime` binary
for [`graysurf/agent-runtime-kit`](https://github.com/graysurf/agent-runtime-kit).
This Plan 01 release is a **stub**: every subcommand prints
`agent-runtime <subcommand>: not implemented` to stderr and exits `1`.
The crate exists so the rest of the install ladder — Homebrew tap,
`scripts/setup.sh`, `required_clis` declarations — can be wired up against
a real binary before Plan 02 lands the actual render / audit / install
bodies.

## Package vs binary name

| Field        | Value               |
| ------------ | ------------------- |
| Package name | `agent-runtime-cli` |
| Binary name  | `agent-runtime`     |

Use the package name (`-p agent-runtime-cli`) for cargo commands and the
binary name (`agent-runtime`) for installed-binary invocations.

## Subcommands (Plan 01 stub — all exit 1)

Enumeration is pinned by
[Resolved Decision #2 in `docs/source/inventory-target-architecture.md`](https://github.com/graysurf/agent-runtime-kit/blob/main/docs/source/inventory-target-architecture.md)
of the consuming repo:

- `render` — render `core/` + `targets/<product>/` into `build/<product>/`.
- `install` — activate rendered output against a product's runtime home.
- `uninstall` — remove installed renderer output from a product's runtime home.
- `doctor` — diagnose host setup, runtime roots, and required CLI floors.
- `audit-drift` — detect source-vs-rendered, rendered-vs-live, and unsafe drift.
- `gc-backups` — prune old backups under `<state_home>/backups/`.
- `restore-backups` — restore a runtime home from a recorded backup snapshot.
- `purge-state` — purge runtime-managed state (use with caution).

## Roadmap

- Plan 02 (`02-nils-cli-render-and-drift-audit`): implement `render` +
  `audit-drift` bodies; cut a `0.1.0` release and re-pin the Homebrew
  formula.
- Plan 04 (`04-install-and-bootstrap`): implement `install` / `uninstall`
  / `doctor` / `gc-backups` / `restore-backups` / `purge-state` bodies.

Track open work and discussion in
[graysurf/agent-runtime-kit Issue #1](https://github.com/graysurf/agent-runtime-kit/issues/1).
