# Provider Exact Attention Correlation Implementation Handoff

## Status

- Date: 2026-07-15
- Status: approved for one L2 implementation tracker; implementation not started
- Source: reevaluation of the session 7/8 provider-hook turn-state handoff
  against current source, installed provider versions and schemas, consumer
  behavior, and current official provider documentation.
- Scope: `sympoies/nils-cli` `agent-session`, with read-only live acceptance in
  the existing Agent Console consumer.

## Purpose

Replace the obsolete session 7/8 implementation sequence with the remaining
current work. Producer ownership, persistence, session projection, activity
stream, provider setup, and consumer rendering already shipped. The unresolved
problem is exact attention correlation: when a provider asks for approval or
input, `agent-session` must clear that request only when the same provider
request is known to be resolved [U1] [F1] [F2].

This is not a Codex-only redesign and it does not revive the proposed v2 schema.
Codex and Claude share the same reducer invariant and stable v1 contract, while
each provider supplies different correlation evidence.

## Decision

Use one shared contract, two provider evidence lanes, and one explicit
unsupported case:

1. Keep `agent-session.turn-event.v1` and `agent-session.turn-state.v1` as the
   provider-neutral boundary. Do not introduce v2 or a new provider trait before
   evidence requires one.
2. Implement exact Codex attention for agent-session-managed app-server
   runtimes from the JSON-RPC server request id and matching
   `serverRequest/resolved` notification.
3. Extend Claude exact attention beyond the already-supported
   `AskUserQuestion` path using `Elicitation` and `ElicitationResult` when both
   callbacks carry the same non-empty `elicitation_id`. Verify installed and
   live form/URL payloads before enabling this mapping.
4. Keep generic Codex and Claude permission dialogs conservative. Their hooks
   still expose no request id shared with the later tool event, so arbitrary
   `PostToolUse` remains progress only and never clears a permission latch.

## Shared invariant

The current reducer already expresses the provider-neutral rule [F1] [F2]:

- `attention_requested` adds one opaque runtime-scoped correlation and latches
  attention.
- `attention_cleared` removes only the matching correlation.
- A new turn, terminal completion/failure, or runtime boundary may clear all
  remaining correlations because it proves the prior interaction is over.
- Uncorrelated progress updates `last_progress_at`; it never proves a pending
  request was answered.

The existing normalized v1 event is the provider adapter boundary. Each lane
projects provider-private identifiers through the runtime-scoped hash before
persistence or serving.

## Disposition of the session 7/8 handoff

| Earlier proposal | Current state | Disposition |
| --- | --- | --- |
| Create producer-owned `turn-state.v2` | Producer owns deterministic v1 event/state contracts | Closed differently; keep v1 |
| Add hook ingest and spawn channel | Local hook ingest and runtime/session env are shipped | Complete |
| Add state to session list/glance | Optional v1 `turn_state` is served across views | Complete |
| Add `/activity/events` later | Authenticated replay/reset/heartbeat SSE exists | Complete |
| Install through `agent-runtime-kit` | `agent-session activity setup` owns configuration | Superseded |
| Clear waiting on later progress | Progress is intentionally uncorrelated | Rejected as unsafe |
| Remove every manual dismissal | Exact requests clear; uncorrelated permissions stay conservative | Partially complete |

The remaining work is additive provider evidence projection. No contract
migration or coordinated runtime-kit/Agent Console implementation lane is
required [F1] [F5] [F6].

## Confirmed current facts

### Existing producer and consumer

- V1 requires an opaque `attention_id` for request and clear. The reducer
  removes only the matching pending entry and uses lifecycle boundaries for
  whole-turn cleanup [F1] [F2].
- Runtime generation checks, replay protection, bounded persistence, metadata
  allowlisting, session views, and authenticated SSE already exist [F1] [F2]
  [F3].
- Claude `AskUserQuestion` already maps its shared `tool_use_id` across
  `PreToolUse`, `PostToolUse`, and `PostToolUseFailure`, proving that v1 supports
  exact provider correlation without a schema change [F1] [F2] [F4].
- Agent Console consumes v1 exact clears and retains fingerprint-scoped manual
  dismissal for attention that cannot be correlated. No frontend change is
  expected [F5].
- Activity installation is owned by `agent-session activity setup`; current
  runtime-kit parity intentionally excludes this product-specific bridge [F1]
  [F6].

### Codex evidence lane

Managed Codex sessions already run through a private WebSocket proxy bound to
one app-server thread. It observes bidirectional JSON-RPC, projects a strict
metadata allowlist, and already maps exact usage failures into v1 state [F7].

