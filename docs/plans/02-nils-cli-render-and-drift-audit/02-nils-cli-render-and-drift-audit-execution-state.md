# Phase 1.5 — agent-runtime Render Engine and Minimal Drift Audit Execution State

## Current State

- Status: in-progress
- Target scope: whole plan
- Execution window: sprint-by-sprint (one feature PR per sprint, `/code-review-specialists` review between sprints, merge before next sprint)
- Staged execution confirmation: confirmed 2026-05-20 (Sprint 1 only first, then 2/3/4 in order)
- Current task: Sprint 1 close (PR open + review)
- Next task: Task 2.1 (after Sprint 1 PR merges)
- Last updated: 2026-05-21
- Branch/commit: `feat/agent-runtime-render-engine`
- Source document: docs/plans/02-nils-cli-render-and-drift-audit/02-nils-cli-render-and-drift-audit-plan.md
- Direct source-doc execution waiver: not applicable

## Active drift decisions

- **Sprint 4 version**: plan assumed `0.0.1-dev → 0.1.0`. Reality: `agent-runtime-cli` already ships at workspace `0.12.0` after Plan 01 Sprint 3's coupled-workspace bump and the homebrew tap is at `0.12.0`. Resolved with the user 2026-05-20: Sprint 4 bumps the workspace `0.12.0 → 0.13.0` via `/nils-cli:bump-version-tag-release`, and Task 4.3 pins `required_clis['agent-runtime']` in `graysurf/agent-runtime-kit` to `">=0.13.0"`.
- **nils-common path API**: Plan 02's Task 1.2 description assumes `nils-common` already exposes path-resolution primitives. Reality: no path module in `nils-common` today. Implemented sandboxed join inside `agent-runtime-cli` (`render::writer::sandboxed_join`) as a local primitive. Refactor to `nils-common` is non-blocking and can land later.

## Task Ledger

