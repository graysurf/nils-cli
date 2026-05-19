# Plan: CLI Dispatch Modernization

## Overview

Migrate the five hand-rolled CLI dispatchers onto `clap` derive, document
the global-flag and short-flag conventions in the shared style guide, and
backfill two test gaps that ride along with the `semantic-commit`
migration. Sprint 1 lands the smallest migrations (`semantic-commit`,
`plan-tooling`) plus the style-guide additions. Sprint 2 covers
`git-summary` and `fzf-cli`. Sprint 3 takes on the biggest surface
(`git-cli`).

## Read First

- Primary source:
  docs/plans/cli-dispatch-modernization/cli-dispatch-modernization-review-source.md
- Source type: review-to-improvement-doc
- Open questions carried into execution:
  - Keep `git-cli` Groups as nested clap subcommands? Default: yes.
  - Keep `fzf-cli` interactive loops inside the handler with clap parsing only the entry args? Default: yes.
  - Remove `disable_help_flag = true` on `git-scope`/`git-lock`? Default: yes unless a load-bearing reason surfaces.

## Scope

- In scope:
  - Replace `usage.rs` / `app.rs` hand-rolled dispatch with clap derive definitions for `semantic-commit`, `plan-tooling`, `git-summary`, `fzf-cli`, `git-cli`.
  - Style-guide additions in `docs/runbooks/cli-help-style-guide.md` covering global-flag rules and `-V`/`-v`/`-h` conventions.
  - Audit and (where unjustified) remove `disable_help_flag = true` from `git-scope` and `git-lock` clap definitions.
  - New tests covering `semantic-commit --quiet` and `CAT_PAGER_ENV` behaviour.
  - Backwards-compatible flag and subcommand names — no surface changes.
- Out of scope:
  - Help-text content updates (covered by `cli-help-and-env-discoverability`).
  - JSON envelope / exit-code consolidation (covered by `cli-output-contract-unification`).
  - Adding new subcommands, removing existing ones, or renaming flags.
  - Interactive UX work inside `fzf-cli` handlers.

## Assumptions

1. The `cli-help-style-guide.md` runbook exists (owned by the
   `cli-help-and-env-discoverability` plan). This plan only adds two
   sections (global flags, short flags).
2. `clap` derive can represent nested subcommands (Group →
   Subcommand → Args) for `git-cli`.
3. Hand-rolled `usage.rs` files do not export public API used outside
   the binary.
4. The current shell completion fixtures expect specific subcommand
   names; clap-generated names must match byte-for-byte.

## Sprint 1: Style guide additions + small migrations

**Goal**: Capture the global-flag / short-flag conventions in the style
guide and migrate the two smallest hand-rolled binaries
(`semantic-commit`, `plan-tooling`). Backfill the two known test gaps
on `semantic-commit` while the binary is being touched anyway.

**Demo/Validation**:

- Commands:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
  - `cargo test -p semantic-commit`
  - `cargo test -p plan-tooling`
  - Manual: `semantic-commit --help`, `semantic-commit -V`, `semantic-commit nope` (clap parse error, exit 64), `semantic-commit --quiet commit` (suppressed output).
  - Manual: `plan-tooling --help`, `plan-tooling -V`.
- Verify: both binaries report help/version through clap; unknown
  subcommands exit 64; new tests cover `--quiet` and `CAT_PAGER_ENV`.

**PR grouping intent**: `group`
**Execution Profile**: `parallel-x2`

### Task 1.1: Style guide — global-flag and short-flag conventions

- **Location**:
  - docs/runbooks/cli-help-style-guide.md
- **Description**: Extend the style guide with two new sections:
  (a) Global flags — list the flags that MUST be `global = true`
  (`--format`, `--quiet`, `--verbose`, repo-locating flags) and the
  rule that other flags should be subcommand-scoped; (b) Short flags
  — `-V` = version, `-v` = verbose, `-h`/`--help` are clap-auto; using
  `disable_help_flag = true` requires a documented binary-wide
  rationale. Cross-link the `cli-output-contract-unification` plan
  for the JSON envelope / exit-code source.
- **Dependencies**:
  - none
- **Complexity**:
  - 2
- **Acceptance criteria**:
  - Style guide contains both new sections.
  - At least one example per section (good and bad).
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` passes.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`

### Task 1.2: Migrate `semantic-commit` to clap derive

- **Location**:
  - crates/semantic-commit/src/main.rs
  - crates/semantic-commit/src/lib.rs
  - crates/semantic-commit/src/usage.rs (delete)
  - crates/semantic-commit/src/cli.rs (new) — or fold into `lib.rs`
  - crates/semantic-commit/src/commit.rs (only where dispatch is consumed)
  - crates/semantic-commit/src/staged_context.rs (only where dispatch is consumed)
  - crates/semantic-commit/src/completion.rs (only where dispatch is consumed)
