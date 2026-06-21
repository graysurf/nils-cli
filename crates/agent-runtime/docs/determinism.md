# agent-runtime determinism contract

Render output must be a pure function of the source-root contents. Two
cold processes running `agent-runtime render --source-root <root>
--product <product>` against the same source tree MUST produce
byte-identical output under `build/<product>/`. This document captures
the three rules that enforce that contract and the single escape hatch
the engine is allowed to take.

Source: [`agent-runtime-kit/docs/source/inventory-target-architecture.md`
→ Resolved Decision #9](https://github.com/graysurf/agent-runtime-kit/blob/main/docs/source/inventory-target-architecture.md#resolved-decisions).

## Rule 1 — no hash-randomized iteration on the render path

Rust's `std::collections::HashMap` (and its `HashSet` sibling,
`hash_map::DefaultHasher`, `hash_map::RandomState`) randomises
iteration order, so a context fed to the render engine via any of
these would render the same source to different bytes on different
processes (and on the same process across runs, when the underlying
hasher seeds differ).

Enforcement:

- `crates/agent-runtime/clippy.toml` and
  `crates/nils-common/clippy.toml` list `std::collections::HashMap`,
  `std::collections::HashSet`,
  `std::collections::hash_map::DefaultHasher`, and
  `std::collections::hash_map::RandomState` under `disallowed-types`.
- The `#![deny(clippy::disallowed_types, clippy::disallowed_methods)]`
  attribute at the top of each crate's `lib.rs` makes a violation a
  build failure under `cargo clippy --all-targets -- -D warnings`.
- The render code uses `IndexMap` (insertion-ordered) or `BTreeMap`
  (key-sorted) for every map that crosses into the render engine.
- Filesystem directory walks (`std::fs::read_dir`) sort their entries
  before consumption — the OS returns them in arbitrary order on
  most filesystems. The integration test in
  `tests/integration/render_determinism.rs` and the production walk
  in `render::golden::update_golden` both sort before iterating.

No exemption: the render path is HashMap-free outright. Previously
`crates/agent-runtime/src/render/helpers/` carried a single
`#![allow(clippy::disallowed_types)]` because Tera's `Function` trait
signature forced `&HashMap<String, Value>` on every helper closure.
The minijinja render engine instead hands helpers their keyword
arguments as a `minijinja::value::Kwargs` bag, so that exemption was
removed during the Tera→minijinja migration. The
`render_subtree_has_no_unsanctioned_disallowed_types_allow`
integration test asserts no module under `src/render/` silences
`disallowed_types` — adding `#[allow(...)]` anywhere on the render
path fails the test.

## Rule 2 — no wall-clock or monotonic time

`std::time::SystemTime::now()`, `std::time::Instant::now()`,
`chrono::Utc::now()`, and `chrono::Local::now()` all produce values
that change on every call. If any of these landed in rendered output
the determinism contract would break immediately.

Enforcement: same clippy.toml + `#![deny(...)]` mechanism as Rule 1.

Single exemption: [`render::time::source_commit_timestamp`]. This
function shells out to `git -C <source-root> log -1 --format=%cI HEAD`
and returns the ISO-8601 commit timestamp of the source-root's HEAD.
The value changes only when the source tree itself changes, so it
stays stable for any given source state.

If a template needs a date value, this is the only sanctioned source.
Render-path helpers may call it directly; the lint stays silent
because `source_commit_timestamp` calls `Command::new("git")` rather
than the disallowed `now()` methods.

## Rule 3 — read only from `core/` / `targets/` / `manifests/`

Render must not consult any path outside the source-root subtree. A
helper that read `$HOME/.codex`, `$HOME/.claude`, or runtime state
would couple render output to the host machine and break determinism.

Enforcement:

- `render::writer::sandboxed_join` rejects any path containing `..` or
  starting with `/`, anchoring every render-time path to
  `<source-root>/`.
- `render::writer::canonicalize_under` and
  `render::writer::guard_write_under` resolve the candidate path,
  canonicalise it, and reject anything that resolves outside the
  source root or build root. This closes the symlink-escape vector
  where a hostile `SKILL.md.tera` could be a symlink pointing at
  `/etc/passwd`.
- `render::helpers::script` accepts only paths starting with `core/`,
  `targets/`, or `manifests/`.

## What this contract does NOT cover

- Cross-platform path separator normalization (rendered output ships
  Unix paths; no Windows-host support is in scope).
- TOCTOU between `canonicalize_under` and the subsequent `fs::read`.
  The brief race window is acceptable for the threat model — an
  attacker with write access to the source root during render has
  already compromised the system. Linux-only `openat2(RESOLVE_BENEATH)`
  closes the race; we'll add it if/when the threat model expands.
- Reproducible binary output (the renderer emits text files; binary
  determinism is a build-system concern).

## Verifying the contract locally

```bash
# Determinism gate
cargo clippy -p nils-agent-runtime -p nils-common --all-targets -- -D warnings

# Cross-process determinism integration test
cargo test -p nils-agent-runtime render_determinism

# Whole render unit + integration suite
cargo test -p nils-agent-runtime
```

A failing clippy run, a non-byte-identical second render, or a leaked
non-sanctioned time/HashMap import means the contract is broken — fix
before merge.
