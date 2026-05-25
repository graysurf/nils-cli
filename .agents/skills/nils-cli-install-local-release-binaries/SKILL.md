---
name: nils-cli-install-local-release-binaries
description: Build release binaries and install them into ~/.local/nils-cli.
---

# Nils CLI Install Local Release Binaries

## Contract

Prereqs:

- Run inside the `nils-cli` git work tree (the script resolves the repo root via `git`).
- `cargo` and a Rust toolchain available on `PATH`.
- `install` available on `PATH`.

Inputs:

- Optional flags:
  - `--prefix PATH` (default: `~/.local/nils-cli`)
  - `--bin NAME` (repeatable)
  - `--skip-build`

Outputs:

- Builds the workspace in release mode (unless `--skip-build`).
- Installs selected binaries into the destination directory.
  - Default binaries: all workspace `bin` targets (auto-discovered via `scripts/workspace-bins.sh`)

Exit codes:

- `0`: install succeeded
- `1`: build/install failed
- `2`: usage error (invalid arguments) or missing prerequisites

## Scripts (only entrypoints)

- `scripts/install-local-release-binaries.sh`
