# Forge-CLI v1 Execution State

## Current State

- Status: in progress
- Target scope: whole plan (Sprints 0–8)
- Execution window: 2026-05-19 → ongoing
- Staged execution confirmation: not applicable (default-continue
  authorization: "默認就一直做下去")
- Current task: Sprint 4 review (PR pending)
- Next task: Task 5.1
- Last updated: 2026-05-20
- Branch/commit: `feat/forge-cli-v1-sprint4-merge` HEAD at `9094408`
  (Task 4.3); PR open and awaiting CI green + merge before cutting
  Sprint 5 branch off updated `origin/main`
- Source document: docs/plans/forge-cli/forge-cli-plan.md
- Direct source-doc execution waiver: not applicable

## Task Ledger

| ID       | Status    | Task                                                        | Evidence                   | Notes                                                          |
| -------- | --------- | ----------------------------------------------------------- | -------------------------- | -------------------------------------------------------------- |
| Task 1.1 | completed | Scaffold `crates/forge-cli/` package and workspace wiring   | PR #381 (merged `eb92969`) | Sprint 1                                                       |
| Task 1.2 | completed | Define clap command tree + global flags                     | PR #381                    | Sprint 1                                                       |
| Task 1.3 | completed | Implement provider detection                                | PR #381                    | Sprint 1                                                       |
| Task 1.4 | completed | Subprocess wrapper + envelope serializer + dry-run plumbing | PR #381                    | Sprint 1                                                       |
| Task 1.5 | completed | Implement `auth status` atom                                | PR #381                    | Sprint 1                                                       |
| Task 1.6 | completed | Implement `repo view` atom                                  | PR #381                    | Sprint 1                                                       |
| Task 1.7 | completed | Sprint 1 exit-code matrix + workspace gate                  | PR #381                    | Sprint 1                                                       |
| Task 2.1 | completed | PR body / title / branch validation module                  | PR #388 (merged `856797c`) | Sprint 2 — `crates/forge-cli/src/validations.rs`               |
| Task 2.2 | completed | `pr create` atom                                            | PR #388                    | Sprint 2                                                       |
| Task 2.3 | completed | `pr view`, `pr list`, `pr close` atoms                      | PR #388                    | Sprint 2                                                       |
| Task 2.4 | completed | `pr edit`, `pr comment`, `pr ready` atoms                   | PR #388                    | Sprint 2                                                       |
| Task 3.1 | completed | `pr checks` GitHub backend                                  | PR #390 (merged `7d93d1a`) | Sprint 3                                                       |
| Task 3.2 | completed | `pr checks` GitLab text parser with version fail-fast       | PR #390                    | Pinned `glab` 1.45.x — `glab_version_unsupported` → UNAVAIL 69 |
| Task 3.3 | completed | `pr wait-checks` polling loop                               | PR #390 + fix `1071cc6`    | `checks_failed` RUNTIME 1 / `checks_timeout` UNAVAIL 69        |
| Task 4.1 | completed | Merge-method resolution + `.forge-cli.toml` loader          | commit `cf45459`           | Sprint 4 — 12 lib tests covering loader walk + warning surface |
| Task 4.2 | completed | TTL-zero required-check re-check helper                     | commit `feae217`           | Sprint 4 — 6 lib + 4 integration tests pin no-caching contract |
| Task 4.3 | completed | `pr merge` atom                                             | commit `9094408`           | Sprint 4 — full lock-down chain + 12 lib + 4 integration tests |
| Task 5.1 | pending   | `issue create`, `issue view`, `issue close`, `issue reopen` | n/a                        | Sprint 5 — mirrors Sprint 2 layout                             |
| Task 5.2 | pending   | `issue edit`, `issue comment`                               | n/a                        | Sprint 5                                                       |
| Task 6.1 | pending   | `pr deliver` macro composition + step envelope              | n/a                        | Sprint 6                                                       |
| Task 6.2 | pending   | Macro CLI surface + dry-run plan rendering                  | n/a                        | Sprint 6                                                       |
| Task 7.1 | pending   | Parity harness                                              | n/a                        | Sprint 7                                                       |
| Task 7.2 | pending   | Exit-code matrix completion                                 | n/a                        | Sprint 7                                                       |
| Task 7.3 | pending   | Fixture redaction audit                                     | n/a                        | Sprint 7                                                       |
| Task 8.1 | pending   | `wrappers/forge-cli` + shell completions                    | n/a                        | Sprint 8                                                       |
| Task 8.2 | pending   | Homebrew tap formula update                                 | n/a                        | Sprint 8                                                       |
| Task 8.3 | pending   | `nils-cli` minor bump + tag + tap formula bump              | n/a                        | Sprint 8                                                       |

