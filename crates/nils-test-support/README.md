# nils-test-support

## Overview

`nils-test-support` is a test-only helper crate shared across this workspace.

It provides small utilities to keep tests deterministic when they need to manipulate global state or stub external commands.

## Shared helper policy

Runtime shared-crate ownership boundaries are tracked in
[`docs/specs/workspace-shared-crate-boundary-v1.md`](../../docs/specs/workspace-shared-crate-boundary-v1.md) so test-surface extractions
stay aligned with production-lane decisions.
Stale-test cleanup sequencing is frozen in
[`docs/specs/workspace-test-cleanup-lane-matrix-v1.md`](../../docs/specs/workspace-test-cleanup-lane-matrix-v1.md).

### What belongs in `nils-test-support`

- Test-only utilities reused by multiple crates (guards, git helpers, command wrappers, stubs).
- Deterministic helpers that reduce flakiness and remove local test boilerplate.
- APIs that keep tests explicit while avoiding duplicated harness logic.

### What stays crate-local

- Test assertions specific to one CLI's output/contract expectations.
- Product-specific fixture semantics that are not reusable elsewhere.
- Command-specific golden text snapshots and local approval-test policy.

## Utilities

- Global guards
  - `GlobalStateLock`: serialize tests that mutate process-global state (env, cwd, PATH, etc.)
  - `EnvGuard`, `CwdGuard`: RAII guards for temporarily setting env vars / current directory
- FS helpers
  - `fs`: write text/bytes/json/executables while ensuring parent dirs exist
- Command runners
  - `cmd`: run binaries with captured output (`CmdOutput`) and flexible options (`CmdOptions`), including resolved workspace-binary helpers
    (`run_resolved*`)
  - `CmdOptions::with_env_remove_many`: remove multiple env vars in one call for deterministic harness setup
  - `CmdOptions::without_ambient_managed_session_env`: drop the `cmd::MANAGED_SESSION_ENV` set a managed
    `agent-session` pane pins, so a suite run inside one does not inherit overrides that outrank `PATH`
    and the fixture layout. Removals are applied before values, so a later `with_env` for the same key
    still reaches the child
  - `cmd::path_with_prepend_excluding_program`: construct a PATH that prepends stubs while filtering one real binary
- Workspace binaries
  - `bin`: `resolve` finds `CARGO_BIN_EXE_*` or falls back to `target/<profile>/<name>`, panicking when neither
    yields a path; `resolve_optional` returns `None` instead
  - `bin::sibling_or_skip`: for a binary **another package** owns. Cargo sets `CARGO_BIN_EXE_*` only for the
    package under test — a `dev-dependencies` entry neither exports it nor builds the dependency's binary — so a
    package-scoped run may find nothing, or find an artifact from an earlier release. Returns `None` with a
    stderr reason naming the build command when nothing was built, and **panics** when an artifact was selected
    but does not belong to this build, since that is an operator error rather than a property of the run.
    `NILS_TEST_REQUIRE_SIBLING_BINS=1` makes the absent case fatal too; CI sets it. The gate detects
    cross-release artifacts only — a same-release artifact built from an older commit is accepted
- Git helpers
  - `git`: init temp repos (`InitRepoOptions`), run git commands, and commit files
- Stubbing external tools
  - `StubBinDir`, `write_exe`, `prepend_path`: create a temp bin dir and put it at the front of `PATH`
  - `stubs`: ready-made stub scripts for common external tools (e.g. `fzf`, `bat`, `tree`, `file`, ImageMagick/WebP/JPEG)
- Fixtures
  - `fixtures`: REST/GraphQL setup fixtures + suite manifest helpers
- Loopback HTTP server
  - `http`: in-process loopback servers (`LoopbackServer`, `TestServer`) that record requests

## Example

```rust
use nils_test_support::{prepend_path, EnvGuard, GlobalStateLock, StubBinDir};

let lock = GlobalStateLock::new();
let stub_dir = StubBinDir::new();

let _path = prepend_path(&lock, stub_dir.path());
let _env = EnvGuard::set(&lock, "EXAMPLE", "1");
```

## Migration guidance

When migrating existing crate-local test helpers:

1. Move only reusable primitives; keep command-specific assertions local.
2. Prefer `GlobalStateLock`, `EnvGuard`, and `CwdGuard` for global-state safety.
3. Replace manual `PATH`/stub setup with `StubBinDir`, `prepend_path`, and `stubs`.
4. Re-run affected crate tests and keep flaky-risk notes up to date.

## Docs

- [Docs index](docs/README.md)
