# Plan: `semantic-commit` Agent Commit Flow

## Overview

Expand `semantic-commit` so it remains the audited agent-facing commit
surface for normal commit creation, amend, message-edit, structured
message, trailer, JSON result, and fixup/squash workflows. The work keeps
the existing staged-only default and deliberately avoids generic git
passthrough, hook bypasses, push, force-push, rebase, and implicit
staging.

This is a user-facing CLI behavior change with tests, completions, and
refreshed documentation. The implementation should preserve existing
message validation behavior unless a new mode explicitly opts out because
Git itself defines non-semantic subjects such as `fixup!`.

## Read First

- Primary source:
  docs/plans/semantic-commit-agent-flow/semantic-commit-agent-flow-discussion-source.md
- Source type: discussion-to-implementation-doc
- Open questions carried into execution: none

## Scope

- In scope:
  - `semantic-commit commit --amend`.
  - `semantic-commit commit --amend --no-edit`.
  - `semantic-commit commit --amend --message-only`.
  - `semantic-commit commit --format json` and `--json`.
  - Explicit guard flags for agent safety, including no-unstaged,
    allow-empty, and expected-HEAD checks when they fit the local code
    shape.
  - `--signoff` and repeatable `--trailer <token: value>` handling.
  - Structured message assembly fields such as `--type`, `--scope`,
    `--subject`, and repeatable body bullet fields.
  - Separate `semantic-commit fixup` and `semantic-commit squash`
    subcommands.
  - Unit and integration coverage for new behavior and regression
    boundaries.
  - Regenerated bash and zsh completion assets.
  - A complete review and rewrite of `semantic-commit` documentation
    surfaces, including crate README and crate docs index.
- Out of scope:
  - Raw git passthrough flags.
  - A generic `--no-verify` bypass.
  - Push, force-push, rebase, or PR delivery behavior.
  - Implicit staging, autostage, or hidden `git add -A`.
  - Changes to unrelated CLI crates.

## Assumptions

1. Existing `semantic-commit` integration test helpers can create enough
   temporary git repositories to cover create, amend, message-only amend,
   fixup, squash, and JSON output behavior.
2. The local fast check entrypoint is sufficient final validation for this
   crate-scoped behavior change, with additional package tests and shell
   completion syntax checks run during development.
3. `fixup` and `squash` should intentionally bypass Semantic Commit
   header validation while still preserving staged-change checks,
   optional guard flags, and JSON result output.

## Sprint 1: Design and implement commit-mode expansion

**Goal**: Add agent-safe amend, message assembly, trailer, guard, and JSON
result support to `semantic-commit commit`.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-semantic-commit commit`
  - `semantic-commit commit --help`
  - `semantic-commit completion zsh`
  - `semantic-commit completion bash`
- Verify:
  - `--amend`, `--no-edit`, `--message-only`, `--format json`, `--json`,
    guard flags, `--signoff`, `--trailer`, and structured message fields
    appear in help and completion metadata.
  - Amend and message-only amend run through validation and do not require
    direct `git commit --amend` use by agents.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 1.1: Model commit operations and parser options

- **Location**:
  - `crates/semantic-commit/src/commit.rs`
  - `crates/semantic-commit/src/completion.rs`
- **Description**: Extend commit options with operation mode, JSON output
  mode, guard flags, trailer/signoff fields, and structured message
  fields. Reject ambiguous combinations such as `--no-edit` with explicit
  message input.
- **Dependencies**: none
- **Complexity**: 3
- **Acceptance criteria**:
  - Parser rejects incompatible options with actionable errors.
  - Existing options retain current behavior.
  - Help and completion metadata expose the new flags.
- **Validation**:
  - `cargo test -p nils-semantic-commit commit_help_includes`
  - `semantic-commit commit --help`

### Task 1.2: Implement amend, no-edit, and message-only amend

- **Location**:
  - `crates/semantic-commit/src/commit.rs`
  - `crates/semantic-commit/tests/integration/commit.rs`
- **Description**: Implement `--amend` using validated message input,
  `--amend --no-edit` using the previous commit message, and
  `--amend --message-only` for message-only HEAD edits without staged
  changes.
- **Dependencies**: Task 1.1
- **Complexity**: 3
- **Acceptance criteria**:
  - New amend commits preserve staged-only behavior unless
    `--message-only` or `--allow-empty` explicitly changes the staged
    requirement.
  - `--no-edit` preserves the HEAD message and does not require stdin.
  - `--message-only` rejects staged content unless the final design
    deliberately documents how it combines with staged changes.
- **Validation**:
  - `cargo test -p nils-semantic-commit amend`

### Task 1.3: Add JSON commit result output

- **Location**:
  - `crates/semantic-commit/src/commit.rs`
  - `docs/specs/cli-output-contract-v1.md` if a new contract note is
    needed
  - `crates/semantic-commit/tests/integration/commit.rs`
- **Description**: Add a versioned JSON result for create, amend,
  message-only amend, dry-run, and validation-only flows where appropriate.
  Include operation, dry-run status, commit SHA and subject when a commit
  exists, and enough staged file summary for agents to avoid follow-up
  shell parsing.
- **Dependencies**: Task 1.2
- **Complexity**: 3
- **Acceptance criteria**:
  - JSON output is valid, snake_case, and versioned.
  - Successful live commit/amend output includes the resulting HEAD SHA.
  - `--quiet` and JSON output have deterministic precedence.
- **Validation**:
  - `cargo test -p nils-semantic-commit json`

### Task 1.4: Add guard flags, trailers, signoff, and structured message assembly

- **Location**:
  - `crates/semantic-commit/src/commit.rs`
  - `crates/semantic-commit/tests/integration/commit.rs`
- **Description**: Add explicit guard and message-construction features so
  agents can express common safety and formatting needs without hand-built
  message files or direct git fallback.
- **Dependencies**: Task 1.3
- **Complexity**: 4
- **Acceptance criteria**:
  - Guard flags fail before commit mutation when their condition is not
    met.
  - `--signoff` and repeatable `--trailer` append validated trailers.
  - Structured fields generate the same message shape accepted by normal
    validation.
  - Explicit message input and structured fields are mutually exclusive
    unless the final design has a documented merge rule.
- **Validation**:
  - `cargo test -p nils-semantic-commit guard`
  - `cargo test -p nils-semantic-commit trailer`
  - `cargo test -p nils-semantic-commit structured`

## Sprint 2: Add fixup/squash commit subcommands

**Goal**: Let agents create review-cleanup commits without dropping to
direct `git commit --fixup` or `git commit --squash`.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-semantic-commit fixup`
  - `cargo test -p nils-semantic-commit squash`
  - `semantic-commit fixup --help`
  - `semantic-commit squash --help`
