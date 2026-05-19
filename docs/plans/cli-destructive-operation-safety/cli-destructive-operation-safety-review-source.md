# CLI Destructive Operation Safety Review Source

- Status: open, ready for implementation planning
- Date: 2026-05-19
- Source: static `/code-review-specialists` audit of the `nils-cli` workspace
  (`main` @ `bf740b5`).
- Scope: harden the destructive command surfaces across `git-lock`,
  `memo-cli`, `heuristic-inbox`, and `codex-cli` so the highest-blast-radius
  user errors are caught before any state changes.

## Execution

- Recommended plan:
  docs/plans/cli-destructive-operation-safety/cli-destructive-operation-safety-plan.md
- Recommended execution state:
  docs/plans/cli-destructive-operation-safety/cli-destructive-operation-safety-execution-state.md

## Purpose

The audit's red-team lens surfaced nine findings on destructive operations
and stdin handling. The unifying theme is that confirmation gates are
either missing, weak ("type --hard"), or only enforced at runtime after
heavy setup work has already happened. None of these findings is a code
bug — every command does what it says — but the design defaults assume an
attentive user. The fix pattern is the same across binaries: parse-time
guards via `clap` (`conflicts_with`, `requires`), TTY-aware confirmation,
and richer dry-run output. This plan groups the fixes by ownership so each
binary's safety pass lands in one PR.

## Current Judgment

The destructive surfaces split naturally by binary owner:

- `git-lock` carries the highest blast radius (`unlock` runs `git reset
  --hard`; `delete` permanently removes lock files).
- `memo-cli` has a confusing `--hard` "confirmation" pattern plus an
  unguarded `--stdin` that hangs on TTY.
- `heuristic-inbox` and `codex-cli` are smaller surfaces but
  inconsistent with the rest of the workspace.

The dry-run preview improvements (memo-cli apply, git-lock) and the
error-context improvement (git-lock) are low-risk polish that fits the
same PRs.

## Findings

| Priority | ID | Issue | Evidence | Fix Location | Acceptance |
| --- | --- | --- | --- | --- | --- |
| high | E1 | `git-lock unlock` runs `git reset --hard` with only a y/N prompt and no preview | `crates/git-lock/src/unlock.rs:48-56`; `crates/git-lock/src/main.rs:36-39` (no `--dry-run` arg) | `crates/git-lock/src/unlock.rs` + clap layer | new `--dry-run` flag prints the `git diff HEAD <hash>` summary and exits without resetting; the y/N prompt shows the count of files / lines that will change |
| high | E2 | `git-lock delete` permanently removes the lock file with a generic prompt | `crates/git-lock/src/delete.rs:37-56` | `crates/git-lock/src/delete.rs` | prompt prefaces with `WARNING: this permanently deletes the lock record from disk`; `--force` allows non-interactive bypass; default behaviour stays interactive |
| high | E3 | `memo-cli delete` uses `--hard` as a confirmation flag, not a mode flag | `crates/memo-cli/src/commands/delete.rs:10-14`; `crates/memo-cli/src/cli.rs:137-140` | `crates/memo-cli/src/commands/delete.rs` and `cli.rs` | interactive TTY mode prompts for confirmation by default; non-interactive requires `--yes`; `--hard` is reinterpreted as a real mode flag (soft archive vs. hard delete) or removed with a clear migration note |
| medium | E4 | `heuristic-inbox archive` has no confirmation in non-dry-run mode | `crates/agent-workflow-primitives/src/heuristic_inbox.rs:1648-1650` (dry-run short-circuit); `crates/agent-workflow-primitives/src/heuristic_inbox.rs:1560-1680` (no prompt) | `crates/agent-workflow-primitives/src/heuristic_inbox.rs` | TTY mode prompts before archive moves; non-interactive requires `--yes`; behaviour aligns with `git-lock delete` |
| medium | E5 | `codex-rate-limits --watch` requires `--async` but the check runs at runtime, not parse time | `crates/codex-cli/src/rate_limits/mod.rs:113-119`; `crates/codex-cli/src/cli.rs:214-216` | `crates/codex-cli/src/cli.rs` | clap `#[arg(requires = "async_mode")]` rejects `--watch` without `--async` at parse time |
| medium | E6 | `codex-remove --json` returns a "soft" JSON error when non-interactive and `--yes` is missing | `crates/codex-cli/src/auth/remove.rs:100-123,176-178` | `crates/codex-cli/src/auth/remove.rs` | non-interactive without `--yes` is a usage error (exit `64`) regardless of `--json`; the JSON envelope (when present) carries `code: "usage-error"` and a clear message |
| medium | F1 | `memo-cli apply --stdin` blocks on TTY without a hint | `crates/memo-cli/src/commands/apply.rs:57-62`; `crates/memo-cli/src/cli.rs:223-225` | `crates/memo-cli/src/commands/apply.rs` and `cli.rs` | `--stdin` + TTY stdin fails fast with a usage error ("`--stdin` requires piped input"); `--input` or piped input proceeds as today |
| low | I4 | `memo-cli apply --dry-run` does not show what would change | `crates/memo-cli/src/commands/apply.rs:80-130` | `crates/memo-cli/src/commands/apply.rs`; output modules | dry-run JSON output includes a `changes: [{item_id, field, old, new}]` array; text mode prints the first N changes |
| low | I5 | `git-lock` errors print raw error chains without remediation hints | `crates/git-lock/src/main.rs:145-150` | `crates/git-lock/src/main.rs` | the catch-all error formatter wraps known errors with a one-line next-step hint (e.g. "run `git-lock list` to see available locks") |

