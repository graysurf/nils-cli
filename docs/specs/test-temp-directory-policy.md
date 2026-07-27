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

## The four leak classes

| Class | Mechanism | Caught by |
| --- | --- | --- |
| 1 | Cleanup never runs: the handle is owned by a `static`, or disarmed with `keep()` / `into_path()` / `mem::forget` | `scripts/ci/tempdir-leak-audit.sh` |
| 2 | Cleanup runs and fails: a fixture left read-only, a racing writer | `ScopedTempDir` reports instead of swallowing |
| 3 | Cleanup succeeds, then background work recreates the path | `scripts/ci/tempdir-leak-probe.sh` |
| 4 | The write was never inside the fixture: a lock or marker placed as a *sibling* of the fixture root | `scripts/ci/tempdir-leak-probe.sh` |

Class 1 is mechanical, so it is a static CI gate. Classes 3 and 4 are not
detectable by inspection; the probe catches them at runtime by giving the suite
an empty `TMPDIR` and looking at what survives.

### Class 3 is not only about child processes

The original instance was a detached child process, but *any* writer that
outlives the test body does it. Every case found in this workspace:

- **A detached child process.** `claude-cli`'s `prompt-segment` spawns a refresh
  child that writes its `.refresh.at` marker after the cache file the test polls
  for.
- **A `tokio` task the owner only `abort()`s.** `abort()` schedules cancellation,
  it does not wait, so an in-flight write still lands. `ProxyProjection::drop`
  aborts its worker and does not touch the fail-close task at all.
- **A supervisor doing its job.** `ActivityBroker` re-arms a filesystem watcher
  when its watch root disappears, and re-creating the root is part of re-arming.
  Removing the `TempDir` *is* a root loss, so a live broker faithfully recreates
  the directory teardown just removed.

The common shape: the fixture is removed while something still holds a path into
it, and the writer calls `create_dir_all` before writing. One marker write is
enough to resurrect the whole tree.

### Class 4 is a placement bug, not a timing bug

`RemoteSessionAllocationLock` names its lock after the directory it guards and
writes it *beside* that directory. Pass it a `TempDir` root and the lock lands in
`$TMPDIR` next to the fixture, where no teardown owns it — a guaranteed leak with
no race involved. Give any such API a parent the fixture owns.

## Rules

1. **Never give a temp-dir handle to a `static`.** Rust does not drop statics, so
   its destructor can never run. `OnceLock`, `LazyLock`, and `Lazy` all count.
   When a process-scoped directory is genuinely needed, name it after the process
   and sweep stale siblings on startup — see
   `crates/plan-issue/tests/integration/common.rs`.
2. **Do not disarm cleanup.** `keep()` and `into_path()` hand back a bare path and
   cancel removal. Return the handle alongside the path and let the caller hold
   it for the test's duration.
3. **If a test starts background work, stop it before the fixture drops** — and
   wait on the *last* write, not the first observable one.
   `crates/codex-cli/tests/integration/prompt_segment_refresh.rs` and
   `crates/claude-cli/tests/integration.rs` wait for the refresh lock to be
   released rather than for the cache file, because the child writes its marker
   and releases its lock after the cache. Waiting on an intermediate artifact
   returns while writes are still pending.
4. **`abort()` is not a barrier; `await` is.** Aborting a task only schedules
   cancellation. Where a shutdown path already exists, use it — the projection
   tests call `finish_fail_close`, the broker tests call
   `ActivityBroker::shutdown`, and both `await` the task after aborting it. An
   early `return` out of a polling loop is the usual way this rule gets skipped.
5. **Give sibling-placing APIs a parent the fixture owns.** If a helper writes
   `<dir>.lock` next to `<dir>`, never hand it the fixture root; hand it a
   subdirectory. See `remote_session_allocation_lock_stays_inside_the_caller_owned_parent`.
6. **Prefer `nils_test_support::tempdir::ScopedTempDir` in new tests.** It uses
   `TempDir::close`, which surfaces the cleanup error that plain `Drop` discards,
   and turns a leak into a test failure.

Raw `tempfile::TempDir` is not banned. It is correct for the common case, and
classes 2 and 3 have nothing to do with using it directly, so a blanket ban would
target the wrong thing. The workspace has roughly 3,200 temp-dir creation sites
across 41 crates; the rules above are enforced on new code by the audit rather
than by rewriting all of them.

## How to measure a leak

Use `scripts/ci/tempdir-leak-probe.sh`. Three traps cost real time when
measuring by hand instead:

- **Temp directories are dotfiles.** `ls /tmp` does not list `.tmpXXXXXX`, so a
  before/after diff built on plain `ls` reports zero leaks no matter how many
  there are. Every measurement here needs `ls -A`.
- **The shared `/tmp` has other writers.** Editors, agents, and the user's own
  shells write there constantly, so a diff of the shared directory reports their
  entries as leaks — including for pure unit tests that touch no filesystem. Give
  the run a private `TMPDIR` instead of diffing a shared one.
- **Most of these leaks only appear under concurrency.** Running one test at a
  time on an idle host, the background writer wins its race every time and
  nothing leaks; the same test leaks 5 runs out of 5 when its module runs in
  parallel. Attribution therefore has to keep the parallelism and remove
  candidates (`--skip`) rather than run candidates alone.

The contents of a leaked directory identify the writer: `usage.refresh.at` is the
`claude-cli` refresh child, an empty `sessions/` is the activity broker re-arming,
and a stray `*.allocation.lock` is a sibling-placed lock.

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
- `scripts/ci/tempdir-leak-probe.sh` runs a test selection against a private
  empty `TMPDIR` and fails on anything left behind, which is the only detector
  for classes 3 and 4. It refuses a probe root inside the repository, for the
  reason in the next section.
- `--allow <glob>` exists for state a test *deliberately* reuses across runs
  under a fixed name. The workspace has exactly one:
  `git-cli-test-worker.<euid>`, a per-user cache of private worker binaries
  keyed by source digest, kept so concurrent readers observe a stable inode.
  Fixed-name reusable state is bounded and does not grow; a randomly-named
  directory never qualifies, so never allowlist a `.tmp*` entry.
- `scripts/ci/tests/tempdir-leak-audit.test.sh` and
  `scripts/ci/tests/tempdir-leak-probe.test.sh` cover the two scripts themselves.
  The probe's self-test stubs `cargo` on `PATH`, because a real workspace run
  cannot be made to leak on demand.
- All of them run from `scripts/ci/nils-cli-local-fast.sh`, so the local gate and
  CI agree.

The probe is a detector, not a guarantee: classes 3 and 4 are races, and a run
that happens to win every race reports clean. Treat a probe failure as
conclusive and a probe pass as evidence, and keep the authoring rules above.
