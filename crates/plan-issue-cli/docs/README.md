# plan-issue-cli docs

## Purpose

Crate-local documentation for `nils-plan-issue-cli`.

`Task Decomposition` is the crate's documented runtime-truth execution table for
existing plan/sprint orchestration. Specs define `Owner` as a dispatch alias,
document single-lane normalization to `per-sprint`, and treat
task-spec/subagent prompts as derived artifacts (not a second issue-body
dispatch table). `start-sprint` validates drift against plan-derived lanes and
does not rewrite issue rows in runtime-truth mode.

Issue-backed tracking and dispatch records that keep provider issue bodies as
mutable dashboards use the newer `plan-issue record ...` surface. That surface
renders/audits dashboards, append-only comments, dispatch ledgers, and closeout
readiness locally; provider issue mutation remains outside this crate path.

Current runtime ownership:

- `plan-tooling split-prs` emits grouping primitives only.
- `plan-issue-cli` materializes runtime `Owner/Branch/Worktree/Notes` metadata from plan content, grouping results, and prefixes.
- `plan-issue record build-dispatch-ledger` consumes the same grouping
  primitives without moving provider UI rendering into `plan-tooling`.
- markdown table cell canonicalization is provided by `nils-common::markdown`.

## Specs

- [plan-issue CLI contract v2](specs/plan-issue-cli-contract-v2.md)
- [issue-backed plan record contract v1](specs/issue-backed-plan-record-contract-v1.md)
- [plan-issue state machine and gates v1](specs/plan-issue-state-machine-v1.md)
- [plan-issue gate matrix v1](specs/plan-issue-gate-matrix-v1.md)