- Verify:
  - Subcommands create `fixup!` and `squash!` subjects from a validated
    target revision.
  - Subcommands support staged checks, dry-run, JSON output, repo
    override, and quiet/no-progress behavior where relevant.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 2.1: Add fixup and squash dispatch

- **Location**:
  - `crates/semantic-commit/src/cli.rs`
  - `crates/semantic-commit/src/commit.rs` or a new focused module
  - `crates/semantic-commit/src/completion.rs`
- **Description**: Add `fixup` and `squash` subcommands with shared
  target revision resolution and staged-change validation.
- **Dependencies**: Task 1.4
- **Complexity**: 3
- **Acceptance criteria**:
  - `semantic-commit fixup --target <rev>` creates a `fixup!` commit.
  - `semantic-commit squash --target <rev>` creates a `squash!` commit.
  - Invalid targets fail without creating a commit.
- **Validation**:
  - `cargo test -p nils-semantic-commit fixup`
  - `cargo test -p nils-semantic-commit squash`

### Task 2.2: Align fixup/squash JSON and guard behavior

- **Location**:
  - `crates/semantic-commit/src/commit.rs` or new focused module
  - `crates/semantic-commit/tests/integration/commit.rs`
- **Description**: Share the JSON result and guard behavior with the
  regular commit path where that behavior is meaningful.
- **Dependencies**: Task 2.1
- **Complexity**: 2
- **Acceptance criteria**:
  - JSON output includes operation, target revision, resulting SHA, and
    generated subject.
  - Guard failures occur before mutation.
  - Dry-run validates target and staged state without committing.
- **Validation**:
  - `cargo test -p nils-semantic-commit fixup_json`
  - `cargo test -p nils-semantic-commit squash_json`

## Sprint 3: Refresh docs, completions, and full validation

**Goal**: Keep the `semantic-commit` documentation current with the new
agent-oriented behavior and finish with repo-standard validation.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-semantic-commit`
  - `zsh -n completions/zsh/_semantic-commit`
  - `bash -n completions/bash/semantic-commit`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
- Verify:
  - Generated completions include all new flags and subcommands.
  - Crate README documents the intended agent workflow, examples, exit
    codes, and non-goals.
  - Crate docs index points to the current README and any new detailed
    doc surface.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 3.1: Regenerate completion assets

- **Location**:
  - `completions/zsh/_semantic-commit`
  - `completions/bash/semantic-commit`
- **Description**: Regenerate bash and zsh completion assets after CLI
  metadata changes.
- **Dependencies**: Task 2.2
- **Complexity**: 1
- **Acceptance criteria**:
  - Completion assets include new subcommands and flags.
  - Shell syntax checks pass.
- **Validation**:
  - `zsh -n completions/zsh/_semantic-commit`
  - `bash -n completions/bash/semantic-commit`

### Task 3.2: Rewrite `semantic-commit` documentation

- **Location**:
  - `crates/semantic-commit/README.md`
  - `crates/semantic-commit/docs/README.md`
  - Related root docs only if they contain stale `semantic-commit`
    behavior claims
- **Description**: Review every active `semantic-commit` documentation
  surface and rewrite stale content so it explains the current agent
  workflow, command modes, examples, JSON output, safety guardrails,
  non-goals, and exit codes.
- **Dependencies**: Task 3.1
- **Complexity**: 3
- **Acceptance criteria**:
  - No active doc surface claims `semantic-commit` only creates new
    commits.
  - Docs distinguish Semantic Commit validation from fixup/squash modes.
  - Docs avoid local machine paths and keep repository text in English.
- **Validation**:
  - `rg -n "semantic-commit|fixup|squash|amend" README.md docs crates/semantic-commit`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`

### Task 3.3: Run final validation and prepare delivery

- **Location**:
  - Entire changed scope
- **Description**: Run focused tests during implementation and finish with
  the local fast check entrypoint. Record any validation caveats in the
  execution state before PR delivery.
- **Dependencies**: Task 3.2
- **Complexity**: 2
- **Acceptance criteria**:
  - Focused `nils-semantic-commit` tests pass.
  - Completion syntax checks pass.
  - `--local-fast` passes or any failure is documented with exact blocker
    evidence.
- **Validation**:
  - `cargo test -p nils-semantic-commit`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
