# forge-cli GitLab Capabilities Implementation Handoff

- Status: decisions settled; ready for plan tracking.
- Date: 2026-06-08
- Source: operator request after a live GitLab deploy workflow hit a
  `forge-cli` GitLab delivery blocker.
- Intended next step: open an L2 plan-tracking issue from this bundle. This is
  a source artifact, not the implementation itself.

## Execution

- Recommended plan:
  `docs/plans/2026-06-08-forge-cli-gitlab-capabilities/forge-cli-gitlab-capabilities-plan.md`
- Recommended execution state:
  `docs/plans/2026-06-08-forge-cli-gitlab-capabilities/forge-cli-gitlab-capabilities-execution-state.md`
- Status: decisions settled; plan tracking is the next step.
- Next-task source: this document.

## Problem

`forge-cli` successfully created a GitLab merge request for a deploy change, but
the later GitLab delivery atoms were not robust enough for the installed GitLab
CLI. The live workflow observed:

- `forge-cli pr create` succeeded against GitLab.
- `forge-cli pr merge --dry-run` produced the intended `glab mr merge` plan.
- live `forge-cli pr checks` and `forge-cli pr merge` stopped with
  `glab_version_unsupported` because the installed `glab` minor was newer than
  the pinned parser range.
- The merge was completed through a manual GitLab API fallback after verifying
  the MR state, pipeline result, source branch, target branch, and mergeability.

The immediate incident was recovered, but the shared tool behavior is still too
fragile for agent-owned GitLab delivery. GitLab support should not depend on a
single minor version of text output when GitLab exposes structured API data that
can support the same lifecycle.

## Decisions

- Treat this as an L2 plan because it affects shared `forge-cli` delivery
  behavior, needs a frozen plan, and should be resumable across sessions.
- Keep scope in `sympoies/nils-cli`, focused on the `forge-cli` GitLab provider
  and its direct tests/docs/release path.
- Prefer structured GitLab API reads or mutations for capabilities that are
  currently blocked by `glab` text parsing, especially checks, wait-checks, and
  merge.
- Keep `glab` as a supported backend helper where it provides stable JSON or
  existing behavior is already reliable.
- Preserve provider-neutral envelopes, dry-run planning, exit-code semantics,
  local-path redaction, and idempotent merge verification.
- Avoid GitLab-only special cases leaking into GitHub behavior.

## Scope

In scope:

- Audit the current `forge-cli` GitLab capability matrix across PR/MR, issue,
  label, inbox, repo, and auth surfaces.
- Identify which GitLab operations depend on fragile `glab` text parsing,
  unsupported JSON shapes, or late backend-version failures.
- Improve GitLab MR checks and wait-checks so live delivery can use structured
  GitLab API status data without failing solely because `glab` minor output
  changed.
- Improve GitLab MR merge so a mergeable, green MR can be merged and verified
  through a stable path, with branch cleanup behavior preserved where GitLab
  supports it.
- Add targeted unit/integration tests around GitLab API fallback behavior,
  version guard behavior, provider parity, JSON envelopes, and dry-run output.
- Update `forge-cli` documentation/specs and dependency guidance to explain the
  GitLab backend contract.
- Deliver through the normal nils-cli PR path and release/sync follow-up if a
  released binary is needed for runtime use.

Out of scope:

- Replacing every `glab` call in one pass when the existing JSON-backed path is
  already stable.
- Changing GitHub behavior except for shared abstractions needed to preserve
  parity.
- Taking ownership of GitLab server configuration, project permissions, or CI
  job definitions.
- Storing GitLab tokens or credentials in repo files.
- Expanding `forge-cli search` GitLab support unless the capability audit shows
  it is the smallest required follow-up for this plan.

## Requirements

1. `forge-cli` exposes a current GitLab capability matrix that distinguishes
   supported, intentionally unsupported, and fragile operations.
2. GitLab `pr checks` and `pr wait-checks` do not fail only because installed
   `glab` is a newer minor when structured GitLab API data can satisfy the
   check snapshot.
3. GitLab `pr merge` can complete a mergeable, green MR through a stable
   backend path and then re-fetch merged state plus merge SHA.
