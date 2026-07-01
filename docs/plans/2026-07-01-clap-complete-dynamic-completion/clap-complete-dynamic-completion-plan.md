# Plan: Roll out clap_complete CompleteEnv dynamic completion across CLIs

## Overview

Adopt `clap_complete` **dynamic** completion (`CompleteEnv` +
`engine::ArgValueCandidates`, behind the `unstable-dynamic` feature) so CLIs can
offer runtime-computed completion candidates (worktree names, branches, remotes,
tags, live paths) instead of only the static candidate sets `generate()` bakes
in. The cost is not the clap wiring (Phase 0 spike proved that feasible) but the
completion **framework**: audits, the shared shell adapter, tests, and the
single-completion-path policy all assume static `generate()`. The framework must
learn a first-class `completion_engine = static | dynamic` dimension (Sprint 1)
before any CLI migrates; git-cli is the pilot (Sprint 2); remaining CLIs opt in
per-CLI (Sprint 3). Static generation stays the default and is untouched for
CLIs with no runtime candidates.

## Read First

- Primary source: `docs/plans/2026-07-01-clap-complete-dynamic-completion/clap-complete-dynamic-completion-discussion-source.md`
- Source type: existing issue/spec
- Open questions carried into execution: none

## Scope

- In scope: a `static | dynamic` completion-engine dimension across the
  completion matrix, the three completion CI audits, the shared zsh/bash runtime
  adapter, the zsh completion test, the contract template, and the completion
  development standard; git-cli's migration to `CompleteEnv` with live worktree
  candidates; opt-in per-CLI rollout thereafter.
- Out of scope: forcing dynamic mode on CLIs without runtime candidates; changing
  agent-facing behavior (completion is invisible to agents; `.complete()` is
  zero-cost when idle); replacing the static `generate()` path.

## Assumptions

1. `clap_complete` dynamic registration is unstable; the shell stub and the
   binary stay version-matched via the exact `clap_complete =4.6.5` pin and
   Homebrew shipping stub + binary from one tarball.
2. The workspace already carries `shlex 2.0.1` (via `cc` -> `cmake` ->
   `aws-lc-sys`); `clap_complete`'s `unstable-dynamic` needs `shlex ^1`, so the
   two versions cannot unify and `deny.toml multiple-versions = "deny"` requires
   a `shlex@1.3.0` skip entry.
3. sympoies/nils-cli GitHub branch protection has 0 required checks, so PR
   delivery self-gates: watch CI green, then merge (do not rely on `pr deliver`
   blocking on checks).
4. The test-first gate is enabled globally (`[test_first].require = true`), so
   `--kind feature` PRs in this plan require verified test-first evidence.

## Sprint 1: Framework learns a "dynamic" completion engine mode

**Goal**: The completion framework recognizes `completion_engine = static |
dynamic` as a first-class dimension; static CLIs are unaffected and a synthetic
`dynamic` fixture passes every audit and test. No CLI behavior change ships.
**Demo/Validation**:

- Command(s): `bash scripts/ci/completion-freshness-audit.sh --strict`;
  `bash scripts/ci/completion-flag-parity-audit.sh`;
  `bash scripts/ci/completion-asset-audit.sh`;
  `zsh -f tests/zsh/completion.test.zsh`; `cargo deny check`;
  `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`.
- Verify: existing static CLIs unchanged; the synthetic dynamic fixture is
  classified as dynamic (freshness-skip, flag-parity provider-assert, asset
  registration-shape) and its registration shape + alias family assert in the
  zsh test; `cargo deny check` passes with the shlex skip entry.

### Task 1.1: Add the completion_engine dimension to matrix + contract metadata

- **Location**:
  - `docs/specs/completion-coverage-matrix-v1.md`
  - `docs/specs/completion-contract-template.md`
- **Description**: Add a `Completion engine` value (`static` default, `dynamic`
  opt-in) to the coverage matrix per-CLI rows and policy notes, and a
  `completion_engine=<static|dynamic>` field to the contract template metadata
  tuple and enforcement table. All existing rows are `static`.
- **Dependencies**:
  - none
