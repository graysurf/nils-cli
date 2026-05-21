# Plan: CLI Destructive Operation Safety

## Overview

Harden the destructive command surfaces in three sprints by owner.
Sprint 1 ships the `git-lock` safety pack (highest blast radius).
Sprint 2 ships the `memo-cli` safety pack (most user confusion).
Sprint 3 ships the `heuristic-inbox` and `codex-cli` consistency fixes.
A shared TTY-prompt helper is introduced once the third call site
appears, not before.

## Read First

- Primary source: docs/plans/cli-destructive-operation-safety/cli-destructive-operation-safety-review-source.md
- Source type: review-to-improvement-doc
- Open questions carried into execution:
  - For `memo-cli delete`: redefine `--hard` as a real mode (soft vs. hard) or remove? Default: redefine.
  - For `git-lock unlock --dry-run`: summary table vs. raw `git diff`? Default: summary table, `--verbose` for full diff.
  - Should the TTY-prompt helper land in `nils-term`? Default: yes when the third call site appears.

## Scope

- In scope:
  - `git-lock unlock` gets `--dry-run` and a richer prompt; `git-lock delete` gets a warning preface and
    `--force`; `git-lock` error catch-all gains remediation hints.
  - `memo-cli delete` gets TTY-aware confirmation + `--yes`; `--hard` is reinterpreted as a real mode flag
    (soft vs. hard) with a documented migration; `memo-cli apply` learns to fail fast on TTY-stdin and gain a diff
    preview in dry-run.
  - `heuristic-inbox archive` gains TTY-aware confirmation + `--yes`.
  - `codex-cli` gets parse-time `requires` on `--watch` and a strict usage-error path for non-interactive `--yes`-less removal.
  - A shared TTY-prompt helper in `nils-term` once three call sites exist.
- Out of scope:
  - Renaming subcommands.
  - Output contract changes (delegated to `cli-output-contract-unification`).
  - Help-text style work (delegated to `cli-help-and-env-discoverability`).
  - Adding new subcommands.

## Assumptions

1. `nils-common::cli_contract::exit::USAGE` is available (from the
   `cli-output-contract-unification` plan) by the time Sprint 1 runs.
   If not, this plan defines a local `USAGE_EXIT_CODE` constant and
   migrates to the shared constant on its next touch.
2. `is_terminal()` from `std::io` is reliable enough for TTY
   detection (already used elsewhere in the workspace).
3. `memo-cli delete --hard` is not yet used heavily by automation;
   the redefinition is acceptable as a breaking change for one minor
   version.
4. The audit's "TTY-prompt helper in `nils-term`" decision is owned
   by this plan, not by `cli-ux-progress-and-defaults`.

## Sprint 1: `git-lock` safety pack

**Goal**: Make `git-lock unlock` and `git-lock delete` safe by default
and improve error context.

**Demo/Validation**:

- Commands:
  - `cargo test -p git-lock`
  - Manual: `git-lock unlock --dry-run <label>` (no state change; summary printed).
  - Manual: `git-lock unlock <label>` (prompt names the hash and counts the changed files; y/N).
  - Manual: `git-lock delete <label>` (prompt prefaced with WARNING).
  - Manual: `git-lock delete --force <label>` (no prompt; non-interactive bypass).
  - Manual: `git-lock unlock nonexistent` (error message names the fix: "run `git-lock list`").
- Verify: dry-run never mutates state; delete prompt is clearly
  marked destructive; error hints surface when known errors fire.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 1.1: Add `--dry-run` to `git-lock unlock`

- **Location**:
  - crates/git-lock/src/main.rs
  - crates/git-lock/src/unlock.rs
  - crates/git-lock/tests/integration/unlock.rs
- **Description**: Add a clap `--dry-run` flag on the `unlock`
  subcommand. When set, compute the target hash and emit a per-file
  summary of what `git reset --hard` would change (use
  `nils-common::process::run_output` to call `git diff --stat HEAD
  <hash>`). Skip the reset and skip the prompt. Without `--dry-run`,
  augment the existing prompt with the same summary so users see what
  they are agreeing to. Add `--verbose` (or `--show-diff`) to display
  the full diff before the prompt.
