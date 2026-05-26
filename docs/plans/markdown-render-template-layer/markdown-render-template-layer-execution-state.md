# Markdown Render Template Layer Execution State

## Execution State

- Status: in-progress (Sprint 1 implementation landed; PR #542 awaiting required checks + review)
- Target scope: whole plan
- Execution window: Sprint 1 → Sprint 2 (9 PRs) → Sprint 3 (4 PRs)
- Staged execution confirmation: Sprint 1 amended and implemented;
  Sprint 2 begins after PR #542 merges
- Current task: Sprint 1 review (PR #542)
- Next task: Task 2.1 — Migrate `plan-issue-cli/src/issue_body.rs`
  (Sprint 2, gated on PR #542 merging)
- Last updated: 2026-05-26
- Branch/commit: feat/markdown-render-template-layer-plan @ 3ace5d4
- PR: <https://github.com/sympoies/nils-cli/pull/542>
- Source document:
  docs/plans/markdown-render-template-layer/markdown-render-template-layer-plan.md
- Direct source-doc execution waiver: not applicable

## Task Ledger

| ID       | Status | Task                                                               | Evidence | Notes                                                                           |
| -------- | ------ | ------------------------------------------------------------------ | -------- | ------------------------------------------------------------------------------- |
| Task 1.1 | done   | Scaffold `nils-markdown` crate                                     | PR #542  | commit 1413855                                                                  |
| Task 1.2 | done   | Engine builder and `RenderError`                                   | PR #542  | 18 unit tests; thiserror Send+Sync+'static                                      |
| Task 1.3 | done   | `md_cell` filter and helper bridge                                 | PR #542  | 11 tests covering pipe escape / newline collapse / re-export parity             |
| Task 1.4 | done   | Migrate `agent-runtime-cli` engine construction to `nils-markdown` | PR #542  | helpers stay in place; engine through `Engine::register_helper`; 138 tests pass |
| Task 1.5 | done   | New byte-equality `assert_render` harness                          | PR #542  | gated by `test-support` feature; smoke covers match + drift                     |
| Task 1.6 | done   | Workspace gate                                                     | PR #542  | nextest 3987/3987; docs-only entrypoint pass                                    |
| Task 2.1 | todo   | Migrate `plan-issue-cli/src/issue_body.rs`                         |          |                                                                                 |
| Task 2.2 | todo   | Migrate `plan-issue-cli/src/task_spec.rs`                          |          |                                                                                 |
| Task 2.3 | todo   | Migrate `plan-issue-cli/src/lifecycle_vnext/templates.rs`          |          |                                                                                 |
| Task 2.4 | todo   | Migrate `plan-issue-cli/src/render.rs`                             |          |                                                                                 |
| Task 2.5 | todo   | Migrate `plan-issue-cli/src/lifecycle_record.rs`                   |          |                                                                                 |
| Task 2.6 | todo   | Migrate `plan-issue-cli/src/execute.rs`                            |          |                                                                                 |
| Task 2.7 | todo   | Migrate `agent-workflow-primitives/src/review_specialists.rs`      |          |                                                                                 |
| Task 2.8 | todo   | Migrate `agent-workflow-primitives/src/repo_retro.rs`              |          |                                                                                 |
| Task 2.9 | todo   | Migrate `agent-workflow-primitives/src/heuristic_inbox.rs`         |          |                                                                                 |
| Task 3.1 | todo   | Implement `md-render` binary main                                  |          |                                                                                 |
| Task 3.2 | todo   | Completion assets and workspace registration                       |          |                                                                                 |
| Task 3.3 | todo   | README + CHANGELOG + agent-docs entry                              |          |                                                                                 |
| Task 3.4 | todo   | Promote design principle to runbook                                |          |                                                                                 |

## Validation

| Command | Status | Summary | Artifact |
| ------- | ------ | ------- | -------- |
| `cargo test -p nils-markdown` | pass | 31 unit + 2 integration tests | PR #542 |
| `cargo test -p agent-runtime-cli` | pass | 138 tests, no fixture edits | PR #542 |
| `cargo nextest run --workspace` | pass | 3987 passed | PR #542 |
| `cargo clippy -p nils-markdown -p agent-runtime-cli --all-targets --all-features -- -D warnings` | pass | scoped clippy | PR #542 |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` | pass | docs hygiene, placement, rumdl, plan-bundle | PR #542 |
| `bash scripts/ci/third-party-artifacts-audit.sh --strict` | pass | regenerated after Cargo.lock churn | PR #542 |
| `bash scripts/ci/completion-asset-audit.sh --strict` | pass | 36 required / 38 workspace bins | PR #542 |
| `cargo build --workspace --locked` | pass | Cargo.lock locked-build | PR #542 |
| `plan-tooling validate` | pass | exit 0 on plan bundle | PR #542 |
| `cargo clippy --workspace --all-targets -- -D warnings` | fail (pre-existing) | 8 `collapsible_if` in `plan-issue-cli/src/execute.rs` + 1 in `lifecycle_vnext/registry.rs`; reproduces on `1413855` with branch changes stashed | out of Sprint 1 scope |
| `scripts/publish-crates.sh --dry-run` | fail (pre-existing) | `nils-common 0.23.0` not on crates.io (max 0.8.3); same failure for `nils-cli-template` | out of Sprint 1 scope |

## Blockers

- none

## Session Log

- 2026-05-26 (amend): Pre-execution scoping found two factual gaps
  between the plan and `agent-runtime-cli/src/render/`: Task 1.4
  framed the four helpers (`cli_ref / script / skill_ref /
  state_out`) as relocatable, but each binds `ManifestSet / Skill /
  StateOutMode / CliToolsManifest` and cannot move into the lowest
  layer; Task 1.5 named `agent-runtime-cli/src/render/golden.rs` as
  a byte-equality helper, but that file is the `--update-golden`
  fixture-refresh mode. Both tasks rewritten on
  `feat/markdown-render-template-layer-plan` (commit e297751):
  Sprint 1 now exposes `Engine::register_helper(name, F)` for
  consumers and adds a new `nils_markdown::golden::assert_render`
  helper; the four agent-runtime-cli helpers stay in place. Plan
  validates with `plan-tooling validate` exit 0.
- 2026-05-26 (Task 1.1): Scaffold `crates/nils-markdown/` with lib,
  README, and docs index; slot after `nils-term` in publish order;
  defer `[[bin]]` declaration to Sprint 3 Task 3.1 so `cargo
  metadata` does not advertise a no-op binary. Commit 1413855.
- 2026-05-26 (Tasks 1.2–1.6): Implement `Engine` (deterministic
  builder, `register_template / render_value / render / render_str
  / register_helper`), `RenderError`, `md_cell` filter, helper
  bridge, and `golden::assert_render` (behind `test-support`
  feature). Migrate `agent-runtime-cli/src/render/writer.rs` +
  `helpers/mod.rs` to construct engines via `nils_markdown::Engine`
  and register the four domain helpers through
  `Engine::register_helper`; `tera::Tera` import is gone from
  `writer.rs`. `agent-runtime-cli` golden fixtures unchanged.
  Workspace gates green (nextest 3987, scoped clippy, docs-only
  entrypoint, third-party-artifacts after regen, completion asset
  audit, Cargo.lock locked-build); two workspace gates fail
  pre-existing on `main` (full clippy, publish-crates dry-run) and
  are documented in the Validation table. Commit 3ace5d4.
- 2026-05-26 (Sprint 1 PR): Opened PR #542 against `main` with
  labels `type::feature`, `area::runtime`, `size::l`,
  `workflow::tracking`.