## Ownership Boundary

- Runtime: `git-lock`, `memo-cli`, `agent-workflow-primitives/heuristic_inbox`,
  `codex-cli`.
- Shared library: `nils-common::cli_contract` (consumed for `exit::USAGE`
  during the migration) and any TTY-prompt helper added during E2/E3/E4.
- Test/harness: per-binary integration tests under
  `crates/<crate>/tests/integration/destructive_safety.rs` (new file
  name) for the parse-time guards, plus existing test files for the
  behavioural tests.

## Backlog / Next Fixes

1. Ship the `git-lock` safety pack first (E1, E2, I5) — highest blast
   radius.
2. Ship the `memo-cli` safety pack next (E3, F1, I4) — most user
   confusion.
3. Ship `heuristic-inbox` and `codex-cli` (E4, E5, E6) — smaller but
   keeps the workspace consistent.

## Retention Intent

- This source doc is execution coordination — delete after plan
  completes.
- The TTY-prompt helper (if added) becomes durable knowledge inside
  `nils-common` or `nils-term`.

## Validation Gate

- `bash scripts/ci/plan-bundle-validate.sh --strict`
- Per binary: `cargo test -p <crate> destructive_safety`
- Manual: TTY vs. piped invocation of each affected command.
- Workspace: `bash scripts/ci/nils-cli-checks-entrypoint.sh`

## Do Not Do

- Do not auto-bypass prompts when stdin is a pipe — require explicit
  `--yes` in non-interactive mode. Silent auto-bypass is the worst of
  both worlds.
- Do not change subcommand names; the audit asks for safer defaults,
  not rebrands.
- Do not weaken the `--dry-run` semantics elsewhere — dry-run must
  remain side-effect-free.
- Do not depend on the `cli-output-contract-unification` plan landing
  first; this plan can land in parallel and consume `exit::USAGE`
  from a temporary local constant if necessary.

## Open Questions

- For `memo-cli delete`: keep `--hard` as a real mode (soft archive
  vs. hard delete) or remove it entirely? (Recommended: redefine as a
  real mode — soft archive is a useful feature to expose now.)
- For `git-lock unlock --dry-run`: render the diff as `git diff`
  output or as a per-file summary table? (Recommended: per-file
  summary table, with a `--verbose` flag enabling full diff.)
- Should `codex-remove` keep emitting JSON when failing non-interactively
  without `--yes`? (Recommended: yes, but with `code: "usage-error"`
  and exit `64` — never `1`.)
- Should the TTY-prompt logic become a shared helper in `nils-term`?
  (Recommended: yes if the pattern reaches three or more call sites;
  add the helper in the first task that hits the third call site.)