4. `pr deliver` inherits the improved GitLab checks/wait/merge behavior without
   needing a separate GitLab-specific macro.
5. Dry-run output remains explicit about planned backend calls and never hides
   destructive operations.
6. JSON envelopes keep provider-neutral schema versions and stable
   `error.kind` values for unsupported, unauthenticated, blocked, and timeout
   cases.
7. Local-path and credential redaction gates continue to protect provider
   payloads and logs.
8. GitLab self-hosted repositories continue to resolve the correct host and
   project path for API calls.

## Acceptance Criteria

1. A GitLab capability matrix is documented in the `forge-cli` spec or crate
   docs, including each supported PR/MR and issue operation.
2. Tests cover the regression where `glab 1.100.x` is installed but checks and
   merge can proceed through structured GitLab API data.
3. Tests cover the retained failure path when neither supported API data nor a
   supported `glab` parser path can satisfy the operation.
4. `forge-cli pr checks`, `pr wait-checks`, `pr merge`, and `pr deliver` keep
   the same success envelope shape for GitHub and GitLab where the operation is
   provider-neutral.
5. `pr merge` still refuses draft MRs, unsupported merge methods, non-green
   required checks, and unsafe branch cleanup conflicts.
6. Documentation and dependency guidance explain when `glab` is still required,
   when GitLab API fallback is used, and how version diagnostics should read.
7. `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` passes before
   delivery.

## Validation Plan

- `plan-tooling validate --file docs/plans/2026-06-08-forge-cli-gitlab-capabilities/forge-cli-gitlab-capabilities-plan.md --format text --explain`
- `bash scripts/ci/plan-bundle-validate.sh --strict --file docs/plans/2026-06-08-forge-cli-gitlab-capabilities/forge-cli-gitlab-capabilities-plan.md`
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
- Targeted implementation validation expected during execution:
  - `cargo fmt -p nils-forge-cli -- --check`
  - `cargo test -p nils-forge-cli pr_checks_gitlab`
  - `cargo test -p nils-forge-cli pr_merge`
  - `cargo test -p nils-forge-cli pr_wait_checks`
  - `cargo test -p nils-forge-cli conformance`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
- Optional live smoke, only when a suitable GitLab sandbox MR is available:
  - `forge-cli --provider gitlab pr checks <mr>`
  - `forge-cli --provider gitlab pr merge --dry-run <mr>`
  - non-destructive `pr deliver --no-merge` path against a sandbox MR.

## Risks And Guardrails

- **Risk**: an API fallback bypasses an intended `pr merge` safety gate.
  **Guardrail**: reuse the existing required-check, draft, merge-method, base
  branch, and post-merge verification gates before any destructive merge.
- **Risk**: GitLab API calls target the wrong host or project on self-hosted
  GitLab.
  **Guardrail**: derive host and encoded project path from repo context or MR
  web URL, and cover nested group paths in tests.
- **Risk**: provider-visible records expose local paths or credentials.
  **Guardrail**: keep source/plan examples repo-relative, retain local-path
  guards, and redact credential-bearing URLs.
- **Risk**: this becomes an unbounded GitLab rewrite.
  **Guardrail**: finish checks/wait/merge reliability first, then document any
  remaining capability gaps as explicit follow-up candidates.

## Read-First References

- `AGENTS.md`
- `DEVELOPMENT.md`
- `BINARY_DEPENDENCIES.md`
- `crates/forge-cli/README.md`
- `crates/forge-cli/docs/specs/forge-cli-spec-v1.md`
- `crates/forge-cli/src/ops/pr_checks_gitlab.rs`
- `crates/forge-cli/src/ops/pr_checks.rs`
- `crates/forge-cli/src/ops/pr_wait_checks.rs`
- `crates/forge-cli/src/ops/pr_merge.rs`
- `crates/forge-cli/src/macros/pr_deliver.rs`
- `crates/forge-cli/tests/integration/pr_checks_gitlab.rs`
- `crates/forge-cli/tests/integration/pr_merge.rs`
- `crates/forge-cli/tests/integration/pr_wait_checks.rs`
- `crates/forge-cli/tests/integration/conformance.rs`