Official app-server docs and the installed Codex 0.144.3 schema expose blocking
server requests plus a matching resolution [W1] [A2]:

| Request method | Attention kind | Exact resolution |
| --- | --- | --- |
| `item/commandExecution/requestApproval` | `approval` | same JSON-RPC request id in `serverRequest/resolved` |
| `item/fileChange/requestApproval` | `approval` | same request id |
| `item/permissions/requestApproval` | `approval` | same request id |
| `item/tool/requestUserInput` | `clarification` | same request id; experimental/version-gated |
| `mcpServer/elicitation/request` | structural `clarification` or `authentication` | same request id |

The proxy does not yet project these methods. The generic Codex
`PermissionRequest` hook may also fire but has no identifier shared with
`PostToolUse`; without reconciliation, one request can be double-counted or a
delayed hook can reopen a resolved request [F2] [F4] [W2].

### Claude evidence lane

Claude documents `Elicitation` for mid-task MCP user input and
`ElicitationResult` after response. Both schemas contain optional
`elicitation_id`; form and URL behavior must therefore be verified on installed
Claude 2.1.210 instead of assumed [W3] [A1].

When both callbacks carry the same non-empty id, v1 is sufficient: project the
id, request attention, then clear only that id. URL mode maps to
`authentication`; other admitted modes map to `clarification`. Message, URL,
requested schema, response content, and decisions are discarded.

Generic Claude permissions remain different. `PermissionRequest` explicitly
omits `tool_use_id`. `PermissionDenied` includes one but fires only for an
auto-mode classifier denial, not when the user answers a manual permission
dialog [W3]. It cannot close the generic permission gap.

## Required behavior

### Codex app-server adapter

1. Validate a recognized request against the bound thread and active turn when
   present.
2. Project its JSON-RPC request id and emit an authoritative
   provider-structured v1 request.
3. Derive the same projected id from `serverRequest/resolved` and emit the
   matching clear.
4. Treat unmatched or repeated resolutions as idempotent no-ops; never clear a
   different request.
5. Keep a private bounded runtime/turn reconciliation ledger:
   - pair one permission-hook placeholder with one exact app-server approval in
     either arrival order;
   - replace the placeholder rather than increment visible pending count;
   - retain a resolved tombstone until turn boundary so a delayed hook cannot
     reopen the request;
   - keep hook-only requests latched and protocol-only requests exactly
     clearable; and
   - keep concurrent exact request ids independent.

### Claude hook adapter

1. Add `Elicitation` and `ElicitationResult` to agent-session-owned Claude
   activity setup without disturbing unrelated configuration.
2. Capture sanitized installed-version fixtures proving whether form and URL
   flows carry the same non-empty id in both callbacks.
3. For proven pairs, use the projected id for stable request/clear events and
   preserve existing `AskUserQuestion` behavior.
4. If an id is absent, record at most a conservative request; an uncorrelated
   result must not clear it. Report the limitation rather than inventing
   correlation.
5. Discard content-bearing MCP fields before normalized event construction.

## Scope

- Shared regression tests for exact clear and progress-never-clears behavior.
- Codex app-server projection, exact events, duplicate reconciliation,
  capability/version evidence, and docs/doctor updates.
- Claude Elicitation capability probe, setup, exact normalization when proven,
  conservative degradation otherwise, fixtures, and docs/doctor updates.
- Existing Agent Console polling/SSE behavior as read-only live acceptance.

## Non-scope

- No v2 schema, new public phase/source kind, or new adapter framework unless
  implementation proves v1 insufficient.
- No runtime-kit activity ownership change or expected Agent Console code
  change.
- No terminal/transcript/UI/content heuristic.
- No exact-clear claim for generic Codex or Claude permission dialogs.
- No clearing on arbitrary `PostToolUse`, elapsed time, terminal activity, or
  manual UI dismissal.

## Requirements

- R1 — Both providers use the same v1 invariant and public contract; provider
  differences remain at evidence normalization.
- R2 — Exact resolution clears only the request with the same projected id,
  including concurrent pending requests.
- R3 — Codex hook/protocol duplicates converge to one visible request across
  either arrival order, replay, restart, and delayed delivery.
- R4 — Claude Elicitation clears automatically only when installed/live
  evidence proves a shared non-empty id.
- R5 — Hook-only or identifier-less requests remain conservatively latched
  until a lifecycle boundary or existing manual presentation dismissal;
  progress does not clear them.
