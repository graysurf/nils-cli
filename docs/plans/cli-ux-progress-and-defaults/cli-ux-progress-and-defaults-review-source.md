# CLI UX Progress and Defaults Review Source

- Status: open, ready for implementation planning
- Date: 2026-05-19
- Source: static `/code-review-specialists` audit of the `nils-cli` workspace
  (`main` @ `bf740b5`).
- Scope: small-surface UX polish — progress / TTY consistency, truncation
  visibility, and magic-default flags — across binaries that already have
  the core contracts right but leak ANSI to pipes, silently truncate
  results, or hardcode policy limits.

## Execution

- Recommended plan:
  docs/plans/cli-ux-progress-and-defaults/cli-ux-progress-and-defaults-plan.md
- Recommended execution state:
  docs/plans/cli-ux-progress-and-defaults/cli-ux-progress-and-defaults-execution-state.md

## Purpose

Four findings cluster as low-severity polish that does not need the
foundation work of the other plans: progress UX is split between
`nils-term::progress` and hand-rolled `is_terminal()` calls (so some
binaries leak ANSI into pipes), `memo-cli list` / `search` silently
truncate results to 20 with no on-screen indicator in text mode, and
`semantic-commit` hardcodes a 100-character header limit with no override.
None of these are blocking, but they show up to users daily and have
straightforward fixes.

## Current Judgment

`nils-term::progress` is already the canonical place — its
`ProgressEnabled::Auto` correctly disables progress on non-TTY. The job is
to migrate the few binaries that still inline TTY detection and to add the
missing integration test that guarantees the helper keeps working. The
truncation footer and `MAX_LINE_WIDTH` override are isolated changes to two
binaries.

## Findings

| Priority | ID | Issue | Evidence | Fix Location | Acceptance |
| --- | --- | --- | --- | --- | --- |
| low | F2 | Progress UX is inconsistent — some binaries use `nils_term::progress`, others inline `stderr().is_terminal()` and may leak ANSI into pipes | `crates/semantic-commit/src/commit.rs:1-7` (uses helper); `crates/api-test/src/main.rs:14` (uses helper); `crates/git-summary/src/summary.rs:53` (inline `is_terminal()`); `crates/api-gql/src/main.rs:89` (inline) | binaries that inline TTY detection | every binary that renders progress / spinner output uses `nils_term::progress`; the workspace lint added in `cli-output-contract-unification` includes a "no direct `is_terminal` for progress" rule |
| low | F3 | `nils-term::progress` TTY auto-disable has no integration test (only an inline call to `is_terminal()`) | `crates/nils-term/src/progress.rs:254` | `crates/nils-term/tests/integration/progress.rs` (new) | one integration test pipes stderr to a file and asserts no ANSI / progress chars appear |
| medium | G1 | `memo-cli list` and `memo-cli search` default `--limit 20` and silently truncate in text mode | `crates/memo-cli/src/cli.rs:145-146` (list default); `crates/memo-cli/src/cli.rs:163-164` (search default); `crates/memo-cli/src/output/text.rs:41-63` (no footer); `crates/memo-cli/src/commands/list.rs:33-43` (JSON has metadata) | `crates/memo-cli/src/output/text.rs` | when `rows.len() == limit`, text output prints a footer naming the limit and suggesting `--limit` and pagination; JSON output unchanged |
| medium | H1 | `semantic-commit` hardcodes `MAX_LINE_WIDTH = 100` with no override | `crates/semantic-commit/src/commit.rs:577` (const); `crates/semantic-commit/src/commit.rs:482` (error) | `crates/semantic-commit/src/commit.rs` and clap layer | new `--max-header-width <N>` flag plus `SEMANTIC_COMMIT_HEADER_WIDTH` env var override the default; help text states the active default |

## Ownership Boundary

- Runtime: `nils-term`, `memo-cli`, `semantic-commit`, and any binary
  caught migrating off inline TTY detection.
- Test/harness: `crates/nils-term/tests/integration/progress.rs` (new);
  per-binary tests for the footer and override behaviour.
- Lint: optional augmentation to the workspace lint script introduced
  by `cli-output-contract-unification` Task 3.4.

## Backlog / Next Fixes

1. Add the missing `nils-term::progress` integration test first — it
   protects the rest of the migrations.
2. Migrate any binary that inlines `is_terminal()` for progress
   decisions onto the helper.
3. Add the `memo-cli` truncation footer.
4. Add the `semantic-commit --max-header-width` flag and env override.

## Retention Intent

- This source doc is execution coordination — delete after plan
  completes.
- The progress integration test stays as durable regression coverage.

## Validation Gate

- `bash scripts/ci/plan-bundle-validate.sh --strict`
- `cargo test -p nils-term progress`
- `cargo test -p memo-cli list_search_footer`
- `cargo test -p semantic-commit max_header_width`
- Workspace: `bash scripts/ci/nils-cli-checks-entrypoint.sh`

## Do Not Do

- Do not change `nils-term::progress` defaults — only add the test
  and migrate callers.
- Do not lower the default `--limit` below 20; the footer addresses
  the issue without surprising existing users.
- Do not lower the default `MAX_LINE_WIDTH` below 100; widen the
  range of overrides instead. Going below 100 by default is a
  behavioural change for every existing user.
- Do not add new global flags to add the override — keep
  `--max-header-width` scoped to `semantic-commit commit`.

## Open Questions

- Should the truncation footer also appear in JSON output as a
  `truncated: true` field? (Recommended: yes — consumers benefit, and
  the field is additive.)
- Should `SEMANTIC_COMMIT_HEADER_WIDTH` be a hard ceiling or a soft
  default that `--max-header-width` overrides? (Recommended: env var
  is a default; flag wins when both are set.)
- Should this plan wait for `cli-output-contract-unification`'s
  lint script to land before adding the "no direct `is_terminal`"
  rule? (Recommended: yes — keep the lint script ownership singular.)
