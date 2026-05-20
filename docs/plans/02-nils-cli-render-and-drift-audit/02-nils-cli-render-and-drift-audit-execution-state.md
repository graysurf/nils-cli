# Phase 1.5 — agent-runtime Render Engine and Minimal Drift Audit Execution State

## Current State

- Status: in-progress
- Target scope: whole plan
- Execution window: sprint-by-sprint (one feature PR per sprint, `/code-review-specialists`
  review between sprints, merge before next sprint)
- Staged execution confirmation: confirmed 2026-05-20 (Sprint 1 only first, then 2/3/4 in order)
- Current task: Sprint 3 close (PR open + review)
- Next task: Sprint 4 — release 0.13.0 + tap bump + cross-repo floor (after Sprint 3 PR merges)
- Last updated: 2026-05-21
- Branch/commit: `feat/agent-runtime-audit-drift`
- Source document: docs/plans/02-nils-cli-render-and-drift-audit/02-nils-cli-render-and-drift-audit-plan.md
- Direct source-doc execution waiver: not applicable

## Active drift decisions

- **Sprint 4 version**: plan assumed `0.0.1-dev → 0.1.0`. Reality:
  `agent-runtime-cli` already ships at workspace `0.12.0` after Plan 01
  Sprint 3's coupled-workspace bump and the homebrew tap is at `0.12.0`.
  Resolved with the user 2026-05-20: Sprint 4 bumps the workspace
  `0.12.0 → 0.13.0` via `/nils-cli:bump-version-tag-release`, and Task 4.3
  pins `required_clis['agent-runtime']` in `graysurf/agent-runtime-kit`
  to `">=0.13.0"`.
- **nils-common path API**: Plan 02's Task 1.2 description assumes
  `nils-common` already exposes path-resolution primitives. Reality: no
  path module in `nils-common` today. Implemented sandboxed join +
  symlink-escape guard inside `agent-runtime-cli`
  (`render::writer::sandboxed_join`, `canonicalize_under`,
  `guard_write_under`) as local primitives. Lift to `nils-common`
  remains non-blocking.

## Task Ledger

| ID  | Status  | Task                                                            | Notes                                                                              |
| --- | ------- | --------------------------------------------------------------- | ---------------------------------------------------------------------------------- |
| 1.1 | done    | Wire manifest ingest and the `--source-root` flag               | Lib + module layout; 5 typed manifest structs; canonicalized SourceRoot            |
| 1.2 | done    | Register the four Tera helpers                                  | script / skill_ref / state_out (runtime) / cli_ref; literal-mode errors to Plan 04 |
| 1.3 | done    | Write `build/<product>/` output and per-skill cache             | SHA-256 keyed `.render-cache.json`; cache hit byte-identical to cache miss         |
| 1.4 | done    | Add `--update-golden` flag                                      | Per-product subtree scope; sentinel test confirms no writes outside active product |
| 2.1 | done    | Add determinism clippy lints to affected crates                 | clippy.toml + `#![deny(clippy::disallowed_types, clippy::disallowed_methods)]`     |
| 2.2 | done    | Add cross-process render determinism integration test           | Two separate `agent-runtime` invocations, cache deleted between, byte-equal walk   |
| 2.3 | done    | Document the only sanctioned time value                         | `render::time::source_commit_timestamp` + `docs/determinism.md` contract           |
| 3.1 | done    | Source-manifest validity and rendered-target diff classes       | `audit_drift::{source_manifest,rendered_target}`; warn-tier (exit 1)               |
| 3.2 | done    | `$AGENT_HOME` leak class (blocking, exit 2)                     | `audit_drift::agent_home_leak`; product-build + source-tree scans; allowlist       |
| 3.3 | done    | Docs-home per product class (blocking, exit 2)                  | `audit_drift::docs_home`; table-driven product → expected docs-home                |
| 3.4 | done    | Audit-drift fixture set                                         | 5 integration tests off render-determinism fixture; one per class + clean baseline |
| 4.1 | pending | Workspace bump 0.12.0 → 0.13.0; publish order + release.yml bin | reshaped per Sprint 4 drift decision                                               |
| 4.2 | pending | Bump `homebrew-tap` formula                                     | depends on 4.1                                                                     |
| 4.3 | pending | Cross-repo: `required_clis['agent-runtime']` → `">=0.13.0"`     | PR against `graysurf/agent-runtime-kit`; depends on 4.2                            |

## Sprint 1 — local validation (pre-PR)

- `cargo test -p agent-runtime-cli` — 54 lib + 8 integration pass.
- `cargo clippy -p agent-runtime-cli --all-targets -- -D warnings` — clean.
- `cargo fmt --all -- --check` — clean.
- `bash scripts/ci/nils-cli-checks-entrypoint.sh` — exit 0 after
  `bash scripts/generate-third-party-artifacts.sh --write`.
- `plan-tooling validate --file <plan>.md --format text --explain` — exit 0
  (plan = `docs/plans/02-nils-cli-render-and-drift-audit/02-nils-cli-render-and-drift-audit-plan.md`).

Sprint 2+: clippy determinism gate, cross-process determinism test,
audit-drift body + fixtures, release/tap/cross-repo floor bump.

## Blockers

- None active. Plan 01 in `graysurf/agent-runtime-kit` reached Sprint 4
  (commit `29c51b5`); five Phase 1 manifests + schemas + stub
  `agent-runtime-cli` crate at workspace `0.12.0` are in place.

## Session Log

- 2026-05-20 — Sprint 1 execution starts. Branch
  `feat/agent-runtime-render-engine` off `main`. Issue #401 state
  initialized via `execute-from-tracking-issue:state:v1` comment;
  dashboard refreshed.