- **Description**: Replace `usage::dispatch` with a clap-derive
  `Parser` + `Subcommand` definition. Map: `staged-context` → variant,
  `commit` → variant, `completion` → variant, `help` → clap-auto.
  Preserve the exact subcommand and flag names. Route unknown
  subcommands through clap (no custom error wording). Wire
  `--version`/`-V` to clap-auto.
- **Dependencies**:
  - Task 1.1
- **Complexity**:
  - 6
- **Acceptance criteria**:
  - `semantic-commit --help`, `-V`, `help`, `staged-context`, `commit`, `completion` all match prior behaviour byte-for-byte where they can (allowing clap's default help template formatting).
  - `semantic-commit nope` exits `64` (clap default usage error).
  - Hand-rolled `usage.rs` is deleted.
  - Existing `semantic-commit` integration tests pass; the `dispatch_*` unit tests are replaced with clap parse tests.
- **Validation**:
  - `cargo test -p semantic-commit`
  - Manual: full subcommand smoke test.

### Task 1.3: Add `--quiet` and `CAT_PAGER_ENV` tests

- **Location**:
  - crates/semantic-commit/tests/integration/commit.rs
- **Description**: Add two tests. (a) `--quiet` test: run the commit
  flow with a fixture repo and assert progress / summary stderr lines
  are absent while stdout JSON (if any) and exit code remain correct.
  (b) `CAT_PAGER_ENV` test: run the commit flow twice — once with
  `GIT_PAGER=less` (expected to be overridden) and once with no
  pager set — and assert the captured stdout is identical.
- **Dependencies**:
  - Task 1.2
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - Both tests pass on the current code base.
  - Reverting either guard makes the corresponding test fail.
- **Validation**:
  - `cargo test -p semantic-commit -- --include-ignored` (or whatever runner the project conventionally uses for end-to-end commit tests; default to nextest if available).

### Task 1.4: Migrate `plan-tooling` to clap derive

- **Location**:
  - crates/plan-tooling/src/main.rs
  - crates/plan-tooling/src/usage.rs (delete)
  - crates/plan-tooling/src/lib.rs (only if dispatch is exposed)
- **Description**: Replace `usage::dispatch` with a clap-derive
  definition; preserve every subcommand (`to-json`, `validate`,
  `batches`, `artifact-audit`, `split-prs`, `scaffold`, `completion`).
  Keep the existing flag names and short flags. Route unknown
  subcommands through clap.
- **Dependencies**:
  - Task 1.1
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - `plan-tooling --help` and every existing subcommand work as before.
  - `plan-tooling nope` exits `64`.
  - Hand-rolled `usage.rs` is deleted.
  - Existing `plan-tooling` integration tests pass.
- **Validation**:
  - `cargo test -p plan-tooling`

## Sprint 2: Mid-complexity migrations

**Goal**: Migrate `git-summary` and `fzf-cli`. Both have a single
binary surface but `fzf-cli` carries an interactive event loop the
migration must avoid disturbing.

**Demo/Validation**:

- Commands:
  - `cargo test -p git-summary`
  - `cargo test -p fzf-cli`
  - Manual: `git-summary --help`, `git-summary <date-range>`, `git-summary -V`.
  - Manual: `fzf-cli --help`, `fzf-cli files`, `fzf-cli git`.
- Verify: both binaries report help/version through clap; existing
  subcommand behaviour is byte-stable.

**PR grouping intent**: `group`
**Execution Profile**: `parallel-x2`

### Task 2.1: Migrate `git-summary` to clap derive

- **Location**:
  - crates/git-summary/src/main.rs
  - crates/git-summary/src/app.rs (rewrite or delete)
  - crates/git-summary/src/cli.rs (new)
- **Description**: Replace the manual pattern-matching in `app.rs`
  with a clap derive definition. Preserve every flag and subcommand
  (`maybe_handle_completion_export` becomes a clap `completion`
  subcommand). Keep the existing default-date-range behaviour inside
  the handler.
- **Dependencies**:
  - Task 1.4
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - `git-summary --help`, `-V`, every flag, and the completion export work as before.
  - Hand-rolled `app.rs` matcher is gone (or shrunk to handler-only logic).
  - Existing tests pass.
- **Validation**:
  - `cargo test -p git-summary`

### Task 2.2: Migrate `fzf-cli` to clap derive (arg layer only)

- **Location**:
  - crates/fzf-cli/src/main.rs
  - crates/fzf-cli/src/cli.rs (new)
  - crates/fzf-cli/src/
- **Description**: Replace the hand-rolled subcommand match with a
  clap `Subcommand` enum. Keep every interactive handler unchanged —
  the migration is purely the parsing layer. Wire help/version through
  clap.
- **Dependencies**:
  - Task 1.4
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - `fzf-cli --help`, `-V`, every subcommand (files / git / processes / ports / history) work as before.
  - Existing tests pass.
- **Validation**:
  - `cargo test -p fzf-cli`

## Sprint 3: `git-cli` migration

**Goal**: Migrate the largest hand-rolled surface (`git-cli`,
~3.7k LOC across `usage.rs`, `branch.rs`, `ci.rs`, `commit.rs`,
`open.rs`, `reset.rs`). The Group abstraction stays; only the
parsing/dispatch layer moves.

**Demo/Validation**:

- Commands:
  - `cargo test -p git-cli`
  - Manual: `git-cli --help`, `git-cli utils <sub>`, `git-cli reset <args>`, `git-cli commit <args>`, `git-cli branch <args>`, `git-cli ci <args>`, `git-cli open <args>`, `git-cli -V`.
- Verify: every Group and every subcommand still works; help is
  clap-generated; unknown subcommands exit `64`.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 3.1: Model Groups as nested clap subcommands

- **Location**:
  - crates/git-cli/src/main.rs
  - crates/git-cli/src/lib.rs
  - crates/git-cli/src/usage.rs (delete)
  - crates/git-cli/src/cli.rs (new)
- **Description**: Define top-level `Cli { command: Group }` where
  `Group` is a clap `Subcommand` enum (`Utils(UtilsCommand)`,
  `Reset(ResetCommand)`, `Commit(CommitCommand)`, etc.). Each Group
  variant wraps its own `Subcommand` enum. The handler dispatch in
  `branch.rs` / `ci.rs` / `commit.rs` / `open.rs` / `reset.rs`
  receives parsed args, not raw `&[String]`.
- **Dependencies**:
  - Task 1.4
- **Complexity**:
  - 8
- **Acceptance criteria**:
  - Every existing Group / subcommand combination still works.
  - `git-cli --help` lists the Group level; `git-cli <group> --help` lists the subcommand level.
  - Unknown Group or subcommand exits `64` through clap.
  - Hand-rolled `usage.rs` is deleted.
  - All existing `git-cli` tests pass.
- **Validation**:
  - `cargo test -p git-cli`
  - Manual: smoke-test every documented `git-cli` invocation.

### Task 3.2: Audit `disable_help_flag` on `git-scope` and `git-lock`

- **Location**:
  - crates/git-scope/src/main.rs
  - crates/git-lock/src/main.rs
- **Description**: Read the surrounding code for each binary; if no
  load-bearing reason remains (the audit only found "to handle `help`
  manually", which is not load-bearing), remove
  `disable_help_flag = true` and let clap render help natively. Keep
  every other clap attribute unchanged.
- **Dependencies**:
  - Task 1.1 (style guide rule must exist)
- **Complexity**:
  - 2
- **Acceptance criteria**:
  - `disable_help_flag = true` either removed or commented with a documented binary-wide rationale.
  - `git-scope --help` and `git-lock --help` render clap-native help.
  - Existing integration tests pass.
- **Validation**:
  - `cargo test -p git-scope`
  - `cargo test -p git-lock`

## Testing Strategy

- Unit: each migrated binary keeps its existing handler tests; only
  the parsing layer changes. New clap parse tests replace the
  hand-rolled `dispatch_*` tests.
- Integration: existing per-binary `tests/integration/*.rs` carry the
  contract; nothing should change behaviour. Sprint 1 adds two new
  semantic-commit tests for `--quiet` and `CAT_PAGER_ENV`.
- Workspace: `cargo test --workspace` after each binary migrates.
- Pre-delivery: `NILS_CLI_TEST_RUNNER=nextest bash
  scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage` before
  release.

## Risks & gotchas

- `git-cli` has 5+ Groups; the nested clap structure is heavier than
  a flat Subcommand. Allow extra time for compile-time errors during
  the migration.
- Shell completion fixtures (`completions/zsh/_*`, `completions/bash/*`)
  are byte-stable contracts; the clap-generated completion script may
  change formatting slightly. Regenerate completions and audit the
  diff before merging.
- Hand-rolled help text in `usage.rs` files has minor copy that
  customers may notice missing after clap takes over. Capture the
  prior wording in commit messages so the help-style work can
  recover any wording that mattered.
- `--quiet` tests must use the shared `nils-test-support` env guards
  to avoid bleeding into other tests that consume the same temp
  directory.
- `fzf-cli`'s interactive loop reads from stdin; clap parses argv
  first, so the loop should not be disturbed if the migration stays
  at the parsing boundary only.

## Rollback plan

- Sprint 1 rollback: revert the migration commits for
  `semantic-commit` and/or `plan-tooling` per binary; the style-guide
  addition can stay or be reverted independently.
- Sprint 2 rollback (per task): each binary's migration is a separate
  PR; revert the affected PR(s) to restore hand-rolled dispatch on
  that binary.
- Sprint 3 rollback: revert the `git-cli` migration commits. The
  Group abstraction stays unchanged in the hand-rolled form; the
  binary keeps working until a follow-up migration is ready.
- `disable_help_flag` audit rollback: re-add the attribute with a
  comment naming the rationale, if removal turns out to break a
  user-facing case.