- R6 — Malformed, oversized, mismatched, stale, unsupported, or dropped exact
  evidence never manufactures a clear.
- R7 — Only projected ids and allowlisted classification/timestamps reach
  persistence, diagnostics, views, or SSE.
- R8 — AskUserQuestion, raw/unmanaged Codex, generic permissions, Hermes, and
  Agent Console behavior do not regress.

## Acceptance criteria

- A managed Codex request enters `needs_input`; matching
  `serverRequest/resolved` clears only it within one proxy round-trip.
- Two concurrent Codex requests count as two and clear independently.
- Hook-first, protocol-first, delayed-hook-after-resolution, duplicate,
  reconnect/replay, and restart fixtures converge without double-count/reopen.
- A live Claude Elicitation pair with a shared id enters `needs_input` and
  clears through its result; identifier-less fixtures remain conservative.
- Existing Claude AskUserQuestion exact clearing remains green.
- Generic provider permission fixtures prove `PostToolUse` advances progress
  without clearing the latch.
- List/glance and `/activity/events` retain v1, and Agent Console clears exact
  cues without a frontend change.
- Retained fixtures/artifacts contain no raw ids or content-bearing fields.

## Validation plan

- Declare the contract delta and affected tests, then capture meaningful
  test-first failures before production edits.
- Run focused Rust tests for correlation, concurrency, reconciliation,
  replay/restart, privacy, bounds, and fail-close behavior.
- Run `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` and
  `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`.
- Re-audit installed Codex schema into `agent-out` and capture sanitized
  installed Claude form/URL Elicitation fixtures.
- Run managed Codex/Claude live acceptance through polling, SSE, and current
  Agent Console.

## Risks and guardrails

- Claude documents `elicitation_id` as optional; exact support is per proven
  payload shape, not a blanket provider claim.
- Codex reconciliation is one-to-one, bounded, runtime/turn scoped, and keeps
  resolved tombstones. Timing alone never correlates or clears attention.
- Changed recognized provider shapes fail closed behind audited capability
  evidence.
- Classification uses event/method and structural mode only. Raw ids are
  projected; content fields are discarded in memory.
- Any required public breaking change stops execution for re-triage instead of
  silently introducing v2.

## Current-state and recurrence audit

- Installed audit: Claude Code 2.1.210, Codex 0.144.3, workspace agent-session
  1.22.4 [A1].
- Archived `2026-07-12-codex-app-server-auto-resume` and merged PR #1154
  established the app-server foundation; this plan extends it [A3].
- Open nils-cli issue #1118 concerns hook configuration representation, not
  exact attention correlation, and is not a duplicate [A4].

## Sources

- [U1] User direction, paraphrased: preserve one shared Codex/Claude hook design,
  reassess current reality, rewrite the document, open an L2 tracker, and commit
  its documents to `main`.
- [F1] `crates/agent-session/docs/turn-state-contract.md`.
- [F2] `crates/agent-session/src/activity.rs`.
- [F3] `crates/agent-session/docs/specs/activity-stream-v1.md` and
  `crates/agent-session/src/serve.rs`.
- [F4] `crates/agent-session/docs/provider-turn-signal-evidence.md`.
- [F5] Current `serenvia/agent-console` activity consumer, inspected at
  `d886e95e2b31696a2ec030cc2b9935b9879bef2c`.
- [F6] Current `graysurf/agent-runtime-kit` hook parity, inspected at
  `ae147fde8a8d18af6a07fb8e87e5e364ebe7075c`.
- [F7] `crates/agent-session/src/codex_app_server.rs`.
- [W1] [Codex app-server](https://developers.openai.com/codex/app-server).
- [W2] [Codex hooks](https://developers.openai.com/codex/hooks).
- [W3] [Claude Code hooks](https://code.claude.com/docs/en/hooks).
- [A1] Local version audit on 2026-07-15.
- [A2] Installed Codex 0.144.3 generated app-server schema audit.
- [A3] Plan archive entry for app-server auto-resume, issue #1151, PR #1154.
- [A4] Open nils-cli issue #1118 audit on 2026-07-15.

## Execution

Recommended plan: docs/plans/2026-07-15-provider-exact-attention-correlation/provider-exact-attention-correlation-plan.md

Recommended execution state: docs/plans/2026-07-15-provider-exact-attention-correlation/provider-exact-attention-correlation-execution-state.md

- Tracking level: L2 with provider-specific tasks and reviewed PRs.
- Next-task source: Sprint 1, Task 1.1 in the recommended plan.
- Retention intent: archive with the completed L2 bundle.
