# plan-tooling validate Ergonomics Execution State

## Current State

- Status: complete (F8 deferred at gate)
- Target scope: whole plan (F1–F8 across Sprints 1–3)
- Execution window: full autonomous run (user-confirmed 2026-05-19)
- Staged execution confirmation: confirmed(full autonomous)
- Current task: Release 0.9.0 via `nils-cli-bump-version-tag-release`
- Next task: (post-release) confirm tap publish + downstream smoke
- Last updated: 2026-05-19
- Branch/commit: feat/plan-tooling-validate-ergonomics
- Source document:
  `docs/plans/plan-tooling-validate-ergonomics/plan-tooling-validate-ergonomics-plan.md`
- Direct source-doc execution waiver: not applicable

## Task Ledger

| ID       | Status      | Task                                             | Evidence                                                       | Notes                                                                                      |
| -------- | ----------- | ------------------------------------------------ | -------------------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| Task 1.1 | done        | F1 EXPLAIN_CATALOG extension                     | commit 00dde13; `cargo test -p nils-plan-tooling` 100% green   | bundle source-doc + KNOWN_UNCATALOGUED allowlist; ALL_EMITTED_ERROR_PATTERNS lock test     |
| Task 1.2 | done        | F2 markdown wrapper normalization                | commit 00dde13; bundle.rs::markdown_field + clean_link_value   | four documented variants accepted; pre-0.9.0 regression test added                         |
| Task 2.1 | done        | F3 free-form dep notes                           | commit 8a3b320; parse.rs::Dependency struct + integration test | to-json round-trips `{id, notes}`; callers in batches/split_prs updated                    |
| Task 2.2 | done        | F4 directory Location + audit                    | commit 8a3b320; audit subsection below                         | downstream consumers cleared; new `location-directory-missing` class                       |
| Task 2.3 | done        | F5 class-grouped text output                     | commit 8a3b320; `format_errors_text` + golden JSON test        | `--no-group` escape hatch; JSON byte-stable                                                |
| Task 3.1 | done        | F6 spec subcommand                               | this commit; src/spec.rs + integration test                    | `--format json` / `--format text`, `-V`, `--help`; clap completion updated and regenerated |
| Task 3.2 | done        | F7 --fix mechanical rewrites                     | this commit; src/fix.rs + integration tests                    | fixed-point unit-property test across 13 fixtures; bundle-wide rewrites                    |
| Task 3.3 | deferred    | F8 --watch (optional)                            | decision below                                                 | gate fired: defer; `--fix` reduces iteration cost enough                                   |
| Release  | in-progress | 0.9.0 bump via nils-cli-bump-version-tag-release | n/a                                                            | gates green; about to run release skill                                                    |

## Audit (F4 sub-step A): downstream consumers of `Location`

Search corpus: every `crates/plan-tooling/src/*.rs` reference to `task.location`, `.location`, plus
the public CLI surface (`split-prs`, `batches`, `artifact-audit`, `to-json`).

| Consumer                                                               | Behavior                                                                                             | File-only assumption?                              |
| ---------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- | -------------------------------------------------- |
| `crates/plan-tooling/src/batches.rs:351`                               | Iterates `task.location` strings and groups by exact path for batch overlap detection.               | No — pure string keys; directory paths group fine. |
| `crates/plan-tooling/src/split_prs.rs:584`                             | Collects `task.location` paths into `location_paths` records and propagates as-is into split output. | No — passes strings through verbatim.              |
| `crates/plan-tooling/src/artifact_audit.rs`                            | Does not inspect `task.location`; only uses the word "location" in prose.                            | N/A.                                               |
| `crates/plan-tooling/src/to_json.rs` (via `parse::Task` serialization) | Serializes `task.location` as a string array.                                                        | No — caller responsibility.                        |

Verdict: **No file-only assumption.** Safe to relax the validator without touching downstream
consumers. Behavior change landed inside `validate.rs` alone (new emitter
`Location directory not found`, new catalog class `location-directory-missing`).

## Decision: F8 `--watch` deferred at the gate

The task description explicitly opens with a decision gate: re-evaluate whether `--fix` reduces
iteration cost enough to defer `--watch`.

- F7 (`validate --fix`) rewrites every mechanical violation captured in the review-source 10-Edit
  sequence (`1.1, 1.2` → multi-line `Task` bullets; `[path](path)` → bare path; `` `path` `` → bare
  path) and is verified idempotent across 13 fixtures.
- Adding `notify` was flagged in the plan's risk register as a substantial workspace dependency bump
  (build time + binary size). No downstream consumer has asked for `--watch` since the source doc
  was filed; the current friction signal is for fewer edit-validate cycles, not real-time
  monitoring.
- Acceptance criterion for F8 explicitly permits a "defer" outcome when the decision and rationale
  are recorded.

Outcome: **defer F8.** Followup ticket can revisit after a real-world run of `--fix` confirms the
iteration cost is acceptable. If `--watch` is later adopted, prerequisites are unchanged (catalog
access already exposed for spec; `--fix` already wires bundle-wide rewriting).

## Validation

| Command                                         | Status  | Summary                                                | Artifact    |
| ----------------------------------------------- | ------- | ------------------------------------------------------ | ----------- |
| `cargo test -p nils-plan-tooling` (lib)         | pass    | 82 unit tests green after F7 lands                     | local run   |
| `cargo test -p nils-plan-tooling` (integration) | pass    | 102 integration tests green after F7 lands             | local run   |
| `zsh -n completions/zsh/_plan-tooling`          | pass    | completions regenerated after each new flag/subcommand | local run   |
| `bash -n completions/bash/plan-tooling`         | pass    | completions regenerated after each new flag/subcommand | local run   |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh` | pending | will run as part of release workflow gate              | release run |

## Blockers

- none

## Session Log

### 2026-05-19

- Read: plan, review-source, AGENTS.md, DEVELOPMENT.md, bundle.rs, validate.rs, parse.rs,
  parse/to_json.rs, batches.rs, split_prs.rs, artifact_audit.rs, completion.rs, usage.rs,
  integration test fixtures, tests/integration.rs, scripts/ci/nils-cli-checks-entrypoint.sh,
  `.agents/skills/nils-cli-bump-version-tag-release/SKILL.md`.
- Changed: crates/plan-tooling/src/{validate.rs, bundle.rs, parse.rs, batches.rs, split_prs.rs,
  completion.rs, usage.rs, lib.rs}; new files crates/plan-tooling/src/{spec.rs, fix.rs};
  completions/{zsh/\_plan-tooling, bash/plan-tooling}; integration tests under
  crates/plan-tooling/tests/integration/{validate.rs, to_json.rs, spec.rs} and tests/integration.rs.
- Validated: `cargo test -p nils-plan-tooling` 82+102 green; `zsh -n` / `bash -n` on regenerated
  completions; bundle.rs unit tests cover the four documented label variants; fix.rs property test
  confirms fixed-point across 13 fixtures.
- Blocked by: —
- Next: run nils-cli-bump-version-tag-release v0.9.0.
