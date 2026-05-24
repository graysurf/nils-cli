# Plan-Issue CLI Provider Abstraction Implementation Handoff

| Field | Value |
| --- | --- |
| Status | Ready for design + implementation (no code in this plan; design-first) |
| Date | 2026-05-25 |
| Source | Downstream sandbox validation in `terrylin/agent-runtime-testing` (gitlab.gamania.com); see F-3 in the sandbox source doc |
| Intended next step | Land an internal provider abstraction in `plan-issue-cli` so plan-tracking + dispatch lifecycle works on GitLab; iterate per Sprint plan |

## Purpose

`plan-issue-cli` (the CLI that drives the issue-backed plan-tracking and
dispatch lifecycles) is currently hard-wired to `gh` for every provider call:
issue create, comment add, label set, body edit, close. As a result **all**
of the following skills are non-functional on GitLab today:

- `dispatch/create-plan-tracking-issue`
- `dispatch/execute-plan-tracking-issue`
- `dispatch/deliver-plan-tracking-issue`
- `dispatch/plan-tracking-issue-closeout`
- `dispatch/deliver-dispatch-plan`
- `dispatch/execute-dispatch-lane`
- `dispatch/review-dispatch-lane-pr`
- `dispatch/dispatch-plan-closeout`
- `pr/create-dispatch-lane-pr` (separately, contract-locked to GitHub; subsumed once provider routing exists)

Concrete failure mode reproduced in sandbox:

```text
plan-issue --format json record open --profile tracking \
  --repo terrylin/agent-runtime-testing --bundle docs/plans/...
→ status: error
  code: record-open-issue-create-failed
  message: gh issue create --repo terrylin/agent-runtime-testing ... failed:
           GraphQL: Could not resolve to a Repository with the name 'terrylin/agent-runtime-testing'
```

The error message is the smoking gun: `plan-issue` invokes `gh` even when the
repo lives on GitLab.

This plan bundle is **design-first**. The scope below frames the work but does
not pre-decide implementation strategy; Sprint 1 explicitly opens that
decision.

## Confirmed facts

- `crates/plan-issue-cli/src/github.rs` contains every provider call: ~1001 lines, all gh-specific. [F1]
- Downstream `forge-cli` already exposes a working provider-neutral surface (`issue create/view/edit/comment/close/reopen`, `pr create/view/edit/...`, `label audit/ensure`). [F2]
- `plan-issue-cli` and `forge-cli` ship from the same workspace so a workspace-internal dep is mechanically fine. [F3]
- The skill SKILL.md files (`dispatch/create-plan-tracking-issue` etc.) already document Outputs in provider-neutral terms; no skill-side rewrite is needed if the CLI starts honouring provider. [F4]
- `forge-cli pr` and `forge-cli issue` follow `cli.forge-cli.*.v1` envelope contracts that `plan-issue-cli` can call as a subprocess (provider-neutral surface) or link to as a library if dependency boundaries allow. [F5]

## Decisions (carried forward unless Sprint 1 design changes them)

1. **Provider abstraction lives inside `plan-issue-cli`**, not in a separate crate. The CLI is the boundary; abstraction below it is internal.
2. **Route through `forge-cli`** for provider-neutral atoms (issue create / comment / edit / close, label ensure, body fetch) rather than re-implementing GitLab support in-tree. This avoids duplicating the gh/glab adapter layer.
3. **No breaking changes to the `plan-issue` CLI surface**: existing callers must continue to work unchanged. Provider detection is automatic from the `--repo` / git remote, mirroring forge-cli's behavior.
4. **Sprint 1 is design only**: produce a short design note + minimal contract sketch before any code change.
5. **Sprint 2 = open path**: `record open` must work for both `--profile tracking` and `--profile dispatch` on GitLab.
6. **Sprint 3 = continue + close**: `record post` (state/session/validation/closeout), `record audit`, `record close`, `link-pr`, plus the dispatch family (`start-plan`, `start-sprint`, etc.) round out the surface.

## Scope

- In scope:
  - Introduce a provider-routing layer inside `plan-issue-cli`.
  - Wire `record open` to support GitLab via `forge-cli`-backed atoms (or equivalent).
  - Land `record post`, `record audit`, `record close`, `link-pr`, and the dispatch flow on GitLab.
  - Add cross-provider tests (GitHub + GitLab) using stubbed provider backends.
  - Update SKILL.md files where current text implies "gh" only (most should already be neutral, but verify).
- Out of scope for this plan:
  - Refactoring `pr/create-dispatch-lane-pr` (it lives in the skills tree, not plan-issue-cli; touch only if Sprint 3 design dictates).
  - Changing the `plan-issue-record:v2` comment marker schema.
  - Replacing `gh` for GitHub-side calls — keep current path, just add a GitLab branch.

## Non-scope

- Adding new lifecycle profiles beyond `tracking` and `dispatch`.
- Changing the v3 issue-backed plan record contract.
- Rewriting `plan-tooling` (already provider-neutral).
- Changing the `code-review-*` SKILLs.

