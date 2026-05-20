# Plan: Phase 1.5 — agent-runtime Render Engine and Minimal Drift Audit

## Overview

Phase 1 (in `sympoies/agent-runtime-kit`, Plan 01) pinned the manifest
schemas and shipped the `agent-runtime-cli` shell with `not
implemented` stubs. Phase 2 (the reporting POC) expects `agent-runtime
render` and `agent-runtime audit-drift` to do real work. This plan
closes that gap entirely inside `sympoies/nils-cli`:

- Sprint 1 — render engine core: Tera helpers (`script`, `skill_ref`,
  `state_out`, `cli_ref`), manifest ingest, `build/<product>/` output,
  `--update-golden` flag, incremental per-skill `.render-cache.json`.
- Sprint 2 — determinism lints + cross-process integration test.
- Sprint 3 — minimal `audit-drift` body covering the four blocking
  classes from the source doc (source-manifest validity, rendered diff,
  `$AGENT_HOME` leak, docs-home per product). Exit codes 0/1/2.
- Sprint 4 — `agent-runtime-cli` v0.1.0 release, tap bump, and the
  cross-repo `required_clis` floor bump back in agent-runtime-kit.

Plan 01 must be in `done` before Sprint 1 starts. Plan 03 (reporting
POC) consumes the binaries shipped here. Plan 04 covers the full
unsafe-scoring matrix, the remaining drift classes, and Decision #7's
Bump Ceremony — explicitly out of scope here.

## Read First

- Primary source: docs/plans/02-nils-cli-render-and-drift-audit/02-nils-cli-render-and-drift-audit-discussion-source.md
- Source type: discussion-to-implementation-doc
- Open questions carried into execution:
  - Ship `--update-golden` as a flag on `render` or a separate subcommand? Default: flag on `render`.
  - Workspace-wide vs. scoped determinism clippy lints? Default: scoped to `agent-runtime-cli` and `nils-common`.

## Scope

- In scope:
  - `crates/agent-runtime-cli/src/render/` — manifest ingest, Tera
    engine wiring, helpers, `build/<product>/` writer,
    `.render-cache.json` incremental cache.
  - `crates/agent-runtime-cli/src/audit_drift/` — minimal audit-drift
    body covering the four blocking classes plus exit-code shape.
  - `crates/agent-runtime-cli/tests/integration/render_determinism.rs`
    (new) — cross-process determinism harness.
  - `crates/agent-runtime-cli/tests/drift/` — fixture set covering each
    of the four classes.
  - Clippy gate inside `crates/agent-runtime-cli` and
    `crates/nils-common` rejecting `std::collections::HashMap`,
    `std::time::SystemTime::now`, and `chrono::Utc::now` in Tera /
    helper paths.
  - `release/crates-io-publish-order.txt` and
    `.github/workflows/release.yml` updates so v0.1.0 of
    `agent-runtime-cli` ships through the existing nils-cli release
    flow.
  - `sympoies/homebrew-tap/Formula/nils-cli.rb` bump.
  - Cross-repo `required_clis` floor bump in
    `sympoies/agent-runtime-kit/manifests/*.yaml`.
