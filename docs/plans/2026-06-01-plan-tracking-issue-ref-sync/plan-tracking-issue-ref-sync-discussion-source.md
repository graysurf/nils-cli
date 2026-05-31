# Plan Tracking Issue Ref Sync - Implementation Handoff

- Status: decisions settled; ready for plan generation.
- Date: 2026-06-01
- Source: a workflow discussion after `plan-archive discover` reported the
  completed `docs/plans/2026-05-31-forge-cli-search/` bundle as blocked with
  `no-provider-refs`. Manual provider lookup showed the matching tracker was
  `https://github.com/sympoies/nils-cli/issues/716`, but the plan folder's
  top-level Markdown never recorded that URL, so offline discovery could not
  infer the provider ref.
- Intended next step: open an L2 plan-tracking issue from this bundle. This is a
  source artifact, not an implementation plan.

## Execution

- Recommended plan: docs/plans/2026-06-01-plan-tracking-issue-ref-sync/plan-tracking-issue-ref-sync-plan.md
- Recommended execution state: docs/plans/2026-06-01-plan-tracking-issue-ref-sync/plan-tracking-issue-ref-sync-execution-state.md
- Status: decisions settled; plan generation is the next step.
- Next-task source: this document

## Problem

The plan-tracking workflow currently has two durable surfaces that can drift:

- `run-state.json`, which is the runtime source of truth for
  `execute-plan-tracking-issue` and live checkpoints.
- The canonical `*-execution-state.md`, which is the durable plan-bundle state
  later consumed by humans and by `plan-archive discover`.

The workflow already lets `tracking checkpoint` inherit the issue number from
`run-state.json`, and it intentionally re-renders visible state from the
payload instead of trusting a possibly stale execution-state header. That is the
right runtime model. The missing invariant is that once a tracking issue exists,
the canonical execution-state file should also carry the issue URL and stay
consistent with the run state.

## Failure Case To Preserve

- A read-only `plan-archive discover --source-repo /Users/terry/Project/sympoies/nils-cli --format json`
  scan reported `docs/plans/2026-05-31-forge-cli-search/` as:
  - `status`: `blocked`
  - `reason`: `no-provider-refs`
  - `refs`: empty
  - `archive_target.exists`: `false`
  - `dirty`: `false`
- The folder's plan title is `forge-cli Issue / PR Search`.
- `gh issue view 716 --repo sympoies/nils-cli` returned the same title,
  `forge-cli Issue / PR Search`, with URL
  `https://github.com/sympoies/nils-cli/issues/716` and state `CLOSED`.
- A targeted search of the plan folder found no `716`, `#716`, or `issues/716`
  text in its top-level Markdown.

This is not a `plan-archive discover` correctness bug. `discover` is
deliberately offline and infers provider refs only by parsing issue, PR, or MR
URLs from local Markdown. The improvement belongs upstream in plan-tracking
creation/resume flow: after a live tracking issue exists, the durable
execution-state Markdown must record that URL before the workflow is considered
ready to execute or archive.

## Confirmed Facts

- [F1] `plan-archive discover` classifies a folder with empty inferred refs as
  blocked with `no-provider-refs`.
- [F2] `plan-archive discover` reads only local files and never calls a
  provider, so title matching is not available and should not become the
  default behavior.
- [F3] `tracking checkpoint --live` can inherit the provider issue number from
  `run-state.json`; if neither `--issue` nor run-state issue is available, it
  reports `tracking-checkpoint-live-missing-issue`.
- [F4] `tracking checkpoint` derives visible state from run-state payload
  rather than echoing the execution-state header, specifically to avoid stale
  header bullets leaking into live comments.
- [F5] A healthy prior bundle records the tracking issue URL in
  `*-execution-state.md`, for example
  `docs/plans/2026-05-31-git-cli-worktree-surface/git-cli-worktree-surface-execution-state.md`.

## Decisions

- Implement the primary contract in `sympoies/nils-cli` first because the
  affected CLIs are `plan-issue`, `plan-tooling`, and `plan-archive`.
- Treat `run-state.json` plus provider lifecycle comments as the runtime source
  of truth; do not make execution-state header text the runtime authority.
- Treat the execution-state tracking issue URL as a durable postcondition once
  a live tracking issue exists.
- Keep `plan-archive discover` offline and deterministic. Do not add default
  provider lookup or title matching.
- Update `graysurf/agent-runtime-kit` skill instructions only after the
  nils-cli contract and command behavior are settled.

## Open Questions Carried Into Execution

1. Which command should own the local execution-state patch after issue open:
   `record open`, `tracking run init`, or a new `plan-tooling` helper that the
   create skill calls?
2. Should `execute-plan-tracking-issue` block when execution-state is missing
   the URL, or emit a strict warning first? The preferred answer is a hard block
   for missing or mismatched refs after run-state carries an issue.
3. Should legacy bundles get a repair command or only a documented manual edit
   path? The preferred answer is a small repair command so archived bundles do
   not depend on ad hoc edits.

## Acceptance Criteria

1. A live plan-tracking issue creation path records the provider issue number in
   run-state and records the issue URL in the canonical execution-state
   Markdown.
2. Execute/checkpoint preflight fails clearly when run-state issue and
   execution-state tracking issue URL are missing, stale, or mismatched.
3. The `forge-cli-search` failure mode is covered by a regression fixture or
   documented repair test.
4. `plan-archive discover` can infer provider refs from bundles that completed
   through the repaired plan-tracking workflow.
5. Agent-runtime-kit skill docs are updated only if nils-cli changes alter the
   required create/execute workflow.
