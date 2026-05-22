# forge-cli GitHub Checks Compatibility Implementation Handoff

- Status: open, ready for plan tracking
- Date: 2026-05-22
- Source: `graysurf/agent-runtime-kit` issue #26 Sprint 5 validation, local
  `forge-cli` and `gh` command output, and the current `forge-cli` GitHub checks
  implementation.
- Intended next step: execute the paired plan and release `nils-cli` `0.17.0`.

## Purpose

Fix the `forge-cli` GitHub checks backend so released `forge-cli` works with
`gh 2.92.0` during PR close and delivery workflows, then cut `nils-cli`
`0.17.0` as the downstream compatibility boundary for `agent-runtime-kit` Plan
05 Sprint 6.

## Confirmed Facts

- Released `forge-cli 0.16.0` fails live GitHub check operations with
  `gh 2.92.0` because it requests unsupported `gh pr checks --json` fields.
- The reproduced failure is:
  `Unknown JSON field: "conclusion"`.
- `gh 2.92.0` reports the supported `gh pr checks --json` fields as:
  `bucket`, `completedAt`, `description`, `event`, `link`, `name`,
  `startedAt`, `state`, and `workflow`.
- A direct `gh pr checks 37 --repo graysurf/agent-runtime-kit --json ...`
  using the supported fields returned a successful check snapshot.
- A direct `gh pr checks 37 --repo graysurf/agent-runtime-kit --required ...`
  returned `no required checks reported on the 'feat/plan-05-pr-domain' branch`
  for the already-merged PR branch.
- The current `forge-cli` implementation requests:
  `name,state,conclusion,bucket,workflow,link,startedAt,completedAt,description,isRequired`.
- `forge-cli pr merge` and `forge-cli pr deliver` depend on the same checks
  backend through required-check gating, so the compatibility fix must cover the
  shared backend, not only the standalone `pr checks` command.
- `agent-runtime-kit` recorded this as extraction backlog item `P5-S5-G1` and
  used provider-native `gh pr checks` / `gh pr merge` as a temporary operator
  fallback.

## Decisions

- Fix this in `nils-cli` / `forge-cli`; do not add raw `gh` lifecycle logic to
  `agent-runtime-kit` skill bodies.
- Release the fix as `nils-cli` `0.17.0`.
- Preserve `forge-cli` as the provider lifecycle primitive used by downstream
  runtime skill bodies.
- Keep provider-native `gh` commands as an explicitly recorded temporary
  fallback only until the fixed release is available.

## Scope

- Update the GitHub `pr checks` backend to work with the `gh 2.92.0` field set.
- Preserve the existing normalized `PrChecksPayload` contract where possible.
- Keep `--required-only` behavior deterministic for `pr checks`,
  `pr wait-checks`, `pr merge`, and `pr deliver`.
- Add regression tests that reproduce the `gh 2.92.0` field set and the
  provider error around unsupported fields.
- Cut the `nils-cli` `0.17.0` release after the compatibility fix passes the
  workspace release gates.
- Update the downstream `agent-runtime-kit` issue with the nils-cli tracking
  issue link and later release handoff.

## Non-Scope

- Do not rewrite `forge-cli` to call GitHub REST or GraphQL directly.
- Do not add Gitea, Forgejo, release, label, or branch protection support.
- Do not change GitLab checks behavior except where shared tests or docs need
  harmless clarification.
- Do not complete the `agent-runtime-kit` Plan 05 Sprint 6 migration in this
  plan.

## Implementation Boundaries

- `nils-cli`: owns `forge-cli` backend behavior, tests, docs, version bump, and
  release.
- `agent-runtime-kit`: owns downstream skill floors and extraction backlog
  closeout after `nils-cli 0.17.0` is released.
- GitHub CLI: remains the subprocess backend; the wrapper must adapt to its
  current supported JSON field set instead of assuming older fields.

## Requirements

- `forge-cli pr checks` must not request unsupported `gh 2.92.0` JSON fields.
- `forge-cli pr wait-checks` must successfully gate passing checks through the
  same normalized payload.
- `forge-cli pr merge` must not fail before merge solely because the checks
  backend requests unsupported fields.
- `forge-cli pr deliver` must use the fixed checks path.
- Required-check handling must not silently treat a real failed required check as
  success.
- No network access should be required for the default automated test suite;
  live behavior is characterized through recorded/stubbed `gh` fixtures.

## Acceptance Criteria

- A regression test fails on the old `conclusion` / `isRequired` field request
  and passes with the fixed field request.
- GitHub checks fixtures model the `gh 2.92.0` field set.
- `cargo test -p nils-forge-cli pr_checks` passes.
- `cargo test -p nils-forge-cli pr_wait_checks required_check_gate pr_deliver`
  passes or the equivalent targeted test set is recorded.
- `bash scripts/ci/nils-cli-checks-entrypoint.sh` passes locally before release.
- `nils-cli 0.17.0` is tagged and published through the standard release
  workflow.
- A post-release `forge-cli --version` check reports `0.17.0`.

## Validation Plan

- Targeted forge-cli tests for GitHub checks parsing and backend argv.
- Integration tests for `pr wait-checks`, required-check gate, `pr merge`, and
  `pr deliver` paths.
- Workspace CI entrypoint before release.
- Release verification after `0.17.0` is available on the local PATH.

## Risks And Guardrails

- `gh pr checks --required` can return no rows or a non-zero status when no
  required checks are configured; the implementation must classify that case
  intentionally instead of surfacing a generic backend failure.
- Removing `isRequired` from the default field set means gating semantics need a
  deliberate source of truth, such as a required-only backend call or equivalent
  fixture-backed classification.
- `state` values from `gh` are uppercase in current output; normalization must
  be case-insensitive or explicitly mapped.
- Release work should not update downstream `agent-runtime-kit` skill floors
  until `forge-cli --version` confirms the released binary boundary.

## Execution

- Recommended plan: docs/plans/forge-cli-gh-292-checks/forge-cli-gh-292-checks-plan.md
- Recommended execution state: docs/plans/forge-cli-gh-292-checks/forge-cli-gh-292-checks-execution-state.md

## Retention Intent

This source document is execution coordination for the `0.17.0` compatibility
release. It can be cleaned up after the tracking issue is closed and downstream
`agent-runtime-kit` has recorded the fixed release handoff.

## Open Questions

- Whether required-only gating should use one `gh pr checks --required` call plus
  one all-checks call, or a single call with documented loss of optional-check
  reporting. Default: preserve full reporting and use a separate required-only
  call for gating.
- Whether `0.17.0` should include any unrelated queued fixes. Default: no; keep
  this release focused on the GitHub checks compatibility fix.
