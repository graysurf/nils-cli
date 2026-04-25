# cli-template

## Overview

`cli-template` is the minimal reference CLI for the `nils-cli` workspace. It demonstrates the
baseline scaffold: clap-based argument parsing with `#[command(version)]`, `tracing` + `EnvFilter`
log initialization, and optional progress output via `nils-term` (rendered to `stderr` so `stdout`
stays machine-readable). It exists as a smoke test and as a copy-paste starting point for new CLI
crates.

## Exemplar role

This crate is the canonical live exemplar referenced by
[`docs/runbooks/new-cli-crate-development-standard.md`](../../docs/runbooks/new-cli-crate-development-standard.md).
The runbook points at `crates/cli-template/Cargo.toml` for current workspace package metadata
(version, edition, license, description, repository, `[[bin]]` shape) instead of hard-coding it.
Keep this README and `Cargo.toml` aligned with that runbook when scaffolding rules change. Do not
hard-code the crate version in this README; treat `Cargo.toml` as the source of truth.

## Package vs binary name

| Field        | Value               |
| ------------ | ------------------- |
| Package name | `nils-cli-template` |
| Binary name  | `cli-template`      |

Use the package name (`-p nils-cli-template`) for cargo commands and the binary name
(`cli-template`) for `--help` / installed-binary invocations.

## Usage

```text
Template CLI for nils-cli workspace

Usage: cli-template [OPTIONS] [COMMAND]

Commands:
  hello          Print a greeting to stdout
  progress-demo  Render a short progress demo (progress on stderr, stdout stays clean)
  help           Print this message or the help of the given subcommand(s)

Options:
      --log-level <LOG_LEVEL>  Log level (e.g. trace, debug, info, warn, error) [default: info]
  -h, --help                   Print help
  -V, --version                Print version
```

Re-derive this block with:

```bash
cargo run -p nils-cli-template -- --help
```

## Commands

- `hello [NAME]`: Print a greeting to `stdout`. `NAME` defaults to `world`. Implemented via
  `nils_common::greeting` to exercise the shared-helper boundary.
- `progress-demo`: Render a short progress demo using `nils_term::progress::Progress`. Progress
  ticks render on `stderr`; `stdout` only receives the final `done` line so the command stays
  pipe-safe.

## Flags

- `--log-level <LOG_LEVEL>`: `tracing-subscriber` `EnvFilter` directive (`trace|debug|info|warn|error`).
  Default `info`. Invalid values fall back to `RUST_LOG` then to `info`; the command still runs.
- `-h, --help`: Print top-level or per-subcommand help.
- `-V, --version`: Print the crate version sourced from `Cargo.toml`.

## Output contract

- `stdout`: greeting line (`hello`) or `done` (`progress-demo`).
- `stderr`: tracing logs and `nils-term` progress output.
- Exit code: `0` for the documented commands; clap's standard `2` for argument errors.

This crate is intentionally excluded from the workspace JSON contract surface — see
[`docs/specs/completion-coverage-matrix-v1.md`](../../docs/specs/completion-coverage-matrix-v1.md)
for the explicit exclusion entry.

## Dependencies

Workspace shared crates this template uses (kept intentionally small):

- [`nils-common`](../nils-common/README.md): `greeting` smoke helper.
- [`nils-term`](../nils-term/README.md): `progress::Progress` / `ProgressOptions` / `ProgressFinish`.

## Docs

- [Docs index](docs/README.md)
- [`new-cli-crate-development-standard.md`](../../docs/runbooks/new-cli-crate-development-standard.md)
  (uses this crate as the live scaffold exemplar)
