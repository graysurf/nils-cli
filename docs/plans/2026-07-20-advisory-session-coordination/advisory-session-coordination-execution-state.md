# Advisory Session Coordination Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: active; implementation complete, release-quality validation and PR delivery in progress
- Target scope: advisory-by-default nils-cli session coordination, automatic
  presence and work-context UX, agent-runtime-kit hook routing, two reviewed
  merged PRs, then approved release/runtime sync and fresh-session acceptance.
- Execution window: Sprint 1 (nils-cli contract) -> Sprint 2 (runtime hooks) ->
  Sprint 3 (review/PR/merge) -> Sprint 4 (preview consent/deployment), serial
  across cross-repository dependencies.
- Current task: Task 3.1, deliver and merge nils-cli
- Next task: Task 3.2, deliver and merge agent-runtime-kit
- Last updated: 2026-07-20
- Branch/commit/PR: nils-cli and runtime-kit
  `fix/advisory-session-coordination`; commits and PRs pending final validation
- Source document: `docs/plans/2026-07-20-advisory-session-coordination/advisory-session-coordination-plan.md`
- Implementation source: `docs/plans/2026-07-20-advisory-session-coordination/advisory-session-coordination-discussion-source.md`
- Direct source-doc execution waiver: not applicable.
- Tracking issue: <https://github.com/sympoies/nils-cli/issues/1318>

## Validation Plan

- Validate the L2 bundle and nils-cli docs-only gate before attaching #1318.
- Capture meaningful test-first red evidence in nils-cli before Rust production
  edits and in runtime-kit before hook/policy production edits.
- Run focused nils-agent-session, hook/routing/privacy, completion, and
  cross-product tests, then each repository's declared validation.
- Require independent specialist review, provider checks, review-thread and
  task sweeps before each merge.
- Present exact release/runtime sync preview after both merges; wait for
  explicit approval before installed-home or live-runtime mutation.
- After approval, validate released binaries and disposable managed/unmanaged
  fresh sessions before strict issue closeout.

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | done | Lock mode, presence, and compatibility behavior with failing tests | nils test-first evidence `20260720-120434-issue-1318-nils-test-first`. | Meaningful red captured before production edits. |
| 1.2 | done | Implement automatic presence and coordination modes | Focused coordination suite and lifecycle/mode matrix pass. | Broker/session lifecycle owns bounded presence; old records default advisory. |
| 1.3 | done | Add self-targeting work-context and advisory UX | `status/set/clear/advise/acknowledge`, docs, JSON, help, and completions implemented. | No manual private JSON or copied IDs for normal use. |
| 2.1 | done | Lock hook mode routing with failing tests | runtime test-first evidence `20260720-122115-issue-1318-runtime-test-first`. | Default/advisory/off/unmanaged red captured before hook edits. |
| 2.2 | done | Implement advisory, enforce, off, and unmanaged routing | Shared hook suite and fail-open/privacy/resource regression tests pass. | Advisory never blocks; enforce compatibility remains. |
| 2.3 | done | Update policy and cross-product acceptance | Source-linked Codex/Claude acceptance passes against the nils worktree binary. | Includes first mutation, overlap, acknowledgement, degraded broker, enforce, off, and unmanaged. |
| 3.1 | in_progress | Deliver and merge nils-cli | Final local-fast, commit, provider PR/checks/review pending. | Three specialist waves resolved; final red-team returned no findings. |
| 3.2 | pending | Deliver and merge agent-runtime-kit | Pending Tasks 2.3 and 3.1. | Independent review and provider checks required. |
| 4.1 | pending | Prepare and obtain deployment preview approval | Pending both merges. | Exact version/base/commit/digest/config and rollback. |
| 4.2 | pending | Release, sync, prove fresh sessions, and close #1318 | Pending explicit approval of Task 4.1. | Close only after installed managed/unmanaged acceptance. |

## Session Log

- 2026-07-20: Maintainer clarified that session coordination is an optional
  collision-awareness benefit, not a mandatory prerequisite for agent work.
  Advisory is the default; unmanaged iTerm agents may bypass participation;
  enforce and off remain explicit modes; mechanical context bookkeeping should
  be automatic. Existing #1318 was selected as the single L2 tracker, with
  implementation/review/PR/merge authorized and deployment held behind an
  exact preview consent boundary.
- 2026-07-20: Current code audit confirmed two independent blocking layers:
  work-context admission and physical checkout writer leases. The launcher and
  broker already own enough lifecycle/repository data for automatic presence;
  raw claim APIs can remain as enforce-mode compatibility surfaces.
- 2026-07-20: Implemented advisory/enforce/off modes, automatic broker presence,
  self-targeting context commands, stable overlap acknowledgement, fail-open
  runtime hooks, explicit enforce-only checkout leases, policy/render updates,
  and a source-linked Codex/Claude acceptance harness. Three specialist review
  waves found and resolved lifecycle, migration, privacy, API, test, and
  performance defects; the final red-team and follow-up review returned no
  findings.

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `plan-archive search` for work-context/session coordination/advisory | pass | No duplicate archived plan or tracker was found; unrelated advisory hits only. | local |
| `forge-cli label audit` with the shared runtime-kit catalog | pass | nils-cli provider labels match the shared taxonomy. | provider/local |
| `plan-tooling validate --file docs/plans/2026-07-20-advisory-session-coordination/advisory-session-coordination-plan.md --format text --explain` | pass | Bundle validation passed with zero errors. | local |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` | pass | Docs placement, hygiene, Markdown, plan, CLI output, and forge fixture gates passed. | local |
| `cargo test -p nils-agent-session coordination -- --nocapture` | pass | 70 focused unit/integration coordination tests passed after review fixes. | local |
| `bash tests/hooks/run.sh` | pass | 311 shared hook tests passed; source-linked acceptance is opt-in. | runtime-kit local |
| `AGENT_SESSION_COUPLED_ACCEPTANCE=1 python3 -m unittest ...test_session_coordination_source_linked_cross_product_acceptance` | pass | Actual source binary passed Codex/Claude, acknowledgement, degraded, enforce/off, and unmanaged acceptance. | runtime-kit local |
| specialist maintainability/security/testing/API/migration/performance plus red-team | pass | All concrete findings fixed; final testing and red-team follow-ups returned `NO_FINDINGS`. | local review |

## Handoff

- Finish Task 3.1 and Task 3.2 through provider review/checks/merge. Do not
  release or sync installed runtime surfaces until Task 4.1 has been reported
  with exact immutable inputs and explicitly approved.
