# Test Cleanup Governance

## Purpose

This runbook defines the stale test lifecycle for workspace cleanup and ongoing maintenance.
Use it to decide whether a candidate should be `remove`, `keep`, `rewrite`, or `defer`, and to keep CI guardrails deterministic.

## Stale Test Lifecycle

1. Discover candidates with `bash scripts/dev/workspace-test-stale-audit.sh`.
2. Classify each candidate using one decision mode:
   - `remove`: deterministic stale artifact with replacement coverage already present.
   - `keep`: still protects active behavior, parity, JSON schema, warning text, color handling, or exit semantics.
   - `rewrite`: behavior is still needed but the test/helper/fixture implementation is obsolete.
   - `defer`: evidence is ambiguous (for example macro indirection or reflection risk) and requires manual review.
3. Validate contract safety before merge.
4. Update CI baseline only after reviewed cleanup PRs merge.

## Deterministic Cleanup Map Inputs

Treat these artifacts as the authoritative cleanup map for Sprint 3:

- `$AGENT_HOME/out/workspace-test-cleanup/stale-tests.tsv`
- `$AGENT_HOME/out/workspace-test-cleanup/decision-rubric.md`
- `$AGENT_HOME/out/workspace-test-cleanup/crate-tiers.tsv`
- `$AGENT_HOME/out/workspace-test-cleanup/execution-manifest.md`
- `docs/specs/workspace-test-cleanup-lane-matrix-v1.md`

The spec freezes the `serial` vs `parallel` lane contract and the decision-mode mapping (`remove`, `rewrite`, `keep`, `defer`) so task
lanes do not drift as cleanup work lands.

## Frozen Serial Sequence

The serialized crate order is fixed:

1. `git-cli` (`serial-1`)
2. `agent-docs` (`serial-2`)
3. `macos-agent` (`serial-3`)
4. `fzf-cli` (`serial-4`)
5. `memo-cli` (`serial-5`)

All other crates remain `parallel` unless the matrix spec is intentionally revised.

## Evidence Rules

Before marking a candidate `remove`, include all of the following:

- Candidate path and symbol evidence from stale-test inventory output.
- Confirmation that `contract-allowlist.tsv` does not protect the candidate path.
- Replacement test evidence when user-visible behavior could change.
- Explicit validation command outputs in the PR (`test-stale-audit`, focused or
  local-fast checks, and CI coverage gate).

For `rewrite`, document:

- Why the old test/helper is obsolete.
- Which test now guards the behavior.
- Which command(s) prove parity/contract behavior still pass.

## Baseline Update Policy

- `scripts/ci/test-stale-audit-baseline.tsv` is a constrained allowlist for known `helper_fanout` + `remove` rows only.
- New regression rows must be fixed in code; do not add them to baseline as a shortcut.
- During cleanup lanes, baseline changes are limited to deleting rows for helpers that were actually removed with replacement coverage.
- Any baseline expansion requires an explicit policy update to both this runbook and
  `docs/specs/workspace-test-cleanup-lane-matrix-v1.md`, plus review evidence in the PR.

## CI Guardrails

- `bash scripts/ci/test-stale-audit.sh --strict`
  - Fails on new orphaned helper regressions relative to `scripts/ci/test-stale-audit-baseline.tsv`.
  - Fails when baseline contains entries outside the frozen S3T1 allowlist.
  - Fails on deprecated-path leftovers (`deprecated_path_marker`) in the current inventory.
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
  - Default local validation before delivery.
- GitHub required checks (non-doc changes):
  - `test`
  - `test_macos`
  - `coverage`
- Coverage gate command used by CI:
  - `cargo llvm-cov nextest --profile ci --workspace --lcov --output-path target/coverage/lcov.info --fail-under-lines 85`
  - `scripts/ci/coverage-summary.sh target/coverage/lcov.info`

## Reviewer Checklist

- Decision mode selected: `remove`, `keep`, `rewrite`, or `defer`.
- Evidence links include crate/file/symbol and replacement assertions when required.
- `bash scripts/ci/test-stale-audit.sh --strict` output is clean.
- Local validation passes.
- GitHub required checks are green, including coverage for non-doc changes.
- Baseline updates are justified and limited to reviewed stale-helper removals.
