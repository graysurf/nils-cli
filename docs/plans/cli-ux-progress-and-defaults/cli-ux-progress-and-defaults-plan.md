# Plan: CLI UX Progress and Defaults

## Overview

Land low-severity UX polish in two sprints. Sprint 1 covers progress /
TTY consistency: add the missing integration test for
`nils-term::progress` and migrate the inline TTY callers onto the
helper. Sprint 2 covers the two user-visible default knobs: a
`memo-cli` truncation footer and a `semantic-commit
--max-header-width` flag plus env override.

## Read First

- Primary source: docs/plans/cli-ux-progress-and-defaults/cli-ux-progress-and-defaults-review-source.md
- Source type: discussion-to-implementation-doc
- Open questions carried into execution:
  - Add `truncated: true` to JSON output? Default: yes.
  - Should env be a default, flag override? Default: yes.
  - Wait for `cli-output-contract-unification` lint to add the "no direct `is_terminal`" rule? Default: yes.

## Scope

- In scope:
  - `crates/nils-term/tests/integration/progress.rs` (new) covering the TTY auto-disable path.
  - Migrating `git-summary`, `api-gql`, and any other audit-flagged binary off inline `is_terminal()` for progress.
  - `memo-cli list` / `memo-cli search` truncation footer (text + JSON `truncated` field).
  - `semantic-commit commit` `--max-header-width` flag and `SEMANTIC_COMMIT_HEADER_WIDTH` env override.
- Out of scope:
  - Changes to `nils-term::progress` defaults or rendering.
  - Pagination redesign or limit defaults.
  - Help-text style work (delegated to `cli-help-and-env-discoverability`).
  - Output-contract changes (delegated to `cli-output-contract-unification`).

## Assumptions

1. `nils-term::progress`'s `ProgressEnabled::Auto` already reads
   `is_terminal()` correctly; this plan only adds coverage, not
   behaviour changes.
2. Binaries with inline `is_terminal()` calls outside
   `nils-term::progress` are limited to the four cited examples; a
   short grep audit during Sprint 1 confirms the list.
3. The `MAX_LINE_WIDTH` constant in `semantic-commit` is the only
   header-width policy point; no other binary enforces a copy.
4. `memo-cli` already tracks limit/offset in JSON output, so the
   `truncated` field is additive.

## Sprint 1: Progress / TTY consistency

