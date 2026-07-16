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
| `changes` | `Detect change set` | `scripts/ci/detect-docs-only.sh` plus exact-base `scripts/ci/detect-release-only.sh` policy | keep | Emits `base_sha`, `docs_only`, `head_sha`, `release_branch`, and `release_candidate`. Release policy is loaded from the exact base SHA; missing or invalid policy emits `release_candidate=false`. |
| `changes` | `Prove full CI on exact base main commit` | exact-base `.github/scripts/release-ci-gate.cjs` (`findTrustedMainCi`) | keep | A canonical candidate becomes eligible for the reduced lane only when the exact base SHA has a unique successful push CI run whose required jobs contain the full-validation marker. API errors and ambiguous evidence fail closed. |
| `changes` | `Finalize fail-closed CI lane` | workflow-local boolean conjunction | keep | Emits `release_only=true` only when both the semantic candidate and exact-base CI proof are true; all other states emit false. |
| `test`, `test_macos` | `Checkout`, `Set up Rust`, `Cache cargo` | Upstream GitHub Actions | keep | Shared setup for full, docs-only, and release-only lanes; checkout fetches full history so exact-base policy scripts can be extracted. |
| `test`, `test_macos` | `Set up Node.js` and non-release tool bootstrap | Upstream GitHub Actions + runner bootstrap shell | keep | Runs only when `release_only != 'true'`; docs-only validation still needs Node, ripgrep, and plan tooling. |
| `test`, `test_macos` | full/docs-only `Nils CLI checks` path | `scripts/ci/nils-cli-checks-entrypoint.sh` -> `./.agents/skills/project-verify-required-checks/scripts/project-verify-required-checks.sh` | keep | Full CI verification contract after setup; passes `--docs-only` when `docs_only == 'true'` and runs whenever `release_only != 'true'`. |
| `test`, `test_macos` | release-only `Nils CLI checks` path | exact-base `scripts/ci/release-only-checks.sh` (which re-runs exact-base `scripts/ci/detect-release-only.sh`) | keep | Fetches full history, loads both scripts from the exact base SHA, revalidates the canonical transform, then runs lockstep, third-party, publish-order, and locked workspace checks. Required job names remain unchanged. |
| `test`, `test_macos` | `Third-party artifact audit` (removed) | replaced by required-checks script ordering | delete | Duplicate workflow fragment removed. |
| `test`, `test_macos` | `Completion asset audit` (removed) | replaced by required-checks script ordering | delete | Duplicate workflow fragment removed. |
| `test`, `test_macos` | `Publish JUnit report`, `Upload JUnit XML` | upstream Actions artifacts/reporting | keep | Post-check reporting only. |
| `coverage` | coverage generation/report/upload/cleanup steps | `cargo llvm-cov` + `scripts/ci/coverage-summary.sh` + upload/comment actions | keep | Coverage artifacts are created and cleaned only in this job. Expensive steps carry both `docs_only != 'true'` and `release_only != 'true'` guards (step-level, never job-level), so skipped lanes still conclude `success` under the required `coverage` check name. |
| `coverage_badge` | badge generation/publish | `scripts/ci/coverage-badge.sh` + git push flow | keep | Push-only automation path; skipped on docs-only pushes (no LCOV artifact is produced). |

### `.github/workflows/release.yml`

