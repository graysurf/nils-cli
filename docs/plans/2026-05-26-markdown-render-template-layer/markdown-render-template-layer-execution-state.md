# Markdown Render Template Layer Execution State

## Execution State

- Status: done (Sprint 1 + Sprint 2 + Sprint 3 implemented; PRs #542–#553
  merged into `main`)
- Target scope: whole plan
- Execution window: Sprint 1 (1 PR) → Sprint 2 (11 PRs incl. one plan
  rescope) → Sprint 3 (1 PR)
- Staged execution confirmation: Sprint 1 amended and implemented;
  Sprint 2 executed task-by-task with two scoping amendments (Task 2.2
  rescoped to a TSV/Markdown audit; Task 2.5 split into 2.5/2.5b);
  Sprint 3 bundled into a single PR
- Current task: tracking-issue closeout (`plan-issue record close`)
- Next task: none — closeout pending
- Last updated: 2026-05-26
- Branch/commit: `main` @ a608586
- PR: <https://github.com/sympoies/nils-cli/pull/553> (last; full list in
  Session Log)
- Source document:
  docs/plans/markdown-render-template-layer/markdown-render-template-layer-plan.md
- Direct source-doc execution waiver: not applicable

## Task Ledger

| ID        | Status | Task                                                                               | Evidence | Notes                                                                               |
| --------- | ------ | ---------------------------------------------------------------------------------- | -------- | ----------------------------------------------------------------------------------- |
| Task 1.1  | done   | Scaffold `nils-markdown` crate                                                     | PR #542  | commit 1413855                                                                      |
| Task 1.2  | done   | Engine builder and `RenderError`                                                   | PR #542  | 18 unit tests; thiserror Send+Sync+'static                                          |
| Task 1.3  | done   | `md_cell` filter and helper bridge                                                 | PR #542  | 11 tests covering pipe escape / newline collapse / re-export parity                 |
| Task 1.4  | done   | Migrate `agent-runtime-cli` engine construction to `nils-markdown`                 | PR #542  | helpers stay in place; engine through `Engine::register_helper`; 138 tests pass     |
| Task 1.5  | done   | New byte-equality `assert_render` harness                                          | PR #542  | gated by `test-support` feature; smoke covers match + drift                         |
| Task 1.6  | done   | Workspace gate                                                                     | PR #542  | nextest 3987/3987; docs-only entrypoint pass                                        |
| Task 2.1  | done   | Migrate `plan-issue-cli/src/issue_body.rs` task-decomposition block                | PR #543  | per-row helper retained for TaskTable; golden fixtures unchanged                    |
| Task 2.2  | done   | Confirm `plan-issue-cli/src/task_spec.rs` TSV / Markdown boundary                  | PR #544  | rescope amendment; `task_spec.rs` emits TSV (not Markdown), no template needed      |
| Task 2.3  | done   | Migrate `plan-issue-cli/src/lifecycle_vnext/templates.rs`                          | PR #545  | 7 role templates under `templates/lifecycle_vnext.md.tera`; per-role golden tests   |
| Task 2.4  | done   | Migrate `plan-issue-cli/src/render.rs`                                             | PR #546  | both Markdown emitters in one PR; goldens byte-equal pre/post                       |
| Task 2.5  | done   | Migrate the dashboard renderers in `lifecycle_record.rs`                           | PR #547  | dashboard-only first slice (plan amendment); engine-glue isolated                   |
| Task 2.5b | done   | Migrate the snapshot, post-comment, and kind helpers in `lifecycle_record.rs`      | PR #548  | adds `serde_json/preserve_order` to crate to match `--workspace` feature union      |
| Task 2.6  | done   | Migrate `plan-issue-cli/src/execute.rs`                                            | PR #549  | execute.rs Markdown emitters routed through `nils_markdown::Engine`                 |
| Task 2.7  | done   | Migrate `agent-workflow-primitives/src/review_specialists.rs`                      | PR #550  | adds `nils-markdown` transitive dep; third-party artifacts regenerated              |
| Task 2.8  | done   | Migrate `agent-workflow-primitives/src/repo_retro.rs`                              | PR #551  | trailing-newline-in-block pattern via `format_bullet_block` helper                  |
| Task 2.9  | done   | Migrate `agent-workflow-primitives/src/heuristic_inbox.rs` (archive + next_action) | PR #552  | scope narrowed to two emitters per plan amendment                                   |
| Task 3.1  | done   | Implement `md-render` binary main                                                  | PR #553  | bundled with 3.2 / 3.3 / 3.4                                                        |
| Task 3.2  | done   | Completion assets and workspace registration                                       | PR #553  | bash + zsh completions; bin gated behind `bin-cli` feature                          |
| Task 3.3  | done   | README + CHANGELOG + agent-docs entry                                              | PR #553  | crate README, `CHANGELOG.md` for 0.23.0, completion matrix row                      |
| Task 3.4  | done   | Promote design principle to runbook                                                | PR #553  | `docs/runbooks/markdown-template-development-standard.md`; docs-placement allowlist |

## Validation