## Validation

| Command                                            | Status  | Summary                                              | Artifact                     |
| -------------------------------------------------- | ------- | ---------------------------------------------------- | ---------------------------- |
| `cargo test -p nils-forge-cli`                     | green   | 167 lib + 46 integration at end of Sprint 3          | local + PR #390 CI           |
| `cargo clippy --all-targets`                       | green   | end of Sprint 3                                      | local + PR #390 CI           |
| `bash scripts/ci/plan-bundle-validate.sh --strict` | green   | Sprint 0 docs-only gate (PR #379)                    | PR #379                      |
| `bash scripts/ci/completion-asset-audit.sh`        | green   | bash + zsh regenerated each sprint                   | PR #381 / #388 / #390        |
| `bash scripts/ci/completion-flag-parity-audit.sh`  | green   | parity with built-in `forge-cli` after each sprint   | PR #381 / #388 / #390        |
| `bash scripts/ci/cli-output-contract-lint.sh`      | green   | each sprint                                          | PR #381 / #388 / #390        |
| `bash scripts/ci/docs-placement-audit.sh`          | green   | docs co-located under `crates/forge-cli/docs/specs/` | PR #379                      |
| `bash scripts/ci/docs-hygiene-audit.sh`            | green   | markdownlint / rumdl clean across plan bundle        | PR #379 / #381 / #388 / #390 |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh`    | pending | end-of-Sprint-8 workspace gate                       | n/a                          |

## Blockers

- none

## Session Log

- 2026-05-19 — Sprint 0 merged (PR #379 / `f5eba86`): spec v1 + ops
  catalog + dispatch plan + discussion source. Docs-only gate green.
- 2026-05-19 — Sprint 1 merged (PR #381 / `eb92969`): crate scaffold,
  global flags, provider detection, `auth status`, `repo view`,
  `--dry-run`, exit-code matrix.
- 2026-05-19 — Sprint 2 merged (PR #388 / `856797c`): seven PR atoms +
  shared validation chain. Test surface jumps to 123 lib + 26
  integration.
- 2026-05-19 — Sprint 3 merged (PR #390 / `7d93d1a`): `pr checks` (gh
  JSON + glab text parser with pinned-minor fail-fast) and
  `pr wait-checks` polling loop with `Clock` trait. Caught a Linux CI
  flake in the `gh_sequence_stub` helper (timeout test) and shipped a
  follow-up fix `1071cc6` clamping the stub index to the trailing
  snapshot.
- 2026-05-20 — Cut `feat/forge-cli-v1-sprint4-merge` from `origin/main`
  for Sprint 4 (`pr merge` lock-down + TTL-zero required-check
  re-check). Tracking issue #391 opened on `sympoies/nils-cli` with
  source / plan / execution-state snapshot comments.
- 2026-05-20 — Sprint 4 production code complete locally: Task 4.1
  config loader (`cf45459`), Task 4.2 TTL-zero gate (`feae217`),
  Task 4.3 `pr merge` atom with six lock-down rules (`9094408`).
  Workspace test surface: 197 lib + 54 integration. PR pending push +
  CI green.
