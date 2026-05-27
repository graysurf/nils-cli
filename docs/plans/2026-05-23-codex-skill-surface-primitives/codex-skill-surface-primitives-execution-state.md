<!-- execute-from-tracking-issue:state:v1 -->
# Codex Skill Surface Primitives Execution State

## Execution State

- Status: in progress
- Target scope: whole issue
- Execution window: whole issue
- Current task: final validation and delivery
- Next task: open PR, complete review, merge, and close issue #446
- Last updated: 2026-05-23 12:42 CST
- Branch/commit/PR/release: `feat/issue-446-codex-skill-surface-primitives`,
  pending commit and PR
- Tracking issue: [#446](https://github.com/sympoies/nils-cli/issues/446)
- Source document:
  docs/plans/codex-skill-surface-primitives/codex-skill-surface-primitives-plan.md
- Direct source-doc execution waiver: not applicable

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| Task 1.1 | done | Document directory-symlink contract | `cargo test -p agent-runtime-cli install` pass | Module docs and schema text now state file-or-directory acceptance. |
| Task 1.2 | done | Pin non-recursive directory-source behavior | `cargo test -p agent-runtime-cli install` pass | Tests cover plan shape, apply idempotence, and real-directory refusal. |
| Task 1.3 | done | Label dry-run link modes | `cargo test -p agent-runtime-cli` pass | Install output distinguishes file, directory, and recursive file symlinks. |
| Task 2.1 | done | Add `doctor --class skill-surface` scaffold | `cargo test -p agent-runtime-cli doctor` pass | Class can run without runtime manifests when the link map is absent. |
| Task 2.2 | done | Classify link-map entries | `cargo test -p agent-runtime-cli doctor::skill_surface` pass | JSON reports entry id, paths, link mode, discoverability, and warnings. |
| Task 2.3 | done | Warn on Codex `SKILL.md` file symlinks | `cargo test -p agent-runtime-cli doctor` pass | Warning code is `codex.active-skill.file-symlink`. |
| Task 3.1 | done | Pin audit-drift parent-scan behavior | `cargo test -p agent-runtime-cli audit_drift` pass | Domain-nested directory skill symlink is not treated as extra surface. |
| Task 3.2 | done | Document live-acceptance boundary | `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` pass | `DEVELOPMENT.md` now names shape-only versus live Codex acceptance. |
| Task 3.3 | done | Surface acceptance boundary in doctor output | `cargo test -p agent-runtime-cli doctor` pass | Human and JSON output include the boundary for Codex skill-surface checks. |

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `agent-docs resolve startup` | pass | Strict startup preflight passed. | terminal log |
| `agent-docs resolve project-dev` | pass | Strict project-dev preflight passed. | terminal log |
| `agent-docs resolve task-tools` | pass | Strict task-tools preflight passed before GitHub operations. | terminal log |
| `plan-tooling validate --file ...codex-skill-surface-primitives-plan.md` | pass | Plan bundle validation passed after formatting. | terminal log |
| `cargo test -p agent-runtime-cli doctor::skill_surface` | pass | Skill-surface classifier unit tests passed. | terminal log |
| `cargo test -p agent-runtime-cli install` | pass | Install unit and filtered integration tests passed. | terminal log |
| `cargo test -p agent-runtime-cli doctor` | pass | Doctor unit and filtered integration tests passed. | terminal log |
| `cargo test -p agent-runtime-cli audit_drift` | pass | Audit-drift unit and filtered integration tests passed. | terminal log |
| `cargo clippy -p agent-runtime-cli --all-targets -- -D warnings` | pass | Agent-runtime CLI clippy passed. | terminal log |
| `cargo test -p agent-runtime-cli` | pass | Agent-runtime CLI full crate tests passed. | terminal log |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` | pass | Docs gates, workspace clippy, nextest, and doc tests passed. | terminal log |
| `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage` | pass | Required checks, nextest, doctests, and coverage passed; line coverage was 85.30%. | `target/coverage/lcov.info` |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` | pass | Docs placement, docs hygiene, markdownlint, plan bundle, CLI output contract, and fixture redaction checks passed after the final state update. | terminal log |
| `agent-runtime doctor --class skill-surface --product codex --format json` | pass | Smoke against `graysurf/agent-runtime-kit`: 65 items, 0 findings, 0 warnings. | `agent-runtime-kit-skill-surface.json` |

## Runtime Findings

- `fix-now`: the original plan markdown exceeded the repository markdown line
  length limit after edits; fixed by reflowing the plan without changing task
  semantics.
- `fix-now`: the full gate found stale third-party artifacts after the
  `Cargo.lock` metadata hash changed; regenerated `THIRD_PARTY_LICENSES.md`
  and `THIRD_PARTY_NOTICES.md`, then reran the full gate successfully.
- `note`: `doctor --class skill-surface` is intentionally a shape diagnostic.
  It reads the source-root link map and does not stat live `$CODEX_HOME`.

## Blockers

- none

## Session Log

### 2026-05-23 12:27 CST

- Created tracking issue [#446](https://github.com/sympoies/nils-cli/issues/446)
  with source, plan, and initial state snapshots.
- Hardened install planning and executor tests around non-recursive directory
  sources for `kind: symlinked-file`.
- Added link-mode metadata through install planning, executor reporting, and
  install command output.
- Added `doctor --class skill-surface` with JSON and human output for Codex
  active-skill shape checks.
- Added audit-drift regression coverage for domain-nested directory skill
  symlinks.
- Updated `DEVELOPMENT.md` with the live Codex Desktop acceptance boundary.
- Validated targeted tests and the `--local-fast` workspace gate.

### 2026-05-23 12:41 CST

- Ran the required non-doc delivery gate with coverage; the rerun passed after
  regenerating third-party artifacts.
- Captured a current `agent-runtime-kit` source-root skill-surface smoke under
  the project output directory. It reported 65 items and no warnings.
- Reran the docs-only gate after updating this execution-state file.
- Next: commit, open PR, run specialist review, merge, and close issue #446.
