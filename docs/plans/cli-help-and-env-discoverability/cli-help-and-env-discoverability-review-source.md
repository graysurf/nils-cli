# CLI Help and Env Discoverability Review Source

- Status: open, ready for implementation planning
- Date: 2026-05-19
- Source: static `/code-review-specialists` audit of the `nils-cli` workspace
  (`main` @ `bf740b5`).
- Scope: workspace-wide upgrade of `--help` quality, environment-variable
  surfacing, and clap-time flag-conflict declarations.

## Execution

- Recommended plan:
  docs/plans/cli-help-and-env-discoverability/cli-help-and-env-discoverability-plan.md
- Recommended execution state:
  docs/plans/cli-help-and-env-discoverability/cli-help-and-env-discoverability-execution-state.md

## Purpose

The audit found three related discoverability gaps: (1) many binaries read
environment variables that are never mentioned in `--help`; (2) help-text
density varies wildly (one-line summaries vs. full `after_help` examples);
(3) some flag conflicts are detected at runtime (after parsing succeeds)
when clap could refuse them at parse time. The fix is one style guide plus
mechanical, low-risk per-binary updates — most changes touch only clap
attributes, not behaviour.

## Current Judgment

A few binaries already model the target shape (`agent-workflow-primitives`
binaries use rich `after_help`; `agent-scope-lock` carries examples). The
rest cluster into two groups: clap-derive binaries (mechanical fix — add
`env`, `long_help`, `after_help`, `conflicts_with`) and hand-rolled binaries
(touched by the dispatch-modernization plan; out of scope here). Once a
style guide exists, the work is predictable and parallelisable.

## Findings

| Priority | ID | Issue | Evidence | Fix Location | Acceptance |
| --- | --- | --- | --- | --- | --- |
| high | C1 | Magic env vars are not surfaced in `--help` | `crates/api-gql/src/commands/call.rs:88` reads `GQL_HISTORY_FILE`; `crates/api-gql/src/commands/report.rs:58,119` reads `GQL_REPORT_INCLUDE_COMMAND_ENABLED`, `GQL_VARS_MIN_LIMIT`; `crates/git-lock/src/store.rs:111` reads `ZSH_CACHE_DIR`; `crates/codex-cli/src/auth/use_secret.rs:220` mentions `CODEX_SECRET_CACHE_DIR` only in error | each binary's clap layer (use `#[arg(env = "...")]`) + a per-binary `after_help` ENVIRONMENT section for binary-wide vars | every env var read by a binary is either listed on a clap `arg(env = "...")` flag or appears in the binary's `after_help` ENVIRONMENT section |
| medium | C2 | Help-text density varies (one-liners vs. detailed) | `crates/semantic-commit/src/usage.rs:62-81` (one-liners); `crates/agent-scope-lock/src/cli.rs:11` (`after_help` examples); `crates/agent-workflow-primitives/src/canary_check.rs` (comprehensive) | every clap-derive binary | every user-facing binary follows the style guide: short `about`, long `long_about`, at least one `after_help` example, and an `EXIT CODES` section |
| medium | C3 | `--json` ↔ `--format` conflict is detected at runtime instead of by clap | `crates/memo-cli/src/cli.rs:233-242` (runtime check), `crates/memo-cli/src/cli.rs:70-75` (no `conflicts_with`) | `memo-cli/src/cli.rs` and any other binary that keeps `--json` alongside `--format` | clap rejects `--json --format text` at parse time with usage-error exit; runtime check is removed |
| low | C4 | `api-gql` has an implicit default subcommand that `--help` does not document | `crates/api-gql/src/main.rs:25-30` (implicit `call` prepend); `crates/api-gql/src/main.rs:42` (prose only) | `api-gql/src/main.rs` and its clap definition | `api-gql --help` clearly states `call` is the default (or the implicit fallback is removed); flags that belong to `call` only do not appear as root flags |

## Ownership Boundary

- Runtime: every user-facing clap-derive binary's `cli.rs`.
- Docs: `docs/runbooks/cli-help-style-guide.md` (new).
- Test/harness: small per-binary `--help` snapshot tests where they don't
  already exist; one `--help` golden file under `tests/fixtures/help/` per
  binary at a minimum.

## Backlog / Next Fixes

1. Draft the style guide and circulate before any per-binary change.
2. Ship the style guide plus a worked migration on one
   moderately-complex binary (recommend `memo-cli` since it has the most
   touched-but-simple flags).
3. Roll the style guide out across the rest of the user-facing binaries.
4. Fix `api-gql` implicit-default behaviour after the style guide lands
   (the choice between "document the default" vs. "remove the implicit
   fallback" is a UX decision documented in the plan).

## Retention Intent

- This source doc is execution coordination — delete after plan
  completes.
- Promote `docs/runbooks/cli-help-style-guide.md` and the per-binary
  help-snapshot fixtures as durable knowledge.

## Validation Gate

- `bash scripts/ci/plan-bundle-validate.sh --strict`
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
- Per binary: `cargo test -p <crate> help_snapshot` (new test name)
- Workspace: `bash scripts/ci/nils-cli-checks-entrypoint.sh`

## Do Not Do

- Do not turn `--help` output into a marketing brochure — every section
  must answer a concrete user question (defaults? env vars? exit codes?
  example invocation?).
- Do not add `--help` snapshot tests that lock the exact wording of
  every flag — lock only the structure (section presence and known env
  vars). Wording can drift; structural drift is the regression target.
- Do not absorb hand-rolled-dispatch binaries here — they migrate first
  via `cli-dispatch-modernization` and join this work as a follow-up.

## Open Questions

- Should the style guide require an `EXIT CODES` block on every binary,
  or only on binaries with more than two exit codes? (Recommended:
  always — the contract is small and consistency wins.)
- For `api-gql`, prefer "document the default" or "remove the implicit
  fallback"? (Recommended: document the default; removing it is a
  breaking change for users who already type bare `api-gql <op>`.)
- Should env-var documentation block on the
  `cli-output-contract-unification` plan first, or land in parallel?
  (Recommended: parallel — neither plan blocks the other.)
