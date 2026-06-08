# forge-cli GitLab Capabilities Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: planned; tracking issue pending.
- Target scope: `forge-cli` GitLab provider capability audit and MR delivery
  reliability improvements in `sympoies/nils-cli`.
- Execution window: Sprint 1 (capability audit and backend contract) -> Sprint
  2 (GitLab checks/wait/merge hardening) -> Sprint 3 (docs, PR delivery, and
  release/runtime follow-up if needed), serial.
- Current task: open the tracking issue from this bundle.
- Next task: Sprint 1 Task 1.1 - audit the GitLab provider surface.
- Last updated: 2026-06-08
- Branch/commit/PR: pending.
- Source document:
  `docs/plans/2026-06-08-forge-cli-gitlab-capabilities/forge-cli-gitlab-capabilities-discussion-source.md`
- Plan document:
  `docs/plans/2026-06-08-forge-cli-gitlab-capabilities/forge-cli-gitlab-capabilities-plan.md`
- Direct source-doc execution waiver: not applicable.
- Tracking issue: pending.
- Source snapshot: pending.
- Plan snapshot: pending.
- Initial state snapshot: pending.

## Validation Plan

- Bundle creation: validate the plan-source bundle before opening the tracker.
- Tracker creation: dry-run `plan-issue record open`, then live-create only if
  labels, title, issue body, lifecycle comments, and repo are correct.
- Initial read-back: audit the live issue with `record audit --profile tracking
  --expect-visible`.
- Sprint 1: targeted `nils-forge-cli` tests for CLI/parity and GitLab
  capability matrix coverage.
- Sprint 2: targeted checks/wait/merge/deliver tests for GitLab API fallback,
  safety gates, and envelope parity.
- Sprint 3: docs-only validation, local-fast validation, provider PR checks,
  and optional non-destructive live GitLab sandbox smoke.

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | pending | Audit the GitLab provider surface | pending | Map supported, unsupported, and fragile GitLab surfaces across `forge-cli`. |
| 1.2 | pending | Choose and test the GitLab structured backend contract | pending | Decide API abstraction shape and cover host/project/auth/redaction behavior. |
| 2.1 | pending | Harden GitLab checks and wait-checks | pending | Avoid unsupported `glab` minor blockers when API status data is available. |
| 2.2 | pending | Harden GitLab merge and post-merge verification | pending | Preserve safety gates and stable merge SHA extraction. |
| 2.3 | pending | Normalize diagnostics and version preflight behavior | pending | Keep `glab_version_unsupported` only where the parser path is truly required. |
| 3.1 | pending | Update documentation and dependency guidance | pending | Document capability matrix, API fallback, and `glab` dependency boundaries. |
| 3.2 | pending | Validate and deliver the nils-cli PR | pending | Run local-fast, provider checks, and optional sandbox smoke. |
| 3.3 | pending | Release and runtime-surface follow-up | pending | Release/sync only if the operator needs the improved binary in runtime surfaces. |

## Session Log

- 2026-06-08: Operator approved L2 tracking for overall `forge-cli` GitLab
  capability improvement after a live GitLab deploy MR required manual API
  fallback when `forge-cli` checks/merge hit `glab_version_unsupported`.

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `plan-tooling validate --file docs/plans/2026-06-08-forge-cli-gitlab-capabilities/forge-cli-gitlab-capabilities-plan.md --format text --explain` | pass | Plan-source bundle validated with zero errors. | local |
| `bash scripts/ci/plan-bundle-validate.sh --strict --file docs/plans/2026-06-08-forge-cli-gitlab-capabilities/forge-cli-gitlab-capabilities-plan.md` | pass | Repository strict plan-bundle validation passed for the new bundle. | local |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` | pass | Docs placement, hygiene, markdown lint, plan-bundle validation, CLI output contract lint, and forge-cli fixture lint passed. | local |
| `plan-issue --repo sympoies/nils-cli --format json --dry-run record open --profile tracking --bundle docs/plans/2026-06-08-forge-cli-gitlab-capabilities ...` | pending | Preview issue body, labels, and lifecycle comments before live create. | local |
| `plan-issue --repo sympoies/nils-cli --format json record open --profile tracking --bundle docs/plans/2026-06-08-forge-cli-gitlab-capabilities ...` | pending | Open tracker issue and write provider URLs back to this state file. | provider |
| `plan-issue --format json record audit --profile tracking --expect-visible ...` | pending | Confirm source, plan, and state records are provider-visible and lint-clean. | local/provider |
