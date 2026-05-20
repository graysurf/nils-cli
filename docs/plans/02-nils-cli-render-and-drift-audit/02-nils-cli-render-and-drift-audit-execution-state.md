# Phase 1.5 — agent-runtime Render Engine and Minimal Drift Audit Execution State

## Current State

- Status: not started
- Target scope: whole plan
- Execution window: undecided
- Staged execution confirmation: not applicable
- Current task: Task 1.1
- Next task: Task 1.1
- Last updated: 2026-05-20
- Branch/commit: not started
- Source document:
  docs/plans/02-nils-cli-render-and-drift-audit/02-nils-cli-render-and-drift-audit-plan.md
- Direct source-doc execution waiver: not applicable

## Task Ledger

| ID       | Status  | Task                                                              | Evidence | Notes                                                              |
| -------- | ------- | ----------------------------------------------------------------- | -------- | ------------------------------------------------------------------ |
| Task 1.1 | pending | Wire manifest ingest and the `--source-root` flag                 | n/a      | depends on Plan 01 stub crate being present                        |
| Task 1.2 | pending | Register the four Tera helpers                                    | n/a      | depends on 1.1                                                     |
| Task 1.3 | pending | Write `build/<product>/` output and per-skill cache               | n/a      | depends on 1.2                                                     |
| Task 1.4 | pending | Add `--update-golden` flag                                        | n/a      | depends on 1.3                                                     |
| Task 2.1 | pending | Add determinism clippy lints to affected crates                   | n/a      | scoped per Open Question default                                   |
| Task 2.2 | pending | Add cross-process render determinism integration test             | n/a      | depends on 1.3 and 2.1                                             |
| Task 2.3 | pending | Document the only sanctioned time value                           | n/a      | depends on 2.1                                                     |
| Task 3.1 | pending | Source-manifest validity and rendered-target diff classes         | n/a      | depends on 1.3                                                     |
| Task 3.2 | pending | `$AGENT_HOME` leak class (blocking, exit 2)                       | n/a      | depends on 3.1                                                     |
| Task 3.3 | pending | Docs-home per product class (blocking, exit 2)                    | n/a      | depends on 3.1                                                     |
| Task 3.4 | pending | Audit-drift fixture set                                           | n/a      | depends on 3.1/3.2/3.3                                             |
| Task 4.1 | pending | Tag `agent-runtime-cli` v0.1.0 and update release config          | n/a      | depends on 2.2 and 3.4                                             |
| Task 4.2 | pending | Bump `homebrew-tap` formula                                       | n/a      | depends on 4.1                                                     |
| Task 4.3 | pending | Cross-repo: bump `required_clis` floors in agent-runtime-kit      | n/a      | cross-repo PR against agent-runtime-kit; depends on 4.2            |

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `plan-tooling validate --file docs/plans/02-nils-cli-render-and-drift-audit/02-nils-cli-render-and-drift-audit-plan.md --strict --format text --explain` | pending | bundle gate before first commit | n/a |
| `cargo test -p agent-runtime-cli render` | pending | Sprint 1 close | n/a |
| `cargo clippy -p agent-runtime-cli -p nils-common --all-targets -- -D warnings` | pending | Sprint 2 close | n/a |
| `cargo test -p agent-runtime-cli render_determinism` | pending | Sprint 2 close | n/a |
| `cargo test -p agent-runtime-cli audit_drift` | pending | Sprint 3 close | n/a |
| `cargo test -p agent-runtime-cli audit_drift_classes` | pending | Sprint 3 close | n/a |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh` | pending | Sprint 2, 3, 4 close | n/a |
| `brew audit --strict --online sympoies/tap/nils-cli` | pending | Sprint 4, after Task 4.2 | n/a |
| `agent-runtime audit-drift --source-root ../agent-runtime-kit` | pending | Sprint 4, after Task 4.3 | n/a |

## Blockers

- Plan 01 in `sympoies/agent-runtime-kit` must reach `done` before
  Task 1.1 can start (need manifest schemas and the
  `crates/agent-runtime-cli/` shell crate).

## Session Log

(none yet)