- **Dependencies**:
  - none
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - `git-lock unlock --dry-run <label>` exits `0`, prints a summary, leaves the working tree untouched.
  - `git-lock unlock <label>` prompt names the hash and lists the changed file count.
  - `git-lock unlock --verbose <label>` includes the full `git diff` in the prompt.
  - New integration test asserts dry-run leaves state untouched and summary line count is non-zero when the target hash differs.
- **Validation**:
  - `cargo test -p git-lock unlock`

### Task 1.2: Strengthen `git-lock delete` warning and add `--force`

- **Location**:
  - crates/git-lock/src/main.rs
  - crates/git-lock/src/delete.rs
  - crates/git-lock/tests/integration/delete.rs
- **Description**: Preface the existing prompt with `WARNING: this
  permanently deletes the lock record from disk`. Add a clap
  `--force` flag that bypasses the prompt (required for
  non-interactive contexts). When stdin is not a TTY and `--force`
  is absent, fail fast with a usage error (exit `64`).
- **Dependencies**:
  - none
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - Interactive delete prompt contains the WARNING line.
  - `git-lock delete --force <label>` removes without prompting.
  - Non-interactive delete without `--force` exits `64`.
  - New integration test covers the three branches above.
- **Validation**:
  - `cargo test -p git-lock delete`

### Task 1.3: Wrap `git-lock` errors with remediation hints

- **Location**:
  - crates/git-lock/src/main.rs
  - crates/git-lock/src/errors.rs (new) — or extend existing error type
  - crates/git-lock/tests/integration/main.rs