## Implementation boundaries

- All provider mutations route through one internal trait or function group.
- Tests cover both providers with stub backends, similar to the existing forge-cli integration test layout.
- Live GitLab smoke uses the same sandbox repo (`terrylin/agent-runtime-testing`).

## Requirements

- R1. `plan-issue --repo <owner>/<repo>` auto-detects provider from the remote URL (or the host portion of the slug when explicit).
- R2. `record open --profile {tracking,dispatch}` opens a GitLab issue with the same body + lifecycle comments as the GitHub path.
- R3. `record post`, `record audit`, `record close`, `link-pr` work identically on both providers.
- R4. Dispatch-family subcommands (`start-plan`, `start-sprint`, `ready-plan`, `accept-sprint`, `close-plan`, etc.) work on GitLab.
- R5. Existing GitHub callers see no behavior change (regression coverage in CI).
- R6. Schema versions of `plan-issue-cli.*` envelopes do not change.

## Acceptance criteria

- AC-1. `cargo test -p nils-plan-issue-cli` is green with the new provider trait + GitLab branch.
- AC-2. Sandbox revalidation: `plan-issue record open --profile tracking --repo terrylin/agent-runtime-testing --bundle docs/plans/p8-smoke` succeeds end-to-end; downstream Tier D skills marked `pass` in the sandbox source doc.
- AC-3. `forge-cli inbox`, `plan-issue record audit` etc. all read back lifecycle markers identically across providers.
- AC-4. Existing nils-cli GitHub workflows (e.g. agent-run-direnv-exec tracking) continue to work unchanged.

## Validation plan

1. **Sprint 1 (design)**: design note in this plan folder + at minimum a contract sketch and an implementation outline (no code yet).
2. **Sprint 2 (open path)**: GitHub stub-driven tests + GitLab stub-driven tests + live GitLab smoke against sandbox repo for `record open`.
3. **Sprint 3 (continue + close)**: cover the remaining surface; live GitLab smoke for `record post`, `record audit`, `record close`, `link-pr`.
4. **Sprint 4 (dispatch family + cleanup)**: cover dispatch lifecycle; downstream sandbox revalidates Tier D + Tier E.

## Open questions

- **Q1**: Should provider routing happen via subprocess-to-`forge-cli` or via direct library linkage? Subprocess keeps the layering clean and matches today's `gh` shell-out style, but multiplies process count for every comment. Library linkage is faster but pulls `forge-cli` into the dep tree. Decide in Sprint 1.
- **Q2**: How to handle "lightweight tracking" issue labels on GitLab when the catalogue is missing? Reuse `forge-cli label ensure` with the shared `manifests/forge-labels.yaml`? Or skip labels when no catalogue?
- **Q3**: GitLab issue numbering uses `iid` (per-project) rather than the cross-org GitHub numbering. The comment payload schema today carries `number`; does it need a `provider` discriminator for clarity?
- **Q4**: `create-dispatch-lane-pr` is a separate skill written GitHub-only. Should this plan absorb it (rename → `create-dispatch-lane-mr`/neutral), or leave it for a fast-follow once `plan-issue-cli` is provider-aware?
- **Q5**: Should `plan-issue` detect provider from the git remote URL of the cwd when `--repo` is omitted (current convention is "explicit only")?

## Risks and guardrails

- **R-1**: Regression on GitHub callers. Mitigation: keep all existing tests passing and add cross-provider tests for the new layer.
- **R-2**: GitLab provider permissions or label catalogues unavailable. Mitigation: graceful fallback path mirroring how `forge-cli label audit` already behaves.
- **R-3**: Subprocess-to-`forge-cli` introduces a runtime PATH dependency. Mitigation: explicit `FORGE_CLI_BIN` env override + clear error when forge-cli is missing.
- **R-4**: Schema drift between `plan-issue` and `forge-cli`. Mitigation: pin the forge-cli minor in Cargo.toml or check binary version at startup.

## Execution

- Recommended plan: `docs/plans/plan-issue-cli-provider-abstraction/plan-issue-cli-provider-abstraction-plan.md`
- Recommended execution state: `docs/plans/plan-issue-cli-provider-abstraction/plan-issue-cli-provider-abstraction-execution-state.md`
- Status: ready (design-first)
- Next-task source: Sprint 1 design note

## Retention intent

Promote to long-term docs once shipped. The design note from Sprint 1 should
live under `crates/plan-issue-cli/src/` doc comments and the runbook for
adding future providers.

## Read-first references

- Downstream sandbox source doc: `terrylin/agent-runtime-testing:docs/plans/gitlab-skill-validation/gitlab-skill-validation-discussion-source.md` (F-3 entry + evidence)
- `crates/plan-issue-cli/src/github.rs` (current implementation; ~1001 lines)
- `crates/forge-cli/src/` (the provider-neutral target surface)
- Companion tracking issue for `forge-cli` fixes: sympoies/nils-cli#483 + PR #485 (F-1, F-2, F-7)

## Source type

`discussion-to-implementation-doc`
