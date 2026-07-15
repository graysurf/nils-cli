# Plan: Provider Exact Attention Correlation

## Overview

Finish exact needs-input clearing for provider interactions that expose a stable
request correlation. Keep the existing provider-neutral v1 reducer and public
contract, add Codex app-server request/resolution projection, add a
capability-verified Claude Elicitation pair, and preserve conservative latching
for generic permission dialogs that still lack exact provider evidence.

## Read First

- Primary source:
  `docs/plans/2026-07-15-provider-exact-attention-correlation/provider-exact-attention-correlation-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Existing foundation: archived plan
  `2026-07-12-codex-app-server-auto-resume`, issue #1151, and merged PR #1154.
- Open questions carried into execution: whether installed Claude 2.1.210 emits
  the same non-empty `elicitation_id` on both callbacks for form and URL flows.

## Scope

- In scope: shared v1 invariant regression coverage; Codex app-server blocking
  request/resolution projection; Codex hook/protocol reconciliation; Claude
  Elicitation setup and exact mapping when proven; provider evidence and doctor
  updates; live Agent Console acceptance.
- Out of scope: v2 schemas, a new provider framework, runtime-kit activity
  ownership, Agent Console feature work, terminal/content heuristics, and exact
  clearing of generic Codex or Claude permission dialogs.

## Assumptions

1. `agent-session.turn-event.v1` is sufficient because it already carries
   correlated request and clear events and the reducer enforces exact removal.
2. Codex app-server exact evidence is admitted only for versions covered by the
   existing audited runtime capability gate.
3. Claude exact Elicitation is enabled only for installed/live payload shapes
   carrying the same non-empty id in request and result.
4. Current Agent Console v1 behavior is an acceptance target, not an expected
   edit target.

## Sprint 1: Freeze the Evidence Contract

**Goal**: Turn the shared invariant and provider capability boundaries into
testable gates before production changes.

**Demo/Validation**:

- Commands: focused activity/app-server tests and sanitized provider fixture
  inspection.
- Verify: progress cannot clear attention; exact ids can; installed Claude
  Elicitation is classified without retaining content.

### Task 1.1: Declare contract delta and capture test-first evidence

- **Location**:
  - `crates/agent-session/src/activity.rs`
  - `crates/agent-session/src/codex_app_server.rs`
  - `crates/agent-session/tests/fixtures/activity/`
- **Description**: Record affected-test decisions and add failing fixtures for
  Codex exact request/resolve, duplicate-source reconciliation, Claude
  Elicitation correlation, and the cross-provider rule that unrelated progress
  never clears attention.
- **Dependencies**:
  - none
- **Complexity**: 5
- **Acceptance criteria**:
  - Pre-edit failures demonstrate the missing Codex and Claude mappings.
  - Existing AskUserQuestion and conservative permission fixtures remain in the
    regression set.
  - Fixtures contain only allowlisted metadata and discarded sentinels.
- **Validation**:
  - focused `cargo test -p nils-agent-session` filters for activity and
    app-server correlation.

### Task 1.2: Verify installed provider capability shapes

- **Location**:
  - runtime `agent-out` evidence directory
  - `crates/agent-session/docs/provider-turn-signal-evidence.md`
- **Description**: Regenerate the installed Codex schema and capture sanitized
  Claude form/URL Elicitation callbacks. Record id presence, lifecycle order,
  and degradation without retaining message, URL, schema, response, prompt, or
  transcript content.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 5
- **Acceptance criteria**:
  - Codex request/resolution shapes match the metadata allowlist.
  - Claude form and URL flows are individually classified as exact or
    conservative based on matching non-empty ids.
  - Missing or changed fields produce an explicit limitation rather than
    guessed correlation.
- **Validation**:
  - Allocate the schema directory with
    `agent-out project --repo . --topic provider-exact-attention-schema --mkdir`,
    then run `codex app-server generate-json-schema --experimental` with that
    directory as `--out`.
  - sanitized fixture schema and allowlist checks.

## Sprint 2: Implement Codex Exact Attention

**Goal**: Make managed app-server requests exactly clearable without
double-counting the existing uncorrelated permission hook.

**Demo/Validation**:

- Commands: focused projection, reducer, replay, and privacy tests.
- Verify: concurrent requests clear independently; hook/protocol order does not
  change visible state.

### Task 2.1: Project app-server request and resolution metadata

- **Location**:
  - `crates/agent-session/src/codex_app_server.rs`
  - `crates/agent-session/src/activity.rs`
- **Description**: Extend the strict observation allowlist for recognized
  blocking request methods and `serverRequest/resolved`. Validate thread/turn
  scope, project the JSON-RPC id, and emit stable v1 request/clear events without
  persisting request content.
- **Dependencies**:
  - Task 1.2
- **Complexity**: 8
- **Acceptance criteria**:
  - Each admitted request produces the correct v1 attention kind.
  - A matching resolution clears only its exact request; unmatched resolution
    is an idempotent no-op.
  - Malformed, mismatched, oversized, stale, or queue-dropped unique evidence
    follows existing fail-close behavior and never manufactures a clear.
- **Validation**:
  - focused app-server projection and activity reducer tests.

### Task 2.2: Reconcile duplicate Codex hook and protocol evidence

- **Location**:
  - `crates/agent-session/src/activity.rs`
  - private runtime-bound activity persistence
- **Description**: Add a bounded per-runtime/per-turn ledger that pairs one
  permission-hook placeholder with one exact app-server approval in either
  arrival order and retains resolved tombstones through turn boundary.
- **Dependencies**:
  - Task 2.1
- **Complexity**: 8
- **Acceptance criteria**:
  - Hook-first, protocol-first, delayed-hook-after-resolution, replay, restart,
    and reconnect fixtures converge to one visible request.
  - Concurrent exact ids remain independent and counts stay correct.
  - Hook-only requests stay latched; protocol-only requests clear exactly.
  - Ledger bounds and lifecycle cleanup are deterministic.
- **Validation**:
  - focused reconciliation, concurrency, persistence, and retention tests.

## Sprint 3: Implement Claude Exact Elicitation

**Goal**: Use Claude's explicit request/result lifecycle when a shared
`elicitation_id` is available, without weakening generic permission safety.

**Demo/Validation**:

- Commands: setup/normalizer fixtures and an installed Claude canary.
- Verify: exact Elicitation clears; identifier-less and generic permissions stay
  conservative.

### Task 3.1: Add capability-gated Elicitation hooks and normalization

- **Location**:
  - `crates/agent-session/src/activity.rs`
  - agent-session-owned Claude setup specification and fixtures
- **Description**: Add `Elicitation` and `ElicitationResult` setup entries,
  normalize matching ids into stable v1 request/clear events, map URL mode to
  authentication and other admitted modes to clarification, and discard
  content-bearing fields.
- **Dependencies**:
  - Task 1.2
- **Complexity**: 7
- **Acceptance criteria**:
  - Proven exact callback pairs clear only their projected id.
  - Existing AskUserQuestion exact handling remains unchanged.
  - Missing-id result fixtures do not create an uncorrelated clear.
  - Add/apply/repair/remove remain additive, idempotent, reversible, and
    preserve unrelated Claude configuration.
- **Validation**:
  - focused Claude normalization and setup parity tests.

### Task 3.2: Publish provider capability and degradation status

- **Location**:
  - `crates/agent-session/docs/turn-state-contract.md`
  - `crates/agent-session/docs/provider-turn-signal-evidence.md`
  - activity doctor output and tests
- **Description**: Document exact and conservative paths separately, report
  installed capability limits without payload content, and make the generic
  permission limitation explicit.
- **Dependencies**:
  - Task 2.2
  - Task 3.1
- **Complexity**: 4
- **Acceptance criteria**:
  - Docs/doctor distinguish managed Codex exact requests, Claude exact
    AskUserQuestion/Elicitation, and generic uncorrelated permissions.
  - No text implies arbitrary PostToolUse resolves an approval.
  - No v2 or Agent Console migration is introduced.
- **Validation**:
  - focused doctor/setup tests and docs contract lint.

## Sprint 4: Integrate and Deliver

**Goal**: Prove both lanes through existing session and consumer surfaces, then
merge reviewed changes without closing the tracker early.

**Demo/Validation**:

- Commands: repository gates, managed provider sessions, polling/SSE readback,
  and current Agent Console inspection.
- Verify: exact cues clear for both proven flows; generic permissions remain
  conservatively latched.

### Task 4.1: Run repository and live provider acceptance

- **Location**:
  - nils-cli workspace
  - managed Codex and Claude tmux sessions
  - current Agent Console deployment
- **Description**: Run scoped validation and live canaries for one Codex exact
  request/resolution and one Claude exact Elicitation pair, then inspect
  list/glance, activity SSE, and the unchanged consumer.
- **Dependencies**:
  - Task 3.2
- **Complexity**: 6
- **Acceptance criteria**:
  - `--local-fast` and docs-only gates pass.
  - Polling/SSE expose unchanged v1 payloads with correct transitions.
  - Agent Console clears exact cues without a new frontend build.
  - Generic permission fixtures remain latched until lifecycle cleanup or
    presentation dismissal.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
  - live provider/session/consumer acceptance record.

### Task 4.2: Deliver implementation PRs and close the tracker

- **Location**:
  - `sympoies/nils-cli` provider PRs
  - L2 tracking issue and execution-state dashboard
- **Description**: Deliver provider lanes as reviewable PRs, run specialist
  review and provider checks, checkpoint after each merge, and close only after
  a completion audit proves every task and validation.
- **Dependencies**:
  - Task 4.1
- **Complexity**: 5
- **Acceptance criteria**:
  - Required checks, review threads/tasks, and tracker audit are green.
  - All implementation PRs are merged to `main`.
  - Execution state is terminal and the generic-permission limitation remains
    accurately recorded.
- **Validation**:
  - `forge-cli pr deliver --no-merge`, specialist review, merge, and
    `plan-issue tracking audit --require-complete`.

## Testing Strategy

- Unit: payload allowlists, id projection, attention kinds, exact mapping,
  ledger pairing, bounds, and lifecycle cleanup.
- Reducer: concurrency, duplicate delivery, either arrival order, replay,
  restart, reconnect, delayed hook, unmatched resolution, and progress.
- Setup/doctor: reversible Claude hooks, version capability reporting, and
  sanitized failure modes.
- Integration: app-server proxy, local hook ingest, persistence, list/glance,
  and SSE convergence.
- Live/manual: one exact Codex request and one exact Claude Elicitation through
  unchanged Agent Console.

## Risks & gotchas

- Claude documents `elicitation_id` as optional. Exact support is per proven
  shape, not a blanket provider claim.
- Codex can deliver both hook and protocol evidence. The ledger must not use
  timing alone or merge concurrent exact ids.
- Provider schemas can drift. Recognized malformed unique evidence fails closed
  and capability status changes with the mapping.
- Raw payloads contain sensitive content. Retain only synthetic fixtures or
  sanitized metadata projections.
- Any discovered public breaking need triggers re-triage before code changes.

## Rollback plan

- Revert the affected provider PR while retaining v1 reducer and conservative
  hook behavior.
- Remove new Claude setup entries through the existing reversible setup/remove
  path if the provider rejects them.
- Disable admission for a drifted provider version and report conservative
  status; never replace exact failure with heuristic clearing.