- **Description**: Replace the catch-all `eprintln!("{err:#}")` with a
  formatter that recognises common error variants ("label not found",
  "ambiguous label", "git command failed") and appends a one-line
  remediation hint ("run `git-lock list`" / "use `git-lock unlock
  --label <full-name>`" / "ensure `git` is installed and the cwd is a
  git repo"). Keep the underlying error message visible.
- **Dependencies**:
  - none
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - Each recognised variant emits its expected hint.
  - Unrecognised errors fall through to the prior formatter behaviour.
  - One integration test per variant.
- **Validation**:
  - `cargo test -p git-lock`

## Sprint 2: `memo-cli` safety pack

**Goal**: Make `memo-cli delete` interactive-by-default,
`memo-cli apply --stdin` fail fast on TTY, and `memo-cli apply
--dry-run` show the actual changes.

**Demo/Validation**:

- Commands:
  - `cargo test -p memo-cli`
  - Manual: `memo-cli delete itm_XXXX` on a TTY (prompt asks for confirmation).
  - Manual: `memo-cli delete itm_XXXX --yes` (no prompt; deletes).
  - Manual: `memo-cli delete itm_XXXX` piped (no TTY; usage error).
  - Manual: `memo-cli apply --stdin` on a TTY (usage error: "requires piped input").
  - Manual: `cat payload.json | memo-cli apply --stdin --dry-run` (prints change preview).
- Verify: delete and apply now match the documented safety profile.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 2.1: Redefine `memo-cli delete` semantics

- **Location**:
  - crates/memo-cli/src/cli.rs
  - crates/memo-cli/src/commands/delete.rs
  - crates/memo-cli/tests/integration/delete.rs
- **Description**: Reinterpret `--hard` as a true mode flag (soft
  archive when absent, hard delete when present). Default behaviour
  on a TTY prompts for confirmation; `--yes` bypasses the prompt for
  non-interactive use; non-interactive without `--yes` fails fast
  with exit `64`. Document the soft-archive behaviour in the
  subcommand help. If soft-archive is not implementable in this
  pass, retain the prior `--hard`-required behaviour but emit a
  deprecation message and route the prompt through the new TTY
  helper anyway.
- **Dependencies**:
  - none
- **Complexity**:
  - 6
- **Acceptance criteria**:
  - TTY delete asks "Permanently delete itm_XXXX? [y/N]" by default.
  - `--yes` skips the prompt and deletes.
  - Non-interactive `memo-cli delete` without `--yes` exits `64`.
  - `--hard` either toggles a real soft/hard mode or emits a clear deprecation message routing the user to `--yes`.
  - New integration tests cover all four branches.
- **Validation**:
  - `cargo test -p memo-cli delete`

### Task 2.2: Fail fast on `memo-cli apply --stdin` with TTY stdin

- **Location**:
  - crates/memo-cli/src/cli.rs
  - crates/memo-cli/src/commands/apply.rs
  - crates/memo-cli/tests/integration/apply.rs
- **Description**: Before calling `io::stdin().read_to_string`,
  detect `is_terminal()` and exit `64` with a usage message:
  `"--stdin requires piped input; use --input <file> or pipe content
  into stdin"`. Add an integration test that drives the binary with
  a fake TTY (or simply verifies the error path when no input is
  piped within a bounded wait).
- **Dependencies**:
  - none
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - `memo-cli apply --stdin` with no piped input exits `64` quickly (no indefinite block).
  - `cat payload.json | memo-cli apply --stdin` still works.
  - New integration test covers the TTY-stdin path.
- **Validation**:
  - `cargo test -p memo-cli apply`

### Task 2.3: Add change preview to `memo-cli apply --dry-run`

- **Location**:
  - crates/memo-cli/src/commands/apply.rs
  - crates/memo-cli/src/output/json.rs
  - crates/memo-cli/src/output/text.rs
  - crates/memo-cli/tests/integration/apply.rs
- **Description**: When `--dry-run` is set, walk the parsed payload
  and produce a `changes: [{item_id, field, old, new}]` array.
  Include it in JSON output unconditionally; include in text output
  truncated to the first N changes (e.g. 10) with a "+N more" tail.
  Keep the existing summary counts.
- **Dependencies**:
  - Task 2.2 (so the apply tests cluster in one PR)
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - JSON dry-run output contains a `changes` array.
  - Text dry-run output lists the first N changes plus a tail count when truncated.
  - New integration test covers both branches.
- **Validation**:
  - `cargo test -p memo-cli apply`

## Sprint 3: `heuristic-inbox` + `codex-cli` consistency

**Goal**: Add confirmation to `heuristic-inbox archive`, fail
`codex-rate-limits --watch` without `--async` at parse time, and
treat `codex-remove` non-interactive without `--yes` as a usage
error.

**Demo/Validation**:

- Commands:
  - `cargo test -p agent-workflow-primitives`
  - `cargo test -p codex-cli`
  - Manual: `heuristic-inbox archive <entry>` on a TTY (prompt; y/N).
  - Manual: `heuristic-inbox archive --yes <entry>` (no prompt).
  - Manual: `codex-rate-limits --watch` (parse-time error: requires `--async`).
  - Manual: `codex-remove <target>` non-interactive without `--yes` (exit `64`).
- Verify: each surface matches the safety pattern used by Sprint 1
  and Sprint 2.

**PR grouping intent**: `group`
**Execution Profile**: `parallel-x3`

### Task 3.1: TTY confirmation for `heuristic-inbox archive`

- **Location**:
  - crates/agent-workflow-primitives/src/heuristic_inbox.rs
  - crates/agent-workflow-primitives/tests/integration/heuristic_inbox.rs
- **Description**: Add a TTY-aware confirmation gate in front of the
  archive move (mirroring the `git-lock delete` shape). Add `--yes`
  to bypass the prompt; non-interactive without `--yes` fails with
  exit `64`. Keep the existing `--dry-run` short-circuit unchanged.
- **Dependencies**:
  - none
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - TTY archive prompts before moving files.
  - `--yes` bypasses the prompt.
  - Non-interactive without `--yes` exits `64`.
  - `--dry-run` continues to short-circuit before any writes.
  - New integration test covers all four branches.
- **Validation**:
  - `cargo test -p agent-workflow-primitives heuristic_inbox`

### Task 3.2: Move `codex-rate-limits --watch` guard into clap

- **Location**:
  - crates/codex-cli/src/cli.rs
  - crates/codex-cli/src/rate_limits/mod.rs
  - crates/codex-cli/tests/integration/rate_limits.rs
- **Description**: Add `#[arg(requires = "async_mode")]` to the
  `--watch` flag definition. Remove the runtime check at
  `rate_limits/mod.rs:113-119`. Adjust the error message wording so
  clap's parse-time output stays helpful ("`--watch` requires
  `--async`").
- **Dependencies**:
  - none
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - `codex-rate-limits --watch` fails at parse time with exit `64`.
  - `codex-rate-limits --watch --async` proceeds.
  - The runtime check is deleted.
  - New test asserts parse-time failure path.
- **Validation**:
  - `cargo test -p codex-cli rate_limits`

### Task 3.3: `codex-remove` non-interactive without `--yes` is a usage error

- **Location**:
  - crates/codex-cli/src/auth/remove.rs
  - crates/codex-cli/tests/integration/remove.rs
- **Description**: When `interactive_io_available()` is false and
  `--yes` is absent, return exit `64` regardless of `--json`. If
  `--json`/`--format json` is set, also emit the parse-error JSON
  envelope (consume `nils_common::cli_contract` when available; fall
  back to a local helper otherwise). The current "soft" JSON error is
  removed.
- **Dependencies**:
  - none
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - Non-interactive `codex-remove <target>` without `--yes` exits `64`.
  - With `--json` the envelope carries `code: "usage-error"`.
  - Interactive flows are unchanged.
  - New integration test covers all three branches (interactive, non-interactive text, non-interactive JSON).
- **Validation**:
  - `cargo test -p codex-cli remove`

### Task 3.4: Add `nils-term::prompt::confirm` shared helper

- **Location**:
  - crates/nils-term/src/prompt.rs (new)
  - crates/nils-term/src/lib.rs
  - crates/git-lock/src/delete.rs (consumer)
  - crates/memo-cli/src/commands/delete.rs (consumer)
  - crates/agent-workflow-primitives/src/heuristic_inbox.rs (consumer)
- **Description**: Once three call sites land (Sprint 1 `git-lock
  delete`, Sprint 2 `memo-cli delete`, Sprint 3 `heuristic-inbox
  archive`), extract the prompt + TTY-detection + `--yes` handling
  pattern into `nils-term::prompt::confirm(question: &str,
  default_no: bool, opts: PromptOptions)`. Migrate the three call
  sites to the helper.
- **Dependencies**:
  - Task 1.2 (git-lock delete)
  - Task 2.1 (memo-cli delete)
  - Task 3.1 (heuristic-inbox archive)
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - `nils-term::prompt::confirm` is publicly exported.
  - Three consumers reduce to one-line calls.
  - Unit tests cover TTY / non-TTY / forced branches.
  - Existing per-binary integration tests still pass.
- **Validation**:
  - `cargo test -p nils-term prompt`
  - `cargo test -p git-lock delete`
  - `cargo test -p memo-cli delete`
  - `cargo test -p agent-workflow-primitives heuristic_inbox`

## Testing Strategy

- Unit: cover the new TTY-prompt helper in `nils-term` once
  extracted (Task 3.4).
- Integration: one `destructive_safety` (or per-feature) test file
  per affected binary asserting the new branches (TTY prompt, `--yes`
  bypass, non-interactive failure, dry-run short-circuit).
- Manual: every destructive command should be smoke-tested under
  TTY and pipe before merge.
- Workspace: `cargo test --workspace` after each sprint.

## Risks & gotchas

- `is_terminal()` differs slightly on macOS vs. Linux; tests should
  not assume specific TTY emulation. Use existing nils-test-support
  patterns where available.
- `git-lock unlock --dry-run` calls `git diff --stat`; if the target
  hash is unreachable the command must still report a clear error.
- `memo-cli delete` redefinition is a breaking change for automation
  that relied on `--hard`. Document in release notes; provide a
  deprecation message for one minor cycle.
- `codex-remove`'s JSON output is consumed by other agents — pin the
  envelope shape with a snapshot test if one does not already exist.
- The `nils-term::prompt::confirm` helper is introduced only after
  three call sites exist; do not pre-extract in Sprint 1.
- The plan deliberately runs in parallel with
  `cli-output-contract-unification`; consume `nils-common::cli_contract`
  symbols when available, fall back to local constants otherwise. Do
  not block this plan on the other plan.

## Rollback plan

- Sprint 1 rollback: revert the `git-lock` safety-pack PR(s); the
  binary returns to its prior behaviour.
- Sprint 2 rollback: revert the `memo-cli` safety-pack PR(s); the
  prior `--hard`-required behaviour is restored. The breaking
  change is therefore recoverable without leaving the workspace in a
  bad state.
- Sprint 3 rollback (per task): each fix is its own PR; reverting one
  task leaves the other two intact.
- Task 3.4 rollback: revert the helper extraction; the three
  consumers inline their copies again. The shared helper can land
  later when the pattern is more mature.