| ID       | Status    | Task                                                         | Evidence                                                                                                                            | Notes                                                                |
| -------- | --------- | ------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------- |
| Task 1.1 | done      | Wire manifest ingest and the `--source-root` flag            | `cargo test -p agent-runtime-cli render::manifest` (8/8 pass), smoke render against `~/Project/graysurf/agent-runtime-kit`           | Lib + module layout; 5 typed manifest structs; canonicalized SourceRoot |
| Task 1.2 | done      | Register the four Tera helpers                               | `cargo test -p agent-runtime-cli render::helpers` (15/15 pass)                                                                       | script / skill_ref / state_out (runtime mode) / cli_ref; literal-mode state_out errors with Plan-04 reference |
| Task 1.3 | done      | Write `build/<product>/` output and per-skill cache           | `cargo test -p agent-runtime-cli render::writer` (8/8 pass), `render::cache` (4/4 pass)                                              | SHA-256 keyed `.render-cache.json`; sandboxed_join rejects `..` and absolute paths; cache hit byte-identical to cache miss |
| Task 1.4 | done      | Add `--update-golden` flag                                   | `cargo test -p agent-runtime-cli render::golden` (4/4 pass), smoke run with `--update-golden`                                        | Per-product subtree scope; sentinel test confirms no writes outside `tests/golden/<product>/` |
| Task 2.1 | pending   | Add determinism clippy lints to affected crates              | n/a                                                                                                                                  | scoped per Open Question default                                     |
| Task 2.2 | pending   | Add cross-process render determinism integration test        | n/a                                                                                                                                  | depends on 1.3 and 2.1                                               |
| Task 2.3 | pending   | Document the only sanctioned time value                      | n/a                                                                                                                                  | depends on 2.1                                                       |
| Task 3.1 | pending   | Source-manifest validity and rendered-target diff classes    | n/a                                                                                                                                  | depends on 1.3                                                       |
| Task 3.2 | pending   | `$AGENT_HOME` leak class (blocking, exit 2)                  | n/a                                                                                                                                  | depends on 3.1                                                       |
| Task 3.3 | pending   | Docs-home per product class (blocking, exit 2)               | n/a                                                                                                                                  | depends on 3.1                                                       |
| Task 3.4 | pending   | Audit-drift fixture set                                      | n/a                                                                                                                                  | depends on 3.1/3.2/3.3                                               |
| Task 4.1 | pending   | Bump workspace 0.12.0 → 0.13.0; add publish-order + release.yml binary | n/a                                                                                                                          | reshaped per Sprint 4 drift decision                                 |
| Task 4.2 | pending   | Bump `homebrew-tap` formula                                  | n/a                                                                                                                                  | depends on 4.1                                                       |
| Task 4.3 | pending   | Cross-repo: bump `required_clis['agent-runtime']` to `">=0.13.0"` | n/a                                                                                                                             | cross-repo PR against `graysurf/agent-runtime-kit`; depends on 4.2   |

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `plan-tooling validate --file docs/plans/02-nils-cli-render-and-drift-audit/02-nils-cli-render-and-drift-audit-plan.md --format text --explain` | done | bundle gate before first commit | exit 0 |
| `cargo test -p agent-runtime-cli render` | done | Sprint 1 close — 43/43 lib tests + 4/4 integration tests pass | local run |
| `cargo clippy -p agent-runtime-cli --all-targets -- -D warnings` | done | Sprint 1 close — clippy clean (Sprint 2 adds determinism lints to gate) | local run |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh` | done | Sprint 1 close — required checks suite green after THIRD_PARTY artifact regeneration + cargo fmt | local run |
| `cargo clippy -p agent-runtime-cli -p nils-common --all-targets -- -D warnings` | pending | Sprint 2 close | n/a |
| `cargo test -p agent-runtime-cli render_determinism` | pending | Sprint 2 close | n/a |
| `cargo test -p agent-runtime-cli audit_drift` | pending | Sprint 3 close | n/a |
| `cargo test -p agent-runtime-cli audit_drift_classes` | pending | Sprint 3 close | n/a |
| `brew audit --strict --online sympoies/tap/nils-cli` | pending | Sprint 4, after Task 4.2 | n/a |
| `agent-runtime audit-drift --source-root ~/Project/graysurf/agent-runtime-kit` | pending | Sprint 4, after Task 4.3 | n/a |

## Blockers

- None active. Plan 01 in `graysurf/agent-runtime-kit` reached Sprint 4 (commit `29c51b5`); five Phase 1 manifests + schemas + stub `agent-runtime-cli` crate at workspace `0.12.0` are in place.

## Session Log

- 2026-05-20 — Sprint 1 execution starts. Branch `feat/agent-runtime-render-engine` created off `main` (clean tree). Issue #401 state initialized via `execute-from-tracking-issue:state:v1` comment; dashboard refreshed. Cross-repo `agent-runtime-kit` repo verified at `~/Project/graysurf/agent-runtime-kit`.
- 2026-05-20 — Task 1.1 lands: `src/lib.rs` exposes `Cli` + `Command` + `run()`; new `commands/` and `render/` module trees; `render::manifest` defines five typed manifests deserialized with `serde_yml` + `deny_unknown_fields`. Workspace deps gain `indexmap`, `serde_yml`, `sha2`, `tera`. Integration test `cli.rs` splits `STUB_SUBCOMMANDS` from `ALL_SUBCOMMANDS` so `render` no longer prints `not implemented` but every other subcommand still does.
- 2026-05-20 — Task 1.2 lands: `render::helpers::{script, skill_ref, state_out, cli_ref}` registered via `register_all`. Helper signature uses `&HashMap` from Tera (the only sanctioned `HashMap` import inside `src/render/`); test_support provides shared fixtures and a chain-walking `format_err` so rejection assertions hit the underlying helper message rather than Tera's `Failed to render '__tera_one_off'` wrapper.
- 2026-05-21 — Task 1.3 lands: `render::writer::write_product` + `render::cache::RenderCache`. SHA-256 hash spans skill id + product + render target + template body + every Phase 1 manifest's raw bytes. Cache file is `BTreeMap`-backed JSON so on-disk bytes are stable. `sandboxed_join` rejects `..` and absolute paths; eight writer tests cover render output, cache hit/miss byte equality, template-change invalidation, skipped products, unknown product, sandboxed_join rejections, and the empty-skills.yaml smoke case.
- 2026-05-21 — Task 1.4 lands: `render::golden::update_golden` reads the `RenderReport`, copies every touched skill's render-dir into `tests/golden/<product>/<...>/expected/`. Sentinel test confirms the active-product subtree is the only one mutated. `--update-golden` flag wired into `commands::render::RenderArgs`.
- 2026-05-21 — Sprint 1 close: regenerated THIRD_PARTY artifacts (new `tera`, `serde_yml`, `sha2`, `indexmap` transitive deps); ran `cargo fmt --all`; `bash scripts/ci/nils-cli-checks-entrypoint.sh` exits 0; `plan-tooling validate` exits 0. Opening feature PR via `pr:create-feature-pr`.