| Command | Status | Summary | Artifact |
| ------- | ------ | ------- | -------- |
| `cargo test -p nils-markdown` | pass | 31 unit + 2 integration tests | PR #542 |
| `cargo test -p agent-runtime-cli` | pass | 138 tests, no fixture edits | PR #542 |
| `cargo nextest run --workspace` | pass | green on every Sprint 2 + Sprint 3 PR | PR #542 → #553 |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | pass | clean post-Sprint-2 (the pre-existing `collapsible_if` warnings in `execute.rs` were resolved as a side effect of the Task 2.6 migration) | PR #549, PR #553 |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` | pass | docs hygiene, placement, rumdl, plan-bundle | every PR in stream |
| `bash scripts/ci/third-party-artifacts-audit.sh --strict` | pass | regenerated when new dep edges added (Task 2.7, Task 3.x) | PR #550, PR #553 |
| `bash scripts/ci/completion-asset-audit.sh --strict` | pass | 37 required after Sprint 3 `md-render` row | PR #553 |
| `bash scripts/ci/completion-flag-parity-audit.sh --strict` | pass | required `--all-features` to materialize the feature-gated `md-render` binary | PR #553 |
| `cargo build --workspace --locked` | pass | Cargo.lock locked-build | every PR in stream |
| `plan-tooling validate` | pass | exit 0 on plan bundle after every amendment | PR #544 (rescope), PR #547/#548 (Task 2.5/2.5b split), PR #552 (Task 2.9 scope) |
| `scripts/publish-crates.sh --dry-run` | fail (pre-existing) | `nils-common 0.23.0` not on crates.io (max 0.8.3); same failure for `nils-cli-template` | out of plan scope |

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
  `workflow::tracking`. Merged 04:49Z.
- 2026-05-26 (Task 2.1): Migrate the task-decomposition block in
  `plan-issue-cli/src/issue_body.rs` to a `nils-markdown` template;
  per-row helper retained for `TaskTable::render`. Golden fixtures
  unchanged. Merged as PR #543 (05:10Z).
- 2026-05-26 (Task 2.2 rescope): Audit found `task_spec.rs` only
  emits TSV (not Markdown) and therefore does not belong in the
  Markdown render layer. Plan amended to recharacterise Task 2.2 as
  a confirmation/audit step. `plan-tooling validate` exit 0.
  Merged as PR #544 (05:28Z).
- 2026-05-26 (Task 2.3): Move the seven `lifecycle_vnext` role
  templates into `crates/plan-issue-cli/templates/lifecycle_vnext.md.tera`;
  `lifecycle_vnext/templates.rs` collapses to view structs + engine
  glue. New per-role byte-equality fixtures under
  `tests/golden/lifecycle_vnext/`. Merged as PR #545 (05:47Z).
- 2026-05-26 (Task 2.4): Migrate both Markdown emitters in
  `plan-issue-cli/src/render.rs` in one PR (per user direction);
  goldens byte-equal pre/post. Merged as PR #546 (06:09Z).
- 2026-05-26 (Task 2.5 split): Plan amended mid-execution to split
  Task 2.5 into 2.5 (dashboard-only first slice) and 2.5b
  (snapshot + post_comment + kind helpers) to ship a smaller
  reviewable diff. `plan-tooling validate` exit 0 after the split.
- 2026-05-26 (Task 2.5): Migrate the dashboard renderers in
  `lifecycle_record.rs`. Merged as PR #547 (06:40Z).
- 2026-05-26 (Task 2.5b): Migrate the snapshot, post_comment, and
  kind helpers. CI surfaced a workspace-feature-unification trap:
  `cargo nextest run --workspace` activates
  `serde_json/preserve_order` via downstream crates, but per-crate
  `cargo test -p plan-issue-cli` does not, so 3 post_comment tests
  failed under per-crate runs. Pinned `serde_json` with the
  `preserve_order` feature in `crates/plan-issue-cli/Cargo.toml`
  and re-blessed 8 fixtures from a workspace build. Merged as PR
  #548 (07:15Z).
- 2026-05-26 (Task 2.6): Migrate `plan-issue-cli/src/execute.rs`
  Markdown emitters; pre-existing `collapsible_if` warnings in the
  same file are resolved as a side effect of the template
  rewrite. Merged as PR #549 (07:34Z).
- 2026-05-26 (Task 2.7): Migrate
  `agent-workflow-primitives/src/review_specialists.rs`; adds
  `nils-markdown` as a transitive dependency of the
  `agent-workflow-primitives` crate. Regenerated
  THIRD_PARTY_LICENSES.md / NOTICES.md. Merged as PR #550 (07:59Z).
- 2026-05-26 (Task 2.8): Migrate
  `agent-workflow-primitives/src/repo_retro.rs`. Adopted the
  trailing-newline-in-block pattern via a `format_bullet_block`
  helper to keep empty-themes vs populated-themes blank-line
  behaviour byte-equal. Merged as PR #551 (08:25Z).
- 2026-05-26 (Task 2.9 scope amendment): Plan narrowed to cover
  only the `heuristic_inbox` archive + next_action emitters; the
  inbox-line bullet emitter is a one-line helper not worth a
  template indirection. `plan-tooling validate` exit 0.
- 2026-05-26 (Task 2.9): Migrate the two scoped emitters. Merged
  as PR #552 (08:42Z).
- 2026-05-26 (Sprint 3, Tasks 3.1–3.4): Bundle into a single PR.
  Implement `md-render` binary main (clap derive CLI behind
  `bin-cli` feature; `render` + `completion` subcommands; default
  `render` flatten); add bash + zsh completion fixtures and
  workspace registration; populate crate README + `CHANGELOG.md`
  for 0.23.0; promote the design principle to
  `docs/runbooks/markdown-template-development-standard.md` and
  allowlist it in `scripts/ci/docs-placement-audit.sh`. CI
  surfaced two more traps: the `completion-flag-parity-audit.sh`
  uses `cargo build --workspace --bins` which does not materialise
  feature-gated binaries, fixed by adding `--all-features`; the
  binary integration test must resolve the path via
  `nils_test_support::bin::resolve("md-render")` so coverage runs
  pointing at `target/llvm-cov-target` work. Merged as PR #553
  (09:24Z).