- **Complexity**: 3
- **Acceptance criteria**:
  - The matrix declares an engine value for every required binary; the audits
    and the zsh test parse it deterministically.
  - The contract template documents the field and its enforcement check.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`

### Task 1.2: Enable the unstable clap features and gate the dependency footprint

- **Location**:
  - `Cargo.toml`
  - `deny.toml`
- **Description**: Add `unstable-ext` to the workspace `clap` features (needed
  for `Arg::add`) and `unstable-dynamic` to `clap_complete`. Add a
  `{ crate = "shlex@1.3.0", reason = "clap_complete unstable-dynamic" }` skip to
  the `deny.toml` duplicate-version list, and confirm `is_executable 1.0.6`
  (pulled by `unstable-dynamic`) passes the license/source policy
  (`MIT OR Apache-2.0`).
- **Dependencies**:
  - none
- **Complexity**: 2
- **Acceptance criteria**:
  - `cargo build` and `cargo deny check` pass with the new features and skip
    entry; no other duplicate-version regressions.
  - The feature unification is noted (harmless across all clap users).
- **Validation**:
  - `cargo deny check`; `cargo build`

### Task 1.3: Teach completion-flag-parity-audit the dynamic mode

- **Location**:
  - `scripts/ci/completion-flag-parity-audit.sh`
- **Description**: For `dynamic` CLIs, stop parsing a static flag list from the
  committed asset (there is none — the asset is a `CompleteEnv` registration
  stub); instead assert that flags / value providers exist in the command model.
  Preserve current static-CLI behavior exactly.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 5
- **Acceptance criteria**:
  - A synthetic `dynamic` fixture passes flag-parity via the provider-assert
    path; static CLIs still parse and enforce their flag lists.
- **Validation**:
  - `bash scripts/ci/completion-flag-parity-audit.sh`

### Task 1.4: Teach completion-freshness and asset audits the dynamic mode

- **Location**:
  - `scripts/ci/completion-freshness-audit.sh`
  - `scripts/ci/completion-asset-audit.sh`
  - `scripts/ci/tests/completion-freshness-audit.test.sh`
- **Description**: A `dynamic` CLI stays classified as a runtime adapter
  (freshness-skip: no static baseline to diff) and its committed registration
  asset passes the asset matrix (registration shape, not full static script).
- **Dependencies**:
  - Task 1.1
- **Complexity**: 5
- **Acceptance criteria**:
  - The synthetic dynamic fixture is skipped by freshness and accepted by the
    asset audit; static CLIs are still diffed and shape-checked as today.
- **Validation**:
  - `bash scripts/ci/completion-freshness-audit.sh --strict`;
    `bash scripts/ci/completion-asset-audit.sh`;
    `bash scripts/ci/tests/completion-freshness-audit.test.sh`

### Task 1.5: Extend the shared runtime adapter for CompleteEnv + alias rewrite

- **Location**:
  - `completions/zsh/_completion-adapter-common.zsh`
  - `completions/bash/completion-adapter-common.bash`
- **Description**: Support the `CompleteEnv` registration shape
  (`_clap_dynamic_completer_<bin>` / `complete -F`) and its callback protocol,
  plus alias rewrite so alias families (e.g. `gx*`/`gxw`) complete via the
  underlying binary. Keep the fail-closed contract; do not add an alternate
  dispatch path for static CLIs.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 6
- **Acceptance criteria**:
  - New dynamic register/load helpers exist alongside the static ones; static
    adapters are byte-identical in behavior; helpers fail closed on load
    failure.
- **Validation**:
  - `zsh -n completions/zsh/_completion-adapter-common.zsh`;
    `bash -n completions/bash/completion-adapter-common.bash`;
    `zsh -f tests/zsh/completion.test.zsh`

### Task 1.6: Cover the dynamic registration shape in the zsh completion test

- **Location**:
  - `tests/zsh/completion.test.zsh`
- **Description**: Handle the dynamic registration shape and alias-family
  registration for a synthetic `dynamic` CLI: assert the registration markers
  and alias coverage rather than a static `_<cli>` function symbol. Add the
  synthetic dynamic fixture assets the test drives.
- **Dependencies**:
  - Task 1.1
  - Task 1.5
- **Complexity**: 5
- **Acceptance criteria**:
  - The zsh test asserts dynamic registration + alias-family for the fixture and
    still asserts the static function shape for static CLIs.
- **Validation**:
  - `zsh -f tests/zsh/completion.test.zsh`

### Task 1.7: Document dynamic mode in the completion development standard

- **Location**:
  - `docs/runbooks/cli-completion-development-standard.md`
- **Description**: Document the `dynamic` mode and its metadata so the
  single-completion-path policy does not read `CompleteEnv` as a forbidden
  alternate dispatch. Clarify that dynamic mode extends (does not replace) the
  clap-first baseline and remains a single path per CLI.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 3
- **Acceptance criteria**:
  - The standard names `completion_engine`, states the enforcement, and
    reconciles it with the single-path policy and the metadata tuple.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`