- Out of scope:
  - Unsafe scoring (signals / weights / threshold) — Plan 04.
  - `extra`, `intentional-difference`, root-map drift classes —
    Plan 04.
  - Bump Ceremony / `agent-runtime doctor` body (Decision #7) — Plan
    04.
  - `install`, `uninstall`, `gc-backups`, `restore-backups`,
    `purge-state` subcommand bodies — later phases.
  - Schema changes to any of the five Phase 1 manifests.

## Assumptions

1. Plan 01 in agent-runtime-kit has merged: `manifests/skills.yaml`,
   `manifests/plugins.yaml`, `manifests/product-capabilities.yaml`,
   `manifests/runtime-roots.yaml`, `manifests/cli-tools.yaml` are
   tracked, and `crates/agent-runtime-cli/` exists as a stub crate in
   the nils-cli workspace with the `agent-runtime` binary registered.
2. `nils-common` already exposes path-resolution primitives the Tera
   helpers can call without re-implementing path canonicalisation.
3. `agent-out` already implements `path-for` and accepts the
   `--repo` / `--topic` flags used by `state_out` in
   `state_out_mode: runtime`.
4. `tera` and `indexmap` are workspace-acceptable Rust dependencies;
   `chrono` is allowed only as a parser, not as a clock source.
5. The nils-cli release workflow (`.github/workflows/release.yml`)
   already publishes every workspace binary together; adding
   `agent-runtime` is config, not new infra.
6. `sympoies/homebrew-tap` ships `nils-cli.rb` as the single formula
   for every binary — no per-binary formula is needed for
   `agent-runtime`.
7. The agent-runtime-kit `required_clis['agent-runtime']` placeholder
   currently reads `"<TBD: pin during Phase 1>"` or similar; Sprint 4
   replaces it with `">=0.1.0"`.

## Sprint 1: Render engine core

**Goal**: Make `agent-runtime render` actually render. Read the five
Phase 1 manifests, evaluate Tera templates with the four registered
helpers, write byte-stable output under `build/<product>/`, and back
unchanged skills with the per-skill `.render-cache.json`.

**Demo/Validation**:

- Commands:
  - `cargo test -p agent-runtime-cli render`
  - `cargo build -p agent-runtime-cli --release`
  - Manual: from a clean clone of agent-runtime-kit, run
    `agent-runtime render --source-root . --product codex`; inspect
    `build/codex/` for a populated skill tree.
  - Manual: re-run the same command and confirm `.render-cache.json`
    reports cache hits for unchanged skills.
  - Manual: `agent-runtime render --source-root . --product codex
    --update-golden` rewrites `tests/golden/codex/<skill>/expected/`
    in the caller repo.
- Verify: every helper expands to the documented form; cache hit and
  cache miss produce identical output for the same input.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 1.1: Wire manifest ingest and the `--source-root` flag

- **Location**:
  - crates/agent-runtime-cli/src/render/mod.rs
  - crates/agent-runtime-cli/src/render/manifest.rs
  - crates/agent-runtime-cli/src/commands/render.rs
- **Description**: Replace the `not implemented` body of the `render`
  subcommand with code that reads the five Phase 1 manifests
  (`manifests/skills.yaml`, `manifests/plugins.yaml`,
  `manifests/product-capabilities.yaml`,
  `manifests/runtime-roots.yaml`, `manifests/cli-tools.yaml`) from
  the directory passed as `--source-root` (default: current working
  directory). Validate `schema_version` on each manifest; emit a clear
  error naming the file and the offending field when validation fails.
  Build a typed context struct keyed by `IndexMap` / `BTreeMap` only —
  no bare `HashMap` at the Tera context entry point.
- **Dependencies**:
  - none
- **Complexity**: 6
- **Acceptance criteria**:
  - All five manifests deserialise into typed structs; unknown
    fields are rejected when `schema_version` matches `1`.
  - `--source-root` defaults to `std::env::current_dir()` and is
    canonicalised before manifests resolve.
  - Missing manifest file produces a non-zero exit with the file
    name and the resolution path printed to stderr.
  - Context struct uses `IndexMap` or `BTreeMap` for every map
    visible to Tera; no `HashMap` import in `src/render/`.
- **Validation**:
  - `cargo test -p agent-runtime-cli render::manifest`
  - `cargo clippy -p agent-runtime-cli --all-targets -- -D warnings`

### Task 1.2: Register the four Tera helpers

- **Location**:
  - crates/agent-runtime-cli/src/render/helpers/mod.rs
  - crates/agent-runtime-cli/src/render/helpers/script.rs
  - crates/agent-runtime-cli/src/render/helpers/skill_ref.rs
  - crates/agent-runtime-cli/src/render/helpers/state_out.rs
  - crates/agent-runtime-cli/src/render/helpers/cli_ref.rs
- **Description**: Implement four registered Tera functions matching
  the contract in `agent-runtime-kit/docs/source/inventory-target-architecture.md`:
  `script(path)` (resolves a core script path via `nils-common`),
  `skill_ref(id)` (resolves a tracked skill id to its product-side
  name + render target), `state_out(domain, topic=, repo=)` (emits an
  `agent-out path-for` invocation in `state_out_mode: runtime`, or the
  resolved literal path in `state_out_mode: literal`, per the skill
  manifest), and `cli_ref(name)` (resolves a `required_clis` entry to
  its declared binary + min-version label for inline rendering). Each
  helper returns a `tera::Value::String`. Errors propagate as
  `tera::Error` with the offending argument named.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 7
- **Acceptance criteria**:
  - All four helpers are reachable via `{{ helper(...) }}` in a Tera
    template loaded by `agent-runtime render`.
  - `state_out` switches mode based on the per-skill
    `state_out_mode` manifest field, defaulting to `runtime`.
  - `script` rejects paths outside `core/`, `targets/`, `manifests/`
    with a typed error (no panic).
  - `cli_ref` rejects unknown binaries (binary not present in
    `cli-tools.yaml` or the skill's `required_clis`) with a typed
    error.
  - Unit tests cover the happy path and at least one rejection per
    helper.
- **Validation**:
  - `cargo test -p agent-runtime-cli render::helpers`

### Task 1.3: Write `build/<product>/` output and per-skill cache

- **Location**:
  - crates/agent-runtime-cli/src/render/writer.rs
  - crates/agent-runtime-cli/src/render/cache.rs
- **Description**: After Tera evaluation, write each rendered file
  under `build/<product>/...` using a stable, sorted traversal of the
  context. Compute a per-skill SHA-256 of (skill source + product
  capability hash + helper output signature) and persist to
  `build/<product>/.render-cache.json` keyed by skill id. Unchanged
  skills are copied verbatim from a prior build directory when the
  hash matches. Render reads only from `core/`, `targets/`,
  `manifests/` — never `~/.codex`, `~/.claude`, or any runtime state.
- **Dependencies**:
  - Task 1.2
- **Complexity**: 7
- **Acceptance criteria**:
  - Output tree under `build/<product>/` is deterministic across
    runs for the same input.
  - `.render-cache.json` records every skill that rendered with its
    hash; missing entries trigger a full re-render of that skill.
  - Cache-hit copy path produces byte-identical content to the
    cache-miss render path for the same source.
  - Render never opens a path outside the configured `--source-root`
    subtree (enforced by a unit test using a sandboxed temp dir).
- **Validation**:
  - `cargo test -p agent-runtime-cli render::writer`
  - `cargo test -p agent-runtime-cli render::cache`

### Task 1.4: Add `--update-golden` flag

- **Location**:
  - crates/agent-runtime-cli/src/commands/render.rs
  - crates/agent-runtime-cli/src/render/golden.rs
- **Description**: Add a `--update-golden` flag to `agent-runtime
  render` (matches `cargo insta --accept`). When set, after rendering
  the active `--product`, copy each rendered file from
  `build/<product>/<skill>/` into
  `tests/golden/<product>/<skill>/expected/` inside `--source-root`.
  When the flag is absent, behaviour is unchanged. Warn (do not error)
  when no golden directory exists yet — create it.
- **Dependencies**:
  - Task 1.3
- **Complexity**: 4
- **Acceptance criteria**:
  - `--update-golden` writes only inside
    `tests/golden/<product>/...` relative to `--source-root`.
  - Without the flag, render never touches `tests/golden/`.
  - Help text states the flag's purpose and its scope ("rewrites
    only the active --product subtree").
  - Integration test exercises both flag-on and flag-off paths.
- **Validation**:
  - `cargo test -p agent-runtime-cli render::golden`

## Sprint 2: Determinism lints and cross-process tests

**Goal**: Lock the determinism contract from Resolved Decision #9 in
code rather than convention. Add clippy lints that reject the
forbidden imports inside the affected crates, and add the
cross-process integration test that catches a future regression
where two cold processes produce different output.

**Demo/Validation**:

- Commands:
  - `cargo clippy -p agent-runtime-cli -p nils-common --all-targets -- -D warnings`
  - `cargo test -p agent-runtime-cli render_determinism`
  - Manual: temporarily add a `use std::collections::HashMap;` inside
    `crates/agent-runtime-cli/src/render/` and confirm clippy fails.
  - Manual: temporarily add a `chrono::Utc::now()` call in a helper
    and confirm clippy fails.
- Verify: the determinism contract is enforced at compile time, not
  only by golden snapshots.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 2.1: Add determinism clippy lints to affected crates

- **Location**:
  - crates/agent-runtime-cli/Cargo.toml
  - crates/agent-runtime-cli/src/lib.rs
  - crates/agent-runtime-cli/clippy.toml
  - crates/nils-common/src/lib.rs
  - crates/nils-common/clippy.toml
- **Description**: Configure `clippy::disallowed_types` (for
  `std::collections::HashMap`) and `clippy::disallowed_methods` (for
  `std::time::SystemTime::now` and `chrono::Utc::now`) inside
  `clippy.toml` for both `agent-runtime-cli` and `nils-common`.
  Register the lints in each crate root via
  `#![deny(clippy::disallowed_types, clippy::disallowed_methods)]`.
  Add a short module-level comment in each crate root naming
  Resolved Decision #9 as the source of the rule and pointing at
  the source doc anchor. Scope is intentionally per-crate — see Open
  Questions in the source doc.
- **Dependencies**:
  - none
- **Complexity**: 5
- **Acceptance criteria**:
  - Adding `use std::collections::HashMap;` in either crate fails
    `cargo clippy -- -D warnings`.
  - Calling `chrono::Utc::now()` or
    `std::time::SystemTime::now()` in either crate fails clippy.
  - `IndexMap` and `BTreeMap` remain permitted.
  - Module-level comment names Decision #9 in each crate root.
- **Validation**:
  - `cargo clippy -p agent-runtime-cli --all-targets -- -D warnings`
  - `cargo clippy -p nils-common --all-targets -- -D warnings`

### Task 2.2: Add cross-process render determinism integration test

- **Location**:
  - crates/agent-runtime-cli/tests/integration/render_determinism.rs
  - crates/agent-runtime-cli/tests/fixtures/render-determinism/manifests/skills.yaml
  - crates/agent-runtime-cli/tests/fixtures/render-determinism/manifests/plugins.yaml
  - crates/agent-runtime-cli/tests/fixtures/render-determinism/manifests/product-capabilities.yaml
  - crates/agent-runtime-cli/tests/fixtures/render-determinism/manifests/runtime-roots.yaml
  - crates/agent-runtime-cli/tests/fixtures/render-determinism/manifests/cli-tools.yaml
  - crates/agent-runtime-cli/tests/fixtures/render-determinism/core/skills/sample/SKILL.md.tera
- **Description**: Add an integration test that builds a small
  fixture source root under
  `tests/fixtures/render-determinism/` (manifests + one skill body
  per product), then spawns `agent-runtime render --source-root <fixture>
  --product codex` twice via `std::process::Command`, deleting
  `build/codex/.render-cache.json` between the two runs. After both
  runs, walk each `build/codex/` tree with sorted-directory
  traversal and assert every file is byte-for-byte identical. Repeat
  the same for `--product claude`.
- **Dependencies**:
  - Task 1.3
  - Task 2.1
- **Complexity**: 6
- **Acceptance criteria**:
  - Two `std::process::Command` invocations (separate processes)
    produce byte-identical `build/<product>/` for both Codex and
    Claude.
  - Deleting `.render-cache.json` between runs does not change the
    output bytes (cache hit and cache miss agree).
  - Fixture under `tests/fixtures/render-determinism/` is small
    (single skill per product, no large binaries).
  - Introducing a `SystemTime::now()`-derived value into a helper
    fails the test (covered by an inline negative-control comment).
- **Validation**:
  - `cargo test -p agent-runtime-cli render_determinism`

### Task 2.3: Document the only sanctioned time value

- **Location**:
  - crates/agent-runtime-cli/src/render/time.rs
  - crates/agent-runtime-cli/docs/determinism.md
- **Description**: Introduce a single module `src/render/time.rs`
  whose public function `source_commit_timestamp() -> Result<String>`
  shells out to `git log -1 --format=%cI HEAD` at render start and
  returns the ISO-8601 string. Document in
  `crates/agent-runtime-cli/docs/determinism.md` that this is the
  only sanctioned time-shaped value in rendered output, that
  `SystemTime::now()` and `chrono::Utc::now()` are clippy-banned
  inside `agent-runtime-cli` and `nils-common`, and that helpers
  must use `IndexMap` / `BTreeMap` at Tera context entry points.
  Cross-link to Resolved Decision #9 in the source doc.
- **Dependencies**:
  - Task 2.1
- **Complexity**: 3
- **Acceptance criteria**:
  - `source_commit_timestamp()` returns a stable ISO-8601 value for
    the same `HEAD`.
  - Failure to shell out to `git` returns a typed error (no panic).
  - `determinism.md` covers the three rules and links to Decision
    #9.
- **Validation**:
  - `cargo test -p agent-runtime-cli render::time`

## Sprint 3: Minimal `audit-drift` body

**Goal**: Cover the four blocking classes the Phase 2 reporting POC
depends on, with deterministic exit codes (0 clean, 1 warn, 2 block).
Defer the rest of the matrix to Plan 04.

**Demo/Validation**:

- Commands:
  - `cargo test -p agent-runtime-cli audit_drift`
  - Manual: `agent-runtime audit-drift --source-root .` against a
    clean fixture exits `0`.
  - Manual: inject an `$AGENT_HOME` reference into a fixture
    rendered file → `audit-drift` exits `2`.
  - Manual: render a Codex policy line with `--docs-home "$HOME/.claude"`
    instead of `"$CODEX_HOME"` → `audit-drift` exits `2`.
- Verify: each blocking class fires its expected exit code and
  prints a finding line naming the class and the offending path.

**PR grouping intent**: `group`
**Execution Profile**: `parallel-x2`

### Task 3.1: Source-manifest validity and rendered-target diff classes

- **Location**:
  - crates/agent-runtime-cli/src/audit_drift/mod.rs
  - crates/agent-runtime-cli/src/audit_drift/source_manifest.rs
  - crates/agent-runtime-cli/src/audit_drift/rendered_target.rs
  - crates/agent-runtime-cli/src/commands/audit_drift.rs
- **Description**: Replace the `not implemented` body of `audit-drift`
  with a runner that executes two classes: (a) source-manifest
  validity — re-validate every Phase 1 manifest against its schema
  and report any schema or `<TBD>` placeholders as `missing`-class
  findings; (b) rendered-target vs source diff — re-render to a
  scratch directory and diff against the current `build/<product>/`
  tree, reporting any byte-level difference as a `stale`-class
  finding with the file path. Exit code policy: 0 if no findings, 1
  if only `warn`-tier findings, 2 if any `block`-tier finding fires.
  Both classes in this task are `warn`-tier (exit 1) by default.
- **Dependencies**:
  - Task 1.3
- **Complexity**: 7
- **Acceptance criteria**:
  - Source-manifest schema failure produces an exit-1 run with a
    finding line naming the manifest file and the field.
  - `<TBD>` literal in any tracked manifest produces an exit-1
    finding (consistent with the source doc's Phase 1 gate).
  - Rendered-target diff produces an exit-1 finding naming the
    offending file path.
  - Exit code is `0` against a clean fixture.
- **Validation**:
  - `cargo test -p agent-runtime-cli audit_drift::source_manifest`
  - `cargo test -p agent-runtime-cli audit_drift::rendered_target`

### Task 3.2: `$AGENT_HOME` leak class (blocking, exit 2)

- **Location**:
  - crates/agent-runtime-cli/src/audit_drift/agent_home_leak.rs
- **Description**: Walk every file under `build/<product>/` and every
  file under tracked `core/` / `targets/` / `manifests/` paths. Any
  occurrence of the literal string `$AGENT_HOME` (case-sensitive)
  raises a `block`-tier finding per Resolved Decision #5. Exit code
  must be `2` if any finding fires. Allowlist the source doc itself
  (`docs/source/inventory-target-architecture.md` in the caller repo)
  because it discusses the removed variable by name; the allowlist is
  hard-coded for this class to avoid taking on the larger
  `drift-audit.allow.yaml` machinery in this plan.
- **Dependencies**:
  - Task 3.1
- **Complexity**: 4
- **Acceptance criteria**:
  - A `$AGENT_HOME` substring anywhere in `build/<product>/` triggers
    exit code 2.
  - A `$AGENT_HOME` substring in `core/` or `manifests/` triggers exit
    code 2.
  - The source doc allowlist entry does not fire.
  - Finding line names the file path and the byte offset.
- **Validation**:
  - `cargo test -p agent-runtime-cli audit_drift::agent_home_leak`

### Task 3.3: Docs-home per product class (blocking, exit 2)

- **Location**:
  - crates/agent-runtime-cli/src/audit_drift/docs_home.rs
- **Description**: Scan rendered policy lines under
  `build/<product>/` for `--docs-home` usage. Codex outputs must use
  `--docs-home "$CODEX_HOME"` exactly; Claude outputs must use
  `--docs-home "$HOME/.claude"` exactly. Any mismatch (wrong
  variable, missing quoting, wrong product subtree) is a `block`-tier
  finding (exit 2). Lines without `--docs-home` are not findings.
- **Dependencies**:
  - Task 3.1
- **Complexity**: 4
- **Acceptance criteria**:
  - Codex tree containing `--docs-home "$HOME/.claude"` triggers exit
    2.
  - Claude tree containing `--docs-home "$CODEX_HOME"` triggers exit
    2.
  - Correct rendering in both trees exits 0 from this class.
  - Finding line names the file and the exact mismatched arg.
- **Validation**:
  - `cargo test -p agent-runtime-cli audit_drift::docs_home`

### Task 3.4: Audit-drift fixture set covering each class

- **Location**:
  - crates/agent-runtime-cli/tests/drift/fixtures/clean/manifests/skills.yaml
  - crates/agent-runtime-cli/tests/drift/fixtures/agent-home-leak/build/codex/skills/sample.md
  - crates/agent-runtime-cli/tests/drift/fixtures/docs-home-mismatch/build/codex/policy/docs-home.md
  - crates/agent-runtime-cli/tests/drift/fixtures/manifest-placeholder-pin/manifests/skills.yaml
  - crates/agent-runtime-cli/tests/drift/fixtures/rendered-stale/build/codex/skills/sample.md
  - crates/agent-runtime-cli/tests/drift/audit_drift_classes.rs
- **Description**: Build a fixture set covering each of the four
  classes plus a `clean` baseline. Each fixture directory contains
  a minimal `core/` + `manifests/` + `build/<product>/` tree shaped
  so the named class fires (and no other). The integration test
  enumerates the fixtures and asserts the expected exit code per
  fixture. Wire the directory paths into the test with trailing-`/`
  Location entries so plan-tooling and the test harness agree on
  layout.
- **Dependencies**:
  - Task 3.1
  - Task 3.2
  - Task 3.3
- **Complexity**: 5
- **Acceptance criteria**:
  - `clean` fixture exits 0.
  - `manifest-placeholder-pin` and `rendered-stale` exit 1.
  - `agent-home-leak` and `docs-home-mismatch` exit 2.
  - Each fixture is small (≤ 5 files) and committed without binary
    blobs.
- **Validation**:
  - `cargo test -p agent-runtime-cli audit_drift_classes`

## Sprint 4: Release and cross-repo handoff

**Goal**: Cut `agent-runtime-cli` v0.1.0 through the nils-cli release
workflow, bump the Homebrew tap formula, and update
agent-runtime-kit's manifest floors so Phase 2 can start with a
binary it can actually pin against.

**Demo/Validation**:

- Commands:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh`
  - `git tag agent-runtime-cli-v0.1.0` (preview only; the release
    skill drives the actual tag).
  - Manual: `brew upgrade sympoies/tap/nils-cli` on a clean host and
    confirm `agent-runtime --version` reports `0.1.0`.
  - Manual: inside agent-runtime-kit, `git grep -F 'pin during Phase 1' -- manifests/`
    returns no lines after Task 4.3.
- Verify: a clean host installs nils-cli 0.1.x, gets the
  `agent-runtime` binary, and agent-runtime-kit's manifests pin to
  `>=0.1.0` rather than placeholder.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 4.1: Tag `agent-runtime-cli` v0.1.0 and update release config

- **Location**:
  - crates/agent-runtime-cli/Cargo.toml
  - release/crates-io-publish-order.txt
  - .github/workflows/release.yml
- **Description**: Bump `crates/agent-runtime-cli/Cargo.toml`'s
  `version` from the Plan 01 `0.0.1-dev` to `0.1.0`. Add
  `agent-runtime-cli` to `release/crates-io-publish-order.txt`
  positioned after its dependencies (`nils-common`, `agent-out`,
  `nils-term`). Update `.github/workflows/release.yml` to include
  the `agent-runtime` binary in the workspace bin list so the
  packaged tarball ships it under `bin/agent-runtime`. Drive the
  tag itself via the repo's release skill; do not hand-tag.
- **Dependencies**:
  - Task 2.2
  - Task 3.4
- **Complexity**: 5
- **Acceptance criteria**:
  - `agent-runtime-cli` builds at `0.1.0` in
    `cargo build -p agent-runtime-cli --release`.
  - `release/crates-io-publish-order.txt` lists
    `agent-runtime-cli` after its dependencies.
  - Release workflow's binary list includes `agent-runtime`.
  - No manual `git tag` invocation in this task; release skill owns
    the tag.
- **Validation**:
  - `cargo build -p agent-runtime-cli --release`
  - `cargo test -p agent-runtime-cli`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh`

### Task 4.2: Bump `homebrew-tap` formula

- **Location**:
  - ../homebrew-tap/Formula/nils-cli.rb
- **Description**: After v0.1.0 release artifacts are published, bump
  the `url` and `sha256` for each platform in
  `sympoies/homebrew-tap/Formula/nils-cli.rb` to point at the new
  release tarball. Use the per-platform SHA-256 published by the
  release workflow. Run `brew audit --strict --online
  sympoies/tap/nils-cli` locally before opening the formula PR.
- **Dependencies**:
  - Task 4.1
- **Complexity**: 3
- **Acceptance criteria**:
  - `Formula/nils-cli.rb` references the v0.1.x release URL set.
  - `brew audit --strict --online sympoies/tap/nils-cli` passes
    locally.
  - `brew upgrade sympoies/tap/nils-cli` on a clean host installs
    the new `agent-runtime` binary alongside the existing nils-cli
    binaries.
- **Validation**:
  - `brew audit --strict --online sympoies/tap/nils-cli`
  - `brew reinstall sympoies/tap/nils-cli && agent-runtime --version`

### Task 4.3: Cross-repo: bump `required_clis` floors in agent-runtime-kit (cross-repo)

- **Location**:
  - ../agent-runtime-kit/manifests/skills.yaml
  - ../agent-runtime-kit/manifests/plugins.yaml
  - ../agent-runtime-kit/manifests/cli-tools.yaml
- **Description**: Cross-repo task — operate inside
  `sympoies/agent-runtime-kit`. Replace every
  `required_clis['agent-runtime']` placeholder authored by Plan 01
  Sprint 2 (currently `"<TBD: pin during Phase 1>"`) with
  `">=0.1.0"`. Bump any other `required_clis` entry whose binary
  was first shipped in this nils-cli release to its `0.x.0` floor.
  Run the agent-runtime-kit drift audit locally against the bumped
  manifests and confirm zero `<TBD>` findings. This task does not
  touch the nils-cli workspace.
- **Dependencies**:
  - Task 4.2
- **Complexity**: 3
- **Acceptance criteria**:
  - `git -C ../agent-runtime-kit grep -F 'pin during Phase 1' -- manifests/`
    returns no matches.
  - Every `required_clis['agent-runtime']` entry reads `">=0.1.0"`.
  - `agent-runtime audit-drift --source-root ../agent-runtime-kit`
    exits 0.
  - The PR for this change is opened against
    `sympoies/agent-runtime-kit`, not nils-cli.
- **Validation**:
  - `agent-runtime audit-drift --source-root ../agent-runtime-kit`
  - `git -C ../agent-runtime-kit grep -F 'pin during Phase 1' -- manifests/`

## Testing Strategy

- Unit: per-helper coverage in `src/render/helpers/`, per-class
  coverage in `src/audit_drift/`, plus `render::time` and
  `render::cache`.
- Integration: `tests/integration/render_determinism.rs`
  (cross-process), `tests/drift/audit_drift_classes.rs` (fixture
  matrix), `tests/integration/render_golden.rs` (covered implicitly
  by Task 1.4's flag-on / flag-off paths).
- Clippy: `cargo clippy -p agent-runtime-cli -p nils-common
  --all-targets -- -D warnings` after Sprint 2; this is the
  determinism gate.
- Workspace: `bash scripts/ci/nils-cli-checks-entrypoint.sh` end of
  Sprint 2, Sprint 3, and Sprint 4.
- Manual smoke: clean-clone render of agent-runtime-kit twice;
  byte-diff `build/` between the two runs.
- Release smoke: `brew reinstall sympoies/tap/nils-cli &&
  agent-runtime --version` reports `0.1.0`.

## Risks & gotchas

- The Bump Ceremony from Resolved Decision #7 is OUT OF SCOPE for
  this plan and lands with Plan 04's doctor work. Do not preempt it
  here — pinning `min_version_effective_from` or
  `recommended_version` is Plan 04's job.
- Cross-process determinism is the harder half of the contract. If
  Tera's evaluation order changes between minor versions, the golden
  snapshots will need a rebake; the rebake itself must produce
  identical output across two processes before the new snapshot is
  committed.
- `state_out` runtime mode shells out to `agent-out` at skill
  execution time. The render-time output is a literal command
  string, not a path. Drift audit must compare against the command
  shape, not against an allocated path.
- The `--docs-home` class is product-specific; if a third product
  joins later, the class needs an extra arm. Keep the class table-
  driven from `runtime-roots.yaml` rather than hard-coded so the
  Plan 04 work is additive.
- The cross-repo task in Sprint 4 (Task 4.3) opens a PR against
  agent-runtime-kit, not nils-cli. Make sure the dispatch / PR
  workflow understands the `..` repo path and routes review to the
  right repo's owners.

## Rollback plan

- Sprint 1 rollback: revert the `render` body PR; the subcommand
  returns to the Plan 01 `not implemented` stub. Manifests are
  unchanged, so no other consumer breaks.
- Sprint 2 rollback: revert the clippy lint PR (the lints sit in
  `clippy.toml` files and crate-root attributes; removing them
  unblocks compile). The cross-process integration test can stay as
  durable coverage even if the lints come out.
- Sprint 3 rollback (per task): each of Task 3.2, 3.3, 3.4 is
  scoped to its own module and can be reverted independently; the
  `audit-drift` runner falls back to source-manifest validity +
  rendered-target diff (Task 3.1), still useful for the reporting
  POC but with weaker coverage.
- Sprint 4 rollback: pin the homebrew-tap formula back to v0.10.x in
  `sympoies/homebrew-tap`, revert the agent-runtime-kit floor bump
  PR, and yank the v0.1.0 release tag. The Plan 01 placeholder
  floors return to `"<TBD: pin during Phase 1>"`.
