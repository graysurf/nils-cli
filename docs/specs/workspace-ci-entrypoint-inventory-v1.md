# Workspace CI Entrypoint Inventory v1

## Purpose

This inventory records the canonical owner for each active CI/release workflow
check, the default local validation path, and keep/delete criteria for helper
scripts.
It is the source of truth for Sprint 1 CI entrypoint consolidation.

## Keep/Delete Criteria

A helper path is `keep` only when at least one active caller is discoverable by repository search in one of:

- GitHub workflow YAML (`.github/workflows/*.yml`)
- Canonical contributor docs (`README.md`, `DEVELOPMENT.md`, `docs/runbooks/**`, crate READMEs)
- Canonical runtime entrypoints (`scripts/**`, `.agents/**`, `wrappers/**`) used by active workflows/docs

A helper path is `delete-candidate` when no active caller exists outside historical/transient
planning artifacts. Removal of a `delete-candidate` script is a separate code-change task and is
out of scope for this governance inventory; flag it as a follow-up.

## Canonical Workflow Owners

### `.github/workflows/ci.yml`

| Job | Step | Canonical owner | Decision | Notes |
| --- | --- | --- | --- | --- |
| `changes` | `Detect docs-only change set` | `scripts/ci/detect-docs-only.sh` (shares `scripts/ci/lib/doc_classify.py`) | keep | Emits `docs_only`; downstream jobs read `needs.changes.outputs.docs_only`. |
| `test`, `test_macos` | `Checkout`, `Set up Rust`, `Cache cargo`, `Set up Node.js`, tool bootstrap | Upstream GitHub Actions + runner bootstrap shell | keep | Platform bootstrap stays in workflow; runs in both lanes (docs-only still needs node/rg/plan-tooling). |
| `test`, `test_macos` | `Nils CLI checks (includes third-party-artifacts-audit, Completion asset audit, docs-hygiene-audit, test-stale-audit)` | `scripts/ci/nils-cli-checks-entrypoint.sh` -> `./.agents/skills/project-verify-required-checks/scripts/project-verify-required-checks.sh` | keep | Full CI verification contract after setup; passes `--docs-only` when `needs.changes.outputs.docs_only == 'true'`. |
| `test`, `test_macos` | `Third-party artifact audit` (removed) | replaced by required-checks script ordering | delete | Duplicate workflow fragment removed. |
| `test`, `test_macos` | `Completion asset audit` (removed) | replaced by required-checks script ordering | delete | Duplicate workflow fragment removed. |
| `test`, `test_macos` | `Publish JUnit report`, `Upload JUnit XML` | upstream Actions artifacts/reporting | keep | Post-check reporting only. |
| `coverage` | coverage generation/report/upload/cleanup steps | `cargo llvm-cov` + `scripts/ci/coverage-summary.sh` + upload/comment actions | keep | Coverage artifacts are created and cleaned only in this job. Each step carries a `docs_only != 'true'` guard (step-level, never job-level) so the check still concludes `success` on docs-only commits and the `release.yml` gate stays satisfied. |
| `coverage_badge` | badge generation/publish | `scripts/ci/coverage-badge.sh` + git push flow | keep | Push-only automation path; skipped on docs-only pushes (no LCOV artifact is produced). |

### `.github/workflows/release.yml`

| Job | Step | Canonical owner | Decision | Notes |
| --- | --- | --- | --- | --- |
| `build` | ripgrep bootstrap | inline OS bootstrap shell | keep | Platform package manager differences remain workflow-local. |
| `build` | `Regenerate third-party artifacts` | `scripts/generate-third-party-artifacts.sh` | keep | Canonical artifact generation gate. |
| `build` | `Package` | `scripts/workspace-bins.sh` + inline packaging shell | keep | Packaging owns workspace binary discovery through script entrypoint. |
| `build` | `Audit release tarball third-party artifacts` | `scripts/ci/release-tarball-third-party-audit.sh` | keep | Canonical release artifact audit. |
| `release` | publish GitHub release | `softprops/action-gh-release@v2` | keep | Standard release publication action. |

### `.github/workflows/publish-crates.yml`

| Job | Step | Canonical owner | Decision | Notes |
| --- | --- | --- | --- | --- |
| `publish` | `Publish selected crates` | `scripts/publish-crates.sh` | keep | Canonical crates publish/dry-run entrypoint. |