## Sprint 2: git-cli pilot (first real, audited, released migration)

**Goal**: git-cli completes `worktree go`/`worktree remove` targets from the
live `git worktree list` via `CompleteEnv` + `ArgValueCandidates`, superseding
the `gxwcd` static workaround, with full CI green and a released patch verified
on a real install.
**Demo/Validation**:

- Command(s): `git-cli worktree go <TAB>` on a real zsh + bash install (incl.
  `gxw` alias completion); full CI (test / test_macos / coverage);
  completion audits under the dynamic mode.
- Verify: TAB yields live worktree names/branches; alias completion works;
  static idle path unchanged; committed registration assets pass audits.

### Task 2.1: Wire CompleteEnv into git-cli main dispatch

- **Location**:
  - `crates/git-cli/src/cli.rs`
  - `crates/git-cli/src/main.rs`
- **Description**: Call `CompleteEnv::with_factory(build_command_model).complete()`
  before the hand-rolled dispatch so `COMPLETE=<shell>` invocations short-circuit
  into dynamic completion; the normal app path is unchanged when `COMPLETE` is
  unset.
- **Dependencies**:
  - Task 1.1
  - Task 1.2
  - Task 1.5
  - Task 1.7
- **Complexity**: 5
- **Acceptance criteria**:
  - Idle runs behave exactly as today; `COMPLETE=zsh git-cli` emits the dynamic
    registration; no regression in hand-rolled parsing.
- **Validation**:
  - `cargo test -p nils-git-cli`; manual `COMPLETE=zsh git-cli` smoke

### Task 2.2: Attach live worktree candidates to worktree go/remove

- **Location**:
  - `crates/git-cli/src/completion.rs`
- **Description**: Attach an `ArgValueCandidates` closure (shelling out to
  `git worktree list --porcelain`, yielding worktree basenames + branch names)
  to the `worktree go` and `worktree remove` target args, replacing the
  `ValueHint::AnyPath` placeholder.
- **Dependencies**:
  - Task 2.1
- **Complexity**: 4
- **Acceptance criteria**:
  - `git-cli worktree go <prefix><TAB>` prefix-filters live worktree/branch
    names; `worktree remove` behaves likewise.
- **Validation**:
  - manual TAB verification; `cargo test -p nils-git-cli`

### Task 2.3: Emit CompleteEnv registration + update committed assets and aliases

- **Location**:
  - `crates/git-cli/src/completion.rs`
  - `completions/zsh/_git-cli`
  - `completions/bash/git-cli`
  - `completions/zsh/aliases.zsh`
  - `completions/bash/aliases.bash`
  - `completions/zsh/_gxwcd`
- **Description**: Switch `git-cli completion <shell>` to emit the `CompleteEnv`
  registration; regenerate the committed zsh/bash assets; reconcile gx*/gxw alias
  wiring; keep `gxwcd` for cd-on-select ergonomics but note the static workaround
  is superseded.
- **Dependencies**:
  - Task 2.1
  - Task 2.2
- **Complexity**: 6
- **Acceptance criteria**:
  - Committed assets match the emitted registration; alias families complete via
    the binary; completion audits pass under dynamic mode.
- **Validation**:
  - `zsh -n completions/zsh/_git-cli`; `bash -n completions/bash/git-cli`;
    completion audits; `zsh -f tests/zsh/completion.test.zsh`

### Task 2.4: Full CI, release patch, and real-install verification

- **Location**:
  - `crates/git-cli/Cargo.toml`
- **Description**: Land the pilot with full CI green (test / test_macos /
  coverage) and completion audits under the dynamic mode, bump the git-cli patch
  version, release it (the release skill drives the workspace bump + homebrew
  tap), and verify on a real install (zsh + bash, including alias completion).
- **Dependencies**:
  - Task 2.3
