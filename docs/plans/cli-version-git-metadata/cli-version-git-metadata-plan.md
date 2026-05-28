# Plan: CLI version git metadata

## Overview

Add a single shared, zero-dependency build-info crate (`nils-build-info`) that
captures `git describe --tags --always --dirty` and the rustc version at build
time, then wire every binary crate's clap definition so `--version` (long)
renders `<semver> (<git-describe>, rustc <ver>)` while `-V` (short) stays clean
semver. This makes dev builds between release tags traceable to a commit and a
dirty working tree without touching the release/version-bump flow and without
adding any third-party dependency.

Source: this bundle's discussion source doc (Read First, below). All open
questions were resolved at the source doc (rollout = all binary crates in one
PR; include rustc version; single-line long-version format).

## Read First

- Primary source:
  `docs/plans/cli-version-git-metadata/cli-version-git-metadata-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Source issue: none (feature discussed 2026-05-29; no originating issue)
- Open questions carried into execution: none (rollout breadth, rustc
  inclusion, and long-version format all locked at the source doc).
- Implementation surface:
  - New crate `crates/nils-build-info/` (`build.rs` + `src/lib.rs`).
  - Each binary crate's clap command definition (e.g.
    `crates/cli-template/src/main.rs:13` `#[command(... version ...)]` and the
    equivalent builder sites).
- Out of scope (tracked separately): adding `vergen`/`gix`/`git2`; any change
  to the release/version-bump flow; a machine-readable `version --json`
  subcommand.

## Read First boundary

- Keep the semver as a substring of `--version`; do not break
  `crates/agent-runtime-cli/tests/integration/cli.rs` (it asserts `--version`
  *contains* `CARGO_PKG_VERSION`).
- Do not add a `build.rs` to `nils-common` or any widely-depended lib crate;
  the build-info `build.rs` lives only in the `nils-build-info` leaf crate.
- No new third-party dependency, so `third-party-artifacts` and the
  `Cargo.lock` locked-build gate stay clean.

## Scope

- In scope:
  - A new leaf crate `nils-build-info` with a zero-dependency `build.rs` that
    emits `NILS_GIT_DESCRIBE` and `NILS_RUSTC_VERSION` via `cargo:rustc-env`,
    wires `rerun-if-changed=.git/HEAD` and `.git/refs`, and falls back to
    `unknown` when `.git` is absent or git fails.
  - Public surface on `nils-build-info`: `GIT_DESCRIBE`, `RUSTC_VERSION`
    consts and a `long_version(pkg_version: &str) -> String` helper producing
    `<semver> (<git-describe>, rustc <ver>)`.
  - Wiring every binary crate that renders `--version` so `-V` keeps clean
    semver and `--version` shows the long string, depending on
    `nils-build-info`.
  - Unit tests for `long_version` formatting and integration coverage that
    `--version` contains the semver and the `git describe`/rustc tokens.
- Out of scope:
  - Any change to `ingest-evidence`-style flows or unrelated `--version`-free
    crates.
  - Non-user-facing `CARGO_PKG_VERSION` uses (e.g. the `web-evidence`
    user-agent string).
  - The release/version-bump flow and homebrew-tap bump.

## Assumptions

- All workspace crates share `[workspace.package]` version, so a semver passed
  from each crate's own `env!("CARGO_PKG_VERSION")` is the correct release
  version to display; the build-info crate only owns the git/rustc tokens.
- cargo sets the `RUSTC` env var for build scripts, so capturing the rustc
  version stays zero-dependency.
- The completion flag-parity audit's required-binary set (38 binaries) is the
  authoritative enumeration of binary crates to wire.
- Published builds (crates.io tarball, homebrew bottle) have no `.git`; the
  `unknown` fallback keeps those builds compiling and, on a clean tagged
  checkout, `git describe` resolves to the bare tag.

## Sprint 1: `nils-build-info` crate

**Goal**: A new leaf crate compiles in a git checkout and in a `.git`-less
tree, exposing the git-describe and rustc tokens plus a `long_version` helper,
with unit tests for the formatting and fallback.

**Demo/Validation**:

- Commands:
  - `cargo build -p nils-build-info`
  - `cargo test -p nils-build-info`
  - `cargo publish -p nils-build-info --dry-run`
- Verify: build succeeds; `long_version("0.26.1")` formats
  `0.26.1 (<describe>, rustc <ver>)`; dry-run packaging is clean.

### Task 1.1: Scaffold `nils-build-info` with the zero-dependency build.rs

- **Location**:
  - `crates/nils-build-info/Cargo.toml`, `crates/nils-build-info/build.rs`
  - root `Cargo.toml` workspace members
- **Description**: Create the leaf crate inheriting `[workspace.package]`.
  Add the zero-dependency `build.rs` that runs `git describe --tags --always
  --dirty` and `$RUSTC --version`, emits `cargo:rustc-env=NILS_GIT_DESCRIBE`
  and `cargo:rustc-env=NILS_RUSTC_VERSION`, prints
  `cargo:rerun-if-changed=.git/HEAD` and `.git/refs`, and falls back to
  `unknown` on any failure or missing `.git`.
- **Dependencies**: none
- **Complexity**: 1
- **Acceptance criteria**:
  - `cargo build -p nils-build-info` succeeds in a git checkout and in a tree
    with no `.git` directory.
  - No new entry appears in `Cargo.lock` beyond the new workspace crate.