**Goal**: Lock the `nils-term::progress` TTY auto-disable behaviour
with an integration test, then migrate the inline-`is_terminal()`
callers onto the helper.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-term`
  - `cargo test -p git-summary`
  - `cargo test -p api-gql`
  - Manual: `<binary> | cat` (piped) → no ANSI / progress chars in captured stderr.
  - Manual: `<binary>` (TTY) → progress visible as before.
- Verify: no binary writes ANSI / progress chars to a non-TTY stderr.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 1.1: Add `nils-term::progress` TTY-pipe integration test

- **Location**:
  - crates/nils-term/tests/integration/progress.rs (new)
- **Description**: Create an integration test that drives a small
  consumer through the public `Progress` API, captures stderr into a
  file (no TTY), and asserts the captured bytes contain no ANSI
  escape sequences and no spinner / progress characters. Add the
  inverse test (forced enable via `ProgressEnabled::Always`) for
  regression symmetry. Use `tempfile` for the redirect.
- **Dependencies**:
  - none
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - Test passes against the current `nils-term::progress`.
  - Disabling the TTY auto-detect (temporarily forcing `Always`) makes the test fail.
  - Test is hermetic — does not require an attached TTY.
- **Validation**:
  - `cargo test -p nils-term progress`

### Task 1.2: Audit inline `is_terminal()` callers and replace with helper

- **Location**:
  - crates/git-summary/src/summary.rs
  - crates/api-gql/src/main.rs
  - any other file the audit grep surfaces
- **Description**: Run a `grep -rn "is_terminal\|IsTerminal" crates/
  --include='*.rs'` audit, exclude `nils-term/src/progress.rs` and
  any TTY-prompt usage from the destructive-safety plan, and migrate
  the remaining call sites onto `nils-term::progress` (or a
  documented direct call when progress is not the goal). Where the
  inline call is checking TTY for a non-progress reason (e.g. colour
  output), leave it but add a comment naming the rationale.
- **Dependencies**:
  - Task 1.1
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - Audit grep returns only documented direct callers.
  - Each migrated binary still works on TTY and pipe.
  - No regression in existing per-binary tests.
- **Validation**:
  - `cargo test -p git-summary`
  - `cargo test -p api-gql`
  - Workspace: `cargo test --workspace`

## Sprint 2: Default visibility knobs

**Goal**: Make the silent-truncation and hardcoded-line-width defaults
visible and overridable.

**Demo/Validation**:

- Commands:
  - `cargo test -p memo-cli`
  - `cargo test -p semantic-commit`
  - Manual: `memo-cli list` (text) on a populated DB with 25 rows shows the footer "(showing 20 of N items, use --limit)".
  - Manual: `memo-cli list --format json` returns `truncated: true` in the envelope.
  - Manual: `semantic-commit commit --max-header-width 72` rejects headers ≥73 chars.
  - Manual: `SEMANTIC_COMMIT_HEADER_WIDTH=80 semantic-commit commit` uses 80 unless `--max-header-width` is also set.
- Verify: the defaults are now visible and configurable.

**PR grouping intent**: `group`
**Execution Profile**: `parallel-x2`

### Task 2.1: Add truncation footer to `memo-cli list` and `search`

- **Location**:
  - crates/memo-cli/src/output/text.rs
  - crates/memo-cli/src/output/json.rs
  - crates/memo-cli/src/commands/list.rs
  - crates/memo-cli/src/commands/search.rs
  - crates/memo-cli/tests/integration/list_search_footer.rs (new)
- **Description**: When `rows.len() == limit` (truncation likely),
  print a footer in text mode naming the limit and suggesting
  `--limit` or `--offset`. In JSON output, add `truncated: bool` to
  the envelope (additive field). Behaviour when not truncated stays
  the same (no footer; `truncated: false`).
- **Dependencies**:
  - none
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - Text mode shows footer only when truncated.
  - JSON mode always includes `truncated`.
  - New integration test covers both modes and both truncated / not-truncated paths.
- **Validation**:
  - `cargo test -p memo-cli list_search_footer`
  - `cargo test -p memo-cli`

### Task 2.2: Add `--max-header-width` flag and env override to `semantic-commit commit`

- **Location**:
  - crates/semantic-commit/src/commit.rs
  - crates/semantic-commit/src/cli.rs (or wherever the clap-derive definition lands after the dispatch-modernization plan)
  - crates/semantic-commit/tests/integration/max_header_width.rs (new)
- **Description**: Add a clap `--max-header-width <N>` argument
  with a default that mirrors the current `MAX_LINE_WIDTH = 100`.
  Read `SEMANTIC_COMMIT_HEADER_WIDTH` from the environment as a
  fallback that only applies when the flag is absent (flag wins when
  both are set). Update the rejection error to name the active
  limit. Update help text to state the default and the env override.
- **Dependencies**:
  - none
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - `--max-header-width 72` rejects ≥73-char headers.
  - `SEMANTIC_COMMIT_HEADER_WIDTH=80` (no flag) uses 80.
  - `--max-header-width 80` plus `SEMANTIC_COMMIT_HEADER_WIDTH=72` uses 80 (flag wins).
  - Error message names the active limit.
  - New integration test covers the three branches above.
- **Validation**:
  - `cargo test -p semantic-commit max_header_width`

## Testing Strategy

- Unit: none required.
- Integration: the three new files described above.
- Manual: piped-vs-TTY smoke test for affected binaries; truncated
  vs. not truncated for memo-cli; flag-only / env-only / both for
  semantic-commit.
- Workspace: `cargo test --workspace` after each sprint.

## Risks & gotchas

- The `nils-term::progress` integration test must not require a real
  TTY; use the `Auto` / `Always` toggle to drive both branches.
- Some inline `is_terminal()` callers may be checking TTY for colour
  output, not progress. Distinguish the cases during the audit —
  colour TTY detection is acceptable inline.
- The `truncated` JSON field is additive but JSON consumers may
  assert exact envelope shape; coordinate with the
  `cli-output-contract-unification` snapshot tests once both plans
  are merging.
- `SEMANTIC_COMMIT_HEADER_WIDTH` must be a positive integer; reject
  zero / negative / non-numeric values with a clear usage error.
  Document the env var in the `cli-help-and-env-discoverability`
  plan as a follow-up.

## Rollback plan

- Sprint 1 rollback: revert the migration commits; the inline
  `is_terminal()` callers return. The integration test stays as
  durable coverage even if no migration lands.
- Sprint 2 rollback (per task): each fix is its own PR; reverting one
  leaves the other intact. The `truncated` JSON field is additive,
  so consumers do not break on revert.