- **Complexity**: 4
- **Acceptance criteria**:
  - Full CI green; release shipped; manual real-install TAB verification passes
    in both shells.
- **Validation**:
  - `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh`;
    release + install verification

## Sprint 3: Roll out to remaining CLIs

**Goal**: Per-CLI opt-in dynamic value providers where they help, migrated
CLI-by-CLI with per-CLI audit + release validation; static `generate()` remains
for CLIs with no runtime candidates.
**Demo/Validation**:

- Command(s): per-CLI completion audits + manual TAB verification; release
  validation per migrated CLI.
- Verify: each migrated CLI's dynamic candidates complete correctly; unmigrated
  CLIs are byte-identical; dynamic mode stays opt-in.

### Task 3.1: Inventory and add per-CLI dynamic value providers

- **Location**:
  - `crates/git-lock/src/completion.rs`
  - `crates/plan-issue/src/completion.rs`
  - `crates/agent-workflow-primitives/src/completion.rs`
- **Description**: Where they help, add dynamic providers: branches
  (`git for-each-ref`), remotes (`git remote`), tags (`git tag`), worktrees,
  live paths, and CLI-specific enumerations (e.g. `api-*` targets). Record which
  CLIs have no runtime candidates and stay static. The listed crates are the
  first inventory anchors; the full candidate set is enumerated during the task.
- **Dependencies**:
  - Task 2.4
- **Complexity**: 6
- **Acceptance criteria**:
  - A per-CLI decision list (dynamic vs stay-static) exists; providers added
    only where they add value.
- **Validation**:
  - per-CLI `cargo test`; completion audits

### Task 3.2: Migrate CLI-by-CLI with per-CLI audit + release validation

- **Location**:
  - `docs/specs/completion-coverage-matrix-v1.md`
  - `docs/specs/completion-contract-template.md`
- **Description**: Migrate each opting-in CLI (optionally in parallel lanes) with
  a per-CLI migration contract copied from the template, audit under dynamic
  mode, and release validation. Update each CLI's matrix engine value. Keep
  static `generate()` for CLIs with no runtime candidates.
- **Dependencies**:
  - Task 3.1
- **Complexity**: 6
- **Acceptance criteria**:
  - Each migrated CLI has a filled contract, green audits, and release
    validation; the matrix reflects each CLI's engine.
- **Validation**:
  - full CI per CLI; per-CLI completion audit + manual verification

## Testing Strategy

- Unit: Rust completion-model tests per migrated CLI (`cargo test -p <crate>`);
  `ArgValueCandidates` closure behavior.
- Integration: the three completion CI audits (freshness, flag-parity, asset)
  exercised against a synthetic `dynamic` fixture and all static CLIs;
  `zsh -f tests/zsh/completion.test.zsh` for registration + alias families;
  `cargo deny check` for the dependency footprint.
- E2E/manual: `git-cli worktree go <TAB>` on real zsh + bash installs, including
  alias completion, after the pilot release.

## Risks & gotchas

- Unstable interface: `clap_complete` dynamic registration is explicitly
  unstable; the shell stub and binary must stay version-matched (re-source on
  upgrade). Mitigated by the exact `=4.6.5` pin and Homebrew shipping stub +
  binary from one tarball; revisit on every `clap_complete` bump.
- Dependency footprint: `+shlex 1.3.0` (duplicate, skip-listed) and
  `+is_executable`; small but real supply-area growth gated by
  `unstable-dynamic`.
- Single-path policy collision: the completion development standard forbids
  alternate completion dispatch; dynamic mode must be documented as a first-class
  engine that extends the clap-first baseline, not an alternate dispatch, or the
  policy grep checks will read it as a violation.
- Agent-neutrality: completion is invisible to agents; `.complete()` is
  zero-cost when idle, so no agent-facing behavior changes.

## Rollback plan

- Sprint 1 is additive and behind a data dimension defaulting to `static`;
  reverting the framework PR restores the pre-dynamic audits/adapter/policy with
  no CLI affected.
- Sprint 2 is a single released CLI; revert git-cli's `main`/completion changes
  and regenerate static assets to restore static completion; `gxwcd` remains the
  worktree workaround.
- Sprint 3 is per-CLI and opt-in; revert an individual CLI's provider changes
  and regenerate its static assets without touching other CLIs.