## Required-Checks Script Ownership

`./.agents/skills/project-verify-required-checks/scripts/project-verify-required-checks.sh` is canonical for full CI verification ordering:

1. docs/stale/third-party/completion audits (`scripts/ci/*`)
2. completion registration/parity checks
3. compile/test gates (`cargo fmt`, `cargo clippy`, workspace tests)

No workflow may duplicate these audit commands as independent pre-steps unless
the required-checks entrypoint is updated first. Day-to-day local development
uses `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`, which
escalates to the workspace Rust gate when changed files can affect reverse
dependencies.

## Helper Surface Decisions (Sprint 1 Scope)

This section enumerates every script under `scripts/ci/*.sh` (matches `ls scripts/ci/*.sh`) and
records the keep/delete decision plus the active caller evidence.

| Path | Decision | Active caller evidence |
| --- | --- | --- |
| `scripts/ci/agent-docs-snapshots.sh` | keep | `crates/agent-docs/README.md` snapshot workflow (`scripts/ci/agent-docs-snapshots.sh [--bless]`) |
| `scripts/ci/completion-asset-audit.sh` | keep | `DEVELOPMENT.md` full checks list + `project-verify-required-checks.sh` step |
| `scripts/ci/completion-flag-parity-audit.sh` | keep | `DEVELOPMENT.md` full checks list + `project-verify-required-checks.sh` step |
| `scripts/ci/coverage-badge.sh` | keep | `.github/workflows/ci.yml` `coverage_badge` job |
| `scripts/ci/coverage-summary.sh` | keep | `.github/workflows/ci.yml` `coverage` job + `DEVELOPMENT.md` coverage flow |
| `scripts/ci/detect-docs-only.sh` | keep | `.github/workflows/ci.yml` `changes` job + `scripts/ci/tests/detect-docs-only.test.sh` |
| `scripts/ci/docs-hygiene-audit.sh` | keep | `DEVELOPMENT.md` full checks list + `project-verify-required-checks.sh` docs-only and full passes |
| `scripts/ci/docs-placement-audit.sh` | keep | `DEVELOPMENT.md` full checks list + `project-verify-required-checks.sh` docs-only and full passes |
| `scripts/ci/markdownlint-audit.sh` | keep | `DEVELOPMENT.md` full checks list + `project-verify-required-checks.sh` docs-only and full passes |
| `scripts/ci/nils-cli-checks-entrypoint.sh` | keep | `.github/workflows/ci.yml` `test` and `test_macos` jobs + `DEVELOPMENT.md` local-fast and CI/full commands |
| `scripts/ci/release-tarball-third-party-audit.sh` | keep | `.github/workflows/release.yml` `build` job |
| `scripts/ci/test-stale-audit.sh` | keep | `DEVELOPMENT.md` full checks list + `project-verify-required-checks.sh` step |
| `scripts/ci/third-party-artifacts-audit.sh` | keep | `DEVELOPMENT.md` full checks list + `project-verify-required-checks.sh` step + dependabot bump skill |

## Auxiliary Wrapper / Tooling Decisions

| Path | Decision | Active caller evidence |
| --- | --- | --- |
| `scripts/ci/lib/doc_classify.py` | keep | imported by `scripts/ci/nils-cli-local-fast.sh` planner + `scripts/ci/detect-docs-only.sh` |
| `wrappers/plan-tooling` | keep | `README.md` wrapper contributor flow + workspace wrapper directory contract |
| `wrappers/git-cli` | keep | `README.md` wrapper contributor flow + `git-cli` wrapper behavior |

## Validation Commands

```bash
test -f docs/specs/workspace-ci-entrypoint-inventory-v1.md
ls scripts/ci/*.sh
rg -n 'scripts/ci/|project-verify-required-checks' \
  .github/workflows/ci.yml .github/workflows/release.yml .github/workflows/publish-crates.yml \
  DEVELOPMENT.md .agents/skills/project-verify-required-checks/scripts/project-verify-required-checks.sh
rg -n 'canonical|delete|delete-candidate|keep|workflow' docs/specs/workspace-ci-entrypoint-inventory-v1.md
```
