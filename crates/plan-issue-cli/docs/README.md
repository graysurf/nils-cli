# plan-issue-cli docs

## Purpose

Crate-local documentation for `nils-plan-issue-cli`.

The default lifecycle for new issue-backed plan records is the
**v3 issue-backed plan record lifecycle** owned by the
`plan-issue record ...` surface. That surface opens provider issues,
posts canonical lifecycle comments, audits and repairs dashboards, and
performs strict provider-verified closeout. See
[issue-backed plan record contract v2](specs/issue-backed-plan-record-contract-v2.md)
and [plan-issue state machine v2](specs/plan-issue-state-machine-v2.md)
for the normative spec.

The prior `start-plan` / `start-sprint` Task Decomposition runtime is
retained for existing dispatch flows that have not migrated yet. Its
state machine, gate invariants, and runtime-truth row model live in the
v1 specs and the
[plan-issue CLI contract v2](specs/plan-issue-cli-contract-v2.md). New
work should not target the Task Decomposition runtime.

Current runtime ownership:

- `plan-tooling` continues to own plan parsing, validation, split modeling,
  and PR grouping primitives.
- `plan-issue record` owns the v3 issue-backed plan record lifecycle:
  open, post, audit, repair-dashboard, and close.
- Prior `plan-issue start-plan` / `start-sprint` commands continue to
  materialize Task Decomposition runtime metadata for in-flight dispatch
  flows.
- `forge-cli` and provider adapters own general provider operations that
  are not part of the issue-backed plan record lifecycle.
- markdown table cell canonicalization is provided by `nils-common::markdown`.

## Specs

- [issue-backed plan record contract v2 (current)](specs/issue-backed-plan-record-contract-v2.md)
- [plan-issue state machine v2 (current)](specs/plan-issue-state-machine-v2.md)
- [plan-issue CLI contract v2 (Task Decomposition runtime metadata)](specs/plan-issue-cli-contract-v2.md)
- [plan-issue state machine v1 (Task Decomposition runtime)](specs/plan-issue-state-machine-v1.md)
- [plan-issue gate matrix v1](specs/plan-issue-gate-matrix-v1.md)
