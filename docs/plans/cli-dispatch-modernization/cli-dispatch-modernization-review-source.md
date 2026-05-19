# CLI Dispatch Modernization Review Source

- Status: open, ready for implementation planning
- Date: 2026-05-19
- Source: static `/code-review-specialists` audit of the `nils-cli` workspace
  (`main` @ `bf740b5`).
- Scope: migrate hand-rolled CLI dispatch onto `clap` derive, settle global
  flag and short-flag conventions, and backfill the test gaps these binaries
  carry.

## Execution

- Recommended plan:
  docs/plans/cli-dispatch-modernization/cli-dispatch-modernization-plan.md
- Recommended execution state:
  docs/plans/cli-dispatch-modernization/cli-dispatch-modernization-execution-state.md

## Purpose

Five binaries (`semantic-commit`, `plan-tooling`, `git-cli`, `git-summary`,
`fzf-cli`) hand-roll their own argument-dispatch loop. The pattern diverges
on three things that bite users: how help is rendered, how `-V` / `-v` /
`-h` are interpreted, and whether unknown-subcommand errors honor exit-code
or JSON-envelope conventions. The audit also found two test gaps
(`semantic-commit --quiet` and `CAT_PAGER_ENV`) on the same binaries.
Migrating to clap derive once is cheaper than maintaining five custom
parsers.

## Current Judgment

`memo-cli` is the proof point that clap derive carries the full required
shape (Subcommand enum, global flags, `--version`/`--help` auto). The risk
profile splits in two:

- Low-risk migrations: `semantic-commit`, `plan-tooling` — small surface,
  no complex global flags, no behavioural fan-out. Move first.
- Higher-risk migrations: `git-cli`, `git-summary`, `fzf-cli` — bigger
  surface (`git-cli` has Groups, `fzf-cli` has interactive sessions).
  Move after the small ones land and the migration recipe is proven.

The global-flag and short-flag rules can be captured in the same style
guide that `cli-help-and-env-discoverability` is publishing, so the
documentation cost is shared.

## Findings

| Priority | ID | Issue | Evidence | Fix Location | Acceptance |
| --- | --- | --- | --- | --- | --- |
| medium | D1 | Five binaries hand-roll dispatch and diverge on help/version/error handling | `crates/semantic-commit/src/usage.rs:3-32`; `crates/plan-tooling/src/usage.rs:3-33`; `crates/git-cli/src/usage.rs:33-75`; `crates/git-summary/src/app.rs:11-104`; `crates/fzf-cli/src/main.rs:62` | each binary's `main.rs`/`usage.rs` / `app.rs` | every user-facing binary parses with clap derive; `usage.rs` files are deleted; help/version are clap-generated; unknown-subcommand errors flow through the shared parse-error helper from the output-contract plan |
| low | D2 | Global-flag placement (pre- vs. post-subcommand) is inconsistent | `crates/memo-cli/src/cli.rs:66-75` (global db/json/format); `crates/git-scope/src/main.rs:25-30`; `crates/plan-issue-cli/src/cli.rs:23-45`; `crates/git-lock/src/main.rs:18-27` (no globals) | the style guide + each binary's clap layer | the style guide names which flags must be global (`--format`, `--quiet`, `--verbose`, repo-locating flags) and which must be subcommand-scoped; every binary conforms |
| low | D3 | `-V` / `-v` / `--help` handling diverges; some binaries set `disable_help_flag = true` | `crates/git-scope/src/main.rs:20`; `crates/git-lock/src/main.rs:22`; `crates/semantic-commit/src/usage.rs:39-40`; `crates/memo-cli/src/cli.rs:59-62` | the style guide + each binary's clap layer | `-V` = version, `-v` = verbose, `-h`/`--help` are clap-auto on every binary; `disable_help_flag = true` is allowed only with a documented binary-wide rationale |
| medium | I1 | `semantic-commit --quiet` is parsed but never asserted by an integration test | `crates/semantic-commit/src/commit.rs:41` (`quiet: bool`) — no covering test | `crates/semantic-commit/tests/integration/commit.rs` | one new test asserts `--quiet` suppresses progress and summary while preserving exit-code behaviour |
| low | I3 | `CAT_PAGER_ENV` (which hardcodes `GIT_PAGER=cat`) is never tested for effect | `crates/semantic-commit/src/commit.rs:14` | `crates/semantic-commit/tests/integration/commit.rs` | one new test runs the commit flow with and without `GIT_PAGER=less` and asserts the captured stdout matches the no-pager output |

## Ownership Boundary

- Runtime: the five hand-rolled binaries.
- Style guide: `docs/runbooks/cli-help-style-guide.md` (owned by the
  `cli-help-and-env-discoverability` plan, with global-flag and
  short-flag sections added by this plan).
- Test/harness: per-binary `tests/integration/*.rs` files.

## Backlog / Next Fixes

1. Land the small migrations (`semantic-commit`, `plan-tooling`) first
   to prove the recipe.
2. Migrate `git-summary` and `fzf-cli` next (medium complexity).
3. Migrate `git-cli` last (largest surface, biggest blast radius).
4. Backfill the `--quiet` and `CAT_PAGER_ENV` tests during the
   `semantic-commit` migration so the new clap definition is exercised
   end to end.

## Retention Intent

- This source doc is execution coordination — delete after the plan
  completes.
- The global-flag and short-flag rules become a durable section of
  `cli-help-style-guide.md`.

## Validation Gate

- `bash scripts/ci/plan-bundle-validate.sh --strict`
- Per binary: `cargo test -p <crate>` after each migration
- Workspace: `bash scripts/ci/nils-cli-checks-entrypoint.sh`
- Full pre-delivery: `NILS_CLI_TEST_RUNNER=nextest bash
  scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage`

## Do Not Do

- Do not migrate every binary in one PR — keep one binary per PR so
  bisects stay clean.
- Do not change subcommand names or flag semantics during migration —
  this is a structural refactor, not a UX redesign.
- Do not silently drop `-V` short flag where it already exists —
  migrate via clap's default `-V`/`--version` handling.
- Do not introduce new exit codes during the migration; the
  `cli-output-contract-unification` plan owns that contract.

## Open Questions

- Should `git-cli` retain its `Group` abstraction (a meta-level above
  subcommands) under clap, or flatten Groups into plain subcommands?
  (Recommended: retain Groups as a multi-level Subcommand tree —
  flattening is a user-facing change.)
- Should `fzf-cli`'s interactive subcommands stay hand-driven inside
  the handler (the interactive loop is not clap's job) while only the
  argument layer migrates? (Recommended: yes; clap handles parsing,
  interactive flow stays in the handler.)
- Is the `disable_help_flag = true` on `git-scope` and `git-lock`
  load-bearing for a specific user case? (Recommended: remove unless a
  load-bearing reason surfaces during migration; bias toward
  consistency.)