- **Validation**:
  - `cargo build -p nils-build-info`
  - `cargo build -p nils-build-info` from a `.git`-stripped copy (or with
    `git` unavailable on PATH) shows the `unknown` fallback.

### Task 1.2: Public surface and unit tests

- **Location**:
  - `crates/nils-build-info/src/lib.rs`
  - `crates/nils-build-info/tests/` (or inline `#[cfg(test)]`)
- **Description**: Expose `pub const GIT_DESCRIBE: &str =
  env!("NILS_GIT_DESCRIBE");`, `pub const RUSTC_VERSION: &str =
  env!("NILS_RUSTC_VERSION");`, and `pub fn long_version(pkg_version: &str) ->
  String` returning `format!("{pkg_version} ({GIT_DESCRIBE}, rustc
  {RUSTC_VERSION})")`. Add unit tests asserting the format shape and that the
  passed-in semver is preserved verbatim.
- **Dependencies**: Task 1.1
- **Complexity**: 1
- **Acceptance criteria**:
  - `long_version("9.9.9")` starts with `9.9.9 (` and contains `rustc `.
  - The consts are non-empty in a normal build.
- **Validation**:
  - `cargo test -p nils-build-info`

## Sprint 2: Wire binary crates

**Goal**: Every binary crate that renders `--version` depends on
`nils-build-info`; `-V` shows clean semver and `--version` shows the long
string, with the existing version integration test and all required CI gates
green.

**Demo/Validation**:

- Commands:
  - `cargo test -p agent-runtime-cli`
  - `<bin> -V` and `<bin> --version` for a representative binary
  - `bash scripts/ci/completion-flag-parity-audit.sh --strict`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
- Verify: `-V` prints bare semver; `--version` prints
  `<semver> (<describe>, rustc <ver>)`; a dirty working tree shows `-dirty`;
  `agent-runtime-cli` version test stays green.

### Task 2.1: Add `long_version` to every binary's clap definition

- **Location**:
  - Each binary crate's clap command definition (the `#[command(... version
    ...)]` derive sites and the equivalent builder sites), e.g.
    `crates/cli-template/src/main.rs`, `crates/plan-tooling/src/cli.rs`, and
    the remaining required binaries.
  - The corresponding `Cargo.toml` files (add the `nils-build-info`
    dependency).
- **Description**: For each binary, keep `version` bound to the crate's own
  `env!("CARGO_PKG_VERSION")` (clean `-V`) and add `long_version =
  nils_build_info::long_version(env!("CARGO_PKG_VERSION"))` (or the const-based
  binding if a crate's clap surface requires a `&'static str`). Add the
  `nils-build-info` workspace dependency to each wired crate.
- **Dependencies**: Task 1.2
- **Complexity**: 3
- **Acceptance criteria**:
  - For a representative binary, `-V` prints only the semver and `--version`
    prints the long string including the `git describe` token and `rustc`.
  - In a dirty checkout, `--version` includes the `-dirty` suffix.
  - Every required binary in the flag-parity audit set is wired (no binary
    left rendering only the short version where the long form is expected).
- **Validation**:
  - `cargo test -p agent-runtime-cli`
  - manual `-V` / `--version` spot check on at least two binaries

### Task 2.2: Tests, completion/asset audits, and full required checks

- **Location**:
  - `crates/agent-runtime-cli/tests/integration/cli.rs` (extend or confirm)
  - completion assets if any version-bearing completion output changes
- **Description**: Confirm the existing version integration test stays green;
  add a focused assertion that `--version` contains the `git describe` token
  shape when built in a git checkout. Run the completion flag-parity and asset
  audits to confirm no binary surface regressed, then run the full required
  checks entrypoint with no `Cargo.lock` drift.
- **Dependencies**: Task 2.1
- **Complexity**: 2
- **Acceptance criteria**:
  - `agent-runtime-cli` version test passes.
  - Completion flag-parity and asset audits pass for all required binaries.
  - `nils-cli-checks-entrypoint.sh --local-fast` passes with no new dependency
    and no `Cargo.lock` drift.
- **Validation**:
  - `bash scripts/ci/completion-flag-parity-audit.sh --strict`
  - `bash scripts/ci/completion-asset-audit.sh --strict`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`

## Risks

- **R-1**: A `build.rs` that reruns on every HEAD change could cascade
  rebuilds across the workspace. Mitigation: isolate the `build.rs` in the
  `nils-build-info` leaf crate only; downstream binaries relink rather than
  recompile heavy graphs (Decision 4 in the source doc).
- **R-2**: Published builds without `.git` could fail to compile. Mitigation:
  the `build.rs` falls back to `unknown`; covered by a `.git`-less build check
  and `cargo publish --dry-run`.
- **R-3**: Per-crate version override could make a const-based `long_version`
  show the wrong semver. Mitigation: bind the semver from each crate's own
  `env!("CARGO_PKG_VERSION")` via the `long_version(pkg_version)` helper so the
  displayed release version always matches the consuming crate.
- **R-4**: Wiring ~38 binaries risks an inconsistent or missed crate.
  Mitigation: drive the wiring list from the flag-parity audit set and gate on
  the strict audits in Task 2.2.