| Job | Step | Canonical owner | Decision | Notes |
| --- | --- | --- | --- | --- |
| `verify_ci` | `Require green CI on tagged commit` | `.github/scripts/release-ci-gate.cjs` (`runReleaseGate`) | keep | Accepts only unique, same-repository, canonical merged-release-PR CI evidence for the exact tag SHA. Missing or ambiguous evidence falls back to polling the existing exact-SHA required checks (`test`, `test_macos`, `coverage`). |
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
| `scripts/ci/cargo-deny-audit.sh` | keep | `DEVELOPMENT.md` supply-chain audit section + `.github/workflows/ci.yml` `cargo-deny` job |
| `scripts/ci/cli-output-contract-lint.sh` | keep | `DEVELOPMENT.md` docs-only/full checks list + `project-verify-required-checks.sh` docs-only and full passes |
| `scripts/ci/completion-asset-audit.sh` | keep | `DEVELOPMENT.md` full checks list + `project-verify-required-checks.sh` step |
| `scripts/ci/completion-freshness-audit.sh` | keep | `DEVELOPMENT.md` full checks list + `project-verify-required-checks.sh` step |
| `scripts/ci/completion-flag-parity-audit.sh` | keep | `DEVELOPMENT.md` full checks list + `project-verify-required-checks.sh` step |
| `scripts/ci/coverage-badge.sh` | keep | `.github/workflows/ci.yml` `coverage_badge` job |
| `scripts/ci/coverage-summary.sh` | keep | `.github/workflows/ci.yml` `coverage` job + `DEVELOPMENT.md` coverage flow |
| `scripts/ci/crate-naming-audit.sh` | keep | `DEVELOPMENT.md` full checks list + `project-verify-required-checks.sh` step |
| `scripts/ci/detect-docs-only.sh` | keep | `.github/workflows/ci.yml` `changes` job + `scripts/ci/tests/detect-docs-only.test.sh` |
| `scripts/ci/detect-release-only.sh` | keep | exact-base policy loaded by `.github/workflows/ci.yml` + `scripts/ci/release-only-checks.sh` + `scripts/ci/tests/detect-release-only.test.sh` |
| `scripts/ci/docs-hygiene-audit.sh` | keep | `DEVELOPMENT.md` full checks list + `project-verify-required-checks.sh` docs-only and full passes + `scripts/ci/tests/docs-hygiene-audit.test.sh` |
| `scripts/ci/docs-placement-audit.sh` | keep | `DEVELOPMENT.md` full checks list + `project-verify-required-checks.sh` docs-only and full passes |
| `scripts/ci/forge-cli-fixture-lint.sh` | keep | `DEVELOPMENT.md` full checks list + `project-verify-required-checks.sh` step |
| `scripts/ci/markdownlint-audit.sh` | keep | `DEVELOPMENT.md` full checks list + `project-verify-required-checks.sh` docs-only and full passes |
| `scripts/ci/nils-cli-checks-entrypoint.sh` | keep | `.github/workflows/ci.yml` `test` and `test_macos` jobs + `DEVELOPMENT.md` local-fast and CI/full commands |
| `scripts/ci/nils-cli-local-fast.sh` | keep | `scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` delegates changed-scope planning/execution here |
| `scripts/ci/plan-bundle-validate.sh` | keep | `DEVELOPMENT.md` docs-only/full checks list + `project-verify-required-checks.sh` docs-only and full passes |
| `scripts/ci/publish-order-audit.sh` | keep | `DEVELOPMENT.md` full checks list + `project-verify-required-checks.sh` step + `scripts/ci/tests/publish-order-audit.test.sh` |
| `scripts/ci/release-tarball-third-party-audit.sh` | keep | `.github/workflows/release.yml` `build` job |
| `scripts/ci/release-only-checks.sh` | keep | exact-base reduced lane in `.github/workflows/ci.yml` `test` and `test_macos` jobs + `scripts/ci/tests/release-workflow-contract.test.sh` |
| `scripts/ci/skill-shell-suites.sh` | keep | `DEVELOPMENT.md` full checks list + `project-verify-required-checks.sh` step (runs every `.agents/skills/*/tests/test_*.sh` suite) |
| `scripts/ci/test-stale-audit.sh` | keep | `DEVELOPMENT.md` full checks list + `project-verify-required-checks.sh` step |
| `scripts/ci/third-party-artifacts-audit.sh` | keep | `DEVELOPMENT.md` full checks list + `project-verify-required-checks.sh` step + dependabot bump skill |
| `scripts/ci/verify-signed-commits.sh` | keep | `lefthook.yml` pre-push hook |
| `scripts/ci/workspace-version-lockstep.sh` | keep | `DEVELOPMENT.md` full checks list + `project-verify-required-checks.sh` step |

## Auxiliary Wrapper / Tooling Decisions

| Path | Decision | Active caller evidence |
| --- | --- | --- |
| `.github/scripts/release-ci-gate.cjs` | keep | `.github/workflows/ci.yml` exact-base main CI proof + `.github/workflows/release.yml` tag gate + `scripts/ci/tests/release-ci-gate.test.cjs` |
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
