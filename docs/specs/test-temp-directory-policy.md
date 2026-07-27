# Test temp-directory policy

How tests in this workspace own temporary directories, and why. Written after a
leak reached roughly 280 GB under `/tmp` without a single test failing.

## Why leaks were invisible

`tempfile::TempDir`'s `Drop` calls `remove_dir_all` and discards the result. Every
failure mode below therefore leaves the directory on disk and reports nothing —
no failing test, no CI warning, no log line. Silence is why it grew unnoticed.

`cargo nextest` runs one process per test. Any per-process leak is a per-test
leak, so a shape that leaked one directory per test binary under `cargo test`
leaks hundreds under nextest. Migrating runners changed the blast radius by two
orders of magnitude without changing a line of test code.

## The three leak classes

| Class | Mechanism | Caught by |
| --- | --- | --- |
| 1 | Cleanup never runs: the handle is owned by a `static`, or disarmed with `keep()` / `into_path()` / `mem::forget` | `scripts/ci/tempdir-leak-audit.sh` |
| 2 | Cleanup runs and fails: a fixture left read-only, a racing writer | `ScopedTempDir` reports instead of swallowing |
| 3 | Cleanup succeeds, then a detached child process recreates the path | Nothing static; see the rule below |

Class 1 is mechanical, so it is a CI gate. Class 3 is not detectable by
inspection — the fix is a test-authoring rule.

## Rules

1. **Never give a temp-dir handle to a `static`.** Rust does not drop statics, so
   its destructor can never run. `OnceLock`, `LazyLock`, and `Lazy` all count.
   When a process-scoped directory is genuinely needed, name it after the process
   and sweep stale siblings on startup — see
   `crates/plan-issue/tests/integration/common.rs`.
2. **Do not disarm cleanup.** `keep()` and `into_path()` hand back a bare path and
   cancel removal. Return the handle alongside the path and let the caller hold
   it for the test's duration.
3. **If a test triggers a detached child, wait for that child to finish before the
   fixture is dropped** — and wait on the child's *last* write, not the first
   observable one. `crates/codex-cli/tests/integration/prompt_segment_refresh.rs`
   waits for the refresh lock to be released rather than for the cache file,
   because the child writes its marker and releases its lock after the cache.
   Waiting on an intermediate artifact returns while writes are still pending,
   and those writes recreate the directory after teardown.
4. **Prefer `nils_test_support::tempdir::ScopedTempDir` in new tests.** It uses
   `TempDir::close`, which surfaces the cleanup error that plain `Drop` discards,
   and turns a leak into a test failure.

Raw `tempfile::TempDir` is not banned. It is correct for the common case, and
classes 2 and 3 have nothing to do with using it directly, so a blanket ban would
target the wrong thing. The workspace has roughly 3,200 temp-dir creation sites
across 41 crates; the rules above are enforced on new code by the audit rather
than by rewriting all of them.

## Do not redirect `TMPDIR` into the build tree

This looks like the obvious containment measure and it does not work here. It was
measured, so the result is recorded rather than re-derived:

- Cargo's `[env]` table in `.cargo/config.toml` **does** set `TMPDIR` for test
  processes (`std::env::temp_dir()` resolved to `target/tmp`), and it covers
  ad-hoc `cargo test` / `cargo nextest run` as well as the gate. Nextest's own
  `[env]` table does **not** take effect for `TMPDIR` — the leak still landed in
  `/tmp`.
- But `target/tmp` is inside a git work tree, and roughly 130 tests across ~25
  crates assert the opposite of that. Every `*_outside_git_repo`,
  `*_not_in_repo`, `not_a_repo`, and `create_outside_git_repository_*` test
  builds a fixture in a temp dir *because* a temp dir is not version-controlled.
  `.gitignore` does not help: repo discovery walks up to `.git` regardless.
- Worse, tests that resolve a repo root from a temp fixture then found the real
  workspace and wrote into it — a `cargo nextest run --workspace` left
  `docs/plans/test-plan.md`, a `docs/plans/*-test/` folder, and `out/` behind in
  the checkout.

Any containment measure therefore has to keep temp dirs **outside** every git
work tree, which rules out `target/`. Leaving `$TMPDIR` alone and sweeping the
system temp directory at the host level (a `tmpfiles.d` rule for `/tmp/.tmp*`) is
outside this repository's scope but is the one option that does not fight the test
suite.

## Enforcement

- `scripts/ci/tempdir-leak-audit.sh` fails the build on class 1. Escape hatch:
  `tempdir-leak-audit: allow` in a comment on the offending line or the line
  above, with a reason.
- `scripts/ci/tests/tempdir-leak-audit.test.sh` covers the audit itself, including
  the escape hatch.
- Both run from `scripts/ci/nils-cli-local-fast.sh`, so the local gate and CI
  agree.
