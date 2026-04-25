# nils-term

## Overview

Small terminal utilities shared across the workspace.

## Shared helper policy

### What belongs in `nils-term`

- Domain-neutral progress primitives that can be reused by multiple binaries.
- Structured APIs (`Progress`, `ProgressOptions`, `ProgressEnabled`) that callers can compose.
- Behavior that can be verified with deterministic unit/integration tests.

### What stays crate-local

- Product-specific progress copy, emoji/prefix style, and section wording.
- CLI-specific policy decisions about when progress should appear for a command.
- Exit-code mapping and command-level error/warning text.

## Progress

`nils-term` provides a minimal, RAII-friendly progress abstraction that is safe for
machine-readable stdout output:

- progress is drawn to **stderr** by default
- with `ProgressEnabled::Auto` (default), progress is enabled only when **stderr is a TTY**

### Determinate progress

```rust
use nils_term::progress::{Progress, ProgressOptions};

let total = 3_u64;
let progress = Progress::new(total, ProgressOptions::default().with_prefix("work "));

for i in 0..total {
    progress.set_message(format!("item {i}"));
    progress.inc(1);
}

progress.finish();
```

### Spinner progress

```rust
use nils_term::progress::{Progress, ProgressFinish, ProgressOptions};

let spinner = Progress::spinner(
    ProgressOptions::default()
        .with_prefix("fetch ")
        .with_finish(ProgressFinish::Clear),
);

spinner.set_message("loading");
spinner.tick();
spinner.finish_and_clear();
```

### Library guidance

Prefer accepting a `Progress` (or `ProgressOptions`) from the caller instead of reading env vars
inside library code. This keeps libraries deterministic and lets binaries decide whether to show
progress (e.g. interactive vs CI). `nils-term` itself reads no env vars (no `NILS_TERM_*`,
`GIT_SCOPE_PROGRESS`, etc.); TTY detection runs against the selected `ProgressDrawTarget`.

## Public API

Re-exported under `nils_term::progress`. See `src/progress.rs` for the source of truth.

Types:

- `Progress` — RAII handle around an `indicatif::ProgressBar` (no-op when disabled).
- `ProgressOptions` — builder-style configuration consumed by `Progress::new` / `Progress::spinner`.
- `ProgressEnabled` — `Auto` (default, gated on TTY for `Stderr` target), `On`, `Off`.
- `ProgressFinish` — `Leave` (default) or `Clear` on drop.
- `ProgressDrawTarget` — `Stderr` (default) or in-memory `Writer { buffer }` for tests.

`ProgressOptions` builders:

- `with_enabled`, `with_prefix`, `with_width`, `with_finish`, `with_draw_target`.

`ProgressDrawTarget` constructors:

- `stderr`, `to_writer`.

`Progress` constructors and methods:

- Constructors: `new` (determinate), `spinner`.
- Mutators: `set_position`, `inc`, `tick`, `set_message`.
- Terminators: `finish`, `finish_with_message`, `finish_and_clear`.
- Suspension: `suspend` (run a closure with the bar paused).

## Consumers

Workspace crates that depend on `nils-term` (see each crate's `Cargo.toml` and the `use nils_term::progress::...` import sites):

- `api-gql`, `api-grpc`, `api-rest`, `api-test`, `api-testing-core`, `api-websocket`
- `cli-template`
- `codex-cli`
- `fzf-cli`
- `git-lock`, `git-scope`, `git-summary`
- `image-processing`
- `plan-tooling`
- `semantic-commit`

To re-derive this list: `rg -nl '^nils-term\b|"nils-term"' crates/*/Cargo.toml` (excluding the crate itself).

## Migration guidance

When migrating crate-local progress code into `nils-term`:

1. Add/keep characterization tests before moving behavior.
2. Keep progress rendering to `stderr` so machine-readable `stdout` remains stable.
3. Keep CLI-local adapters for command wording and error/exit semantics.
4. Re-run command-contract tests for TTY, non-TTY, JSON, and `NO_COLOR` flows.

## Docs

- [Docs index](docs/README.md)