- 2026-05-20 — Task 1.1 lands: `src/lib.rs` exposes `run()`; new
  `commands/` and `render/` module trees; five typed manifest structs
  with `deny_unknown_fields` and `schema_version` validation.
  Workspace deps gain `indexmap`, `serde_yml`, `sha2`, `tera`.
- 2026-05-20 — Task 1.2 lands: `render::helpers::{script, skill_ref,
  state_out, cli_ref}` registered via `register_all`. Helper signature
  uses `&HashMap` from Tera (the only sanctioned `HashMap` import in
  `src/render/`).
- 2026-05-21 — Task 1.3 lands: `render::writer::write_product` plus
  `render::cache::RenderCache`. SHA-256 hash spans skill id, product,
  render target, template body, and every Phase 1 manifest's raw
  bytes. Cache file is `BTreeMap`-backed JSON.
- 2026-05-21 — Task 1.4 lands: `render::golden::update_golden` reads
  the `RenderReport`, copies every touched skill's render-dir into
  `tests/golden/<product>/<...>/expected/`. Sentinel test confirms
  the active-product subtree is the only one mutated.
- 2026-05-21 — Sprint 1 close: regenerated THIRD_PARTY artifacts;
  `cargo fmt --all`; `bash scripts/ci/nils-cli-checks-entrypoint.sh`
  exits 0. PR #404 opened as draft.
- 2026-05-21 — `/code-review-specialists` ran (5 specialists in
  parallel: maintainability, security, api-contract, testing,
  performance). Applied fixes: (a) `state_out` charset-validation
  rejects shell metacharacters (HIGH security); (b) symlink-escape
  guard via `canonicalize_under` + `guard_write_under` at every
  render-time file open (HIGH security); (c) closed `SkillProducts` /
  `PluginProductManifests` typed structs so unknown product keys
  fail parse instead of silent-skip (MEDIUM api-contract); (d)
  `Skill.products` no longer `#[serde(default)]` (MEDIUM
  api-contract); (e) cache `schema_version` mismatch falls back to
  empty (MEDIUM testing); (f) added symlink-escape rejection tests,
  unknown-product-key rejection tests, and four end-to-end render
  integration tests. Lib tests grew 43 → 54, integration grew 4 → 8.
  Deferred items: helper-arg dedup, `Command::name` catch-all → Sprint 2
  cleanup; remaining api-contract / testing low+info items tracked on
  the issue.
- 2026-05-21 — Sprint 3 lands on branch
  `feat/agent-runtime-audit-drift`: Task 3.1 wires
  `audit_drift::source_manifest` (typed `manifest::load_all` re-validation
  plus `<TBD` placeholder scan over the five manifest files; warn-tier)
  and `audit_drift::rendered_target` (re-render into a `TempDir` scratch
  and BTreeMap-keyed byte diff vs live `build/<product>/`; warn-tier).
  Writer refactored with `write_product_to(root, manifests, product,
  output_root)` so the diff class can redirect output without touching
  the live build tree. Tasks 3.2 + 3.3 add the block-tier classes:
  `audit_drift::agent_home_leak` scans `build/<product>/`, `core/`,
  `targets/`, `manifests/` for the literal `$AGENT_HOME` substring
  (hard-coded allowlist for `docs/source/inventory-target-architecture.md`),
  and `audit_drift::docs_home` matches `--docs-home` args per product
  against the table-driven expected value
  (`"$CODEX_HOME"` / `"$HOME/.claude"`). Task 3.4 lands
  `tests/integration/audit_drift_classes.rs` (5 tests) driving each
  fixture variant off the `render-determinism` fixture as base
  (renders both products, then mutates one surface per test before
  invoking `agent-runtime audit-drift`). The render-determinism
  fixture's `runtime-roots.yaml` Phase 1 `<TBD>` placeholders are
  pinned to 0.12.0 / 2026-05-21 so the clean baseline truly exits 0.
  Test counts: lib 59 → 70, integration 16 → 21.
- 2026-05-21 — Sprint 2 lands on branch
  `feat/agent-runtime-determinism-lints`: Task 2.1 introduces
  `clippy.toml` for `agent-runtime-cli` + `nils-common` (disallowed
  `HashMap` + `SystemTime::now` + `chrono::{Utc,Local}::now`) plus
  crate-level `#![deny(clippy::disallowed_types,
  clippy::disallowed_methods)]`. The Tera helper module gets a
  scoped `allow` (Tera forces `&HashMap` at its trait surface);
  `nils-common::fs::temp_path` gets a scoped `allow` with a comment
  marking it off the render path. Task 2.2 adds
  `tests/integration/render_determinism.rs` — two
  `std::process::Command` invocations of `agent-runtime render`
  with `.render-cache.json` deleted between runs, walking
  `build/<product>/` with `BTreeMap` and asserting byte-equal for
  both `codex` and `claude`. Fixture lives under
  `tests/fixtures/render-determinism/` (5 manifests + one
  Tera template exercising every helper). Task 2.3 adds
  `render::time::source_commit_timestamp(source_root)` (shells out
  to `git -C <root> log -1 --format=%cI HEAD`; 3 unit tests) plus
  `docs/determinism.md` documenting the 3 rules + the single
  sanctioned wall-clock escape hatch + the single sanctioned
  `HashMap` exemption. Test counts: lib 54 → 57, integration 8 →
  10. Determinism gate verified to actually fire on a temporary
  HashMap injection. `bash scripts/ci/nils-cli-checks-entrypoint.sh`
  exits 0.
