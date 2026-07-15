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

This remains one Codex/Claude design. Both providers share the same reducer
invariant and stable v1 contract. They differ only in the evidence their
adapters can safely normalize. The design does not revive the proposed v2
schema and does not assume that every provider dialog is exactly correlatable.

## Decision

Use one shared contract, provider-specific evidence adapters, and explicit
runtime source authority:

1. Keep `agent-session.turn-event.v1` and `agent-session.turn-state.v1` as the
   provider-neutral boundary. Do not introduce v2 or a new provider trait before
   evidence requires one.
2. For an agent-session-managed Codex app-server runtime, select protocol
   authority only when capability evidence proves that the app-server request
   surface is complete for the admitted interaction matrix and generic
   permission-hook reporting is disabled for that runtime. Project the typed
   JSON-RPC request id and clear it only on the matching
   `serverRequest/resolved` notification.
3. For raw, unmanaged, or deliberately hook-authoritative Codex runtimes,
   retain conservative generic hook latching. Do not attempt to pair an
   identifier-less hook with an exact protocol request.
4. Select Codex authority once at runtime creation or resume. Do not switch
   authority mid-runtime: delayed events make that ambiguous. If completeness
   cannot be proven, select hook authority. If an admitted protocol projection
   becomes unhealthy or an unexpected generic permission hook reaches ingest,
   fail closed to an explicit degraded or unknown activity result for the rest
   of that runtime and require a new runtime/resume before selecting fallback.
5. Extend Claude exact attention beyond the already-supported
   `AskUserQuestion` path only when `Elicitation` and `ElicitationResult` carry
   the same non-empty `elicitation_id`. If installed/live evidence does not
   prove that pair, a conservative Claude outcome is a valid completed lane,
   not a blocker for Codex.
6. Keep generic Codex and Claude permission dialogs conservative. Their hooks
   expose no identifier shared with the later tool event, so arbitrary
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
- Consumer dismissal suppresses one rendered fingerprint locally. It does not
  mutate or clear producer-owned pending attention.

The existing normalized v1 event is the provider adapter boundary. Each exact
lane projects provider-private identifiers through a runtime-scoped hash before
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

The remaining work is additive provider evidence projection and capability
reporting. No contract migration or coordinated runtime-kit/Agent Console
implementation lane is required [F1] [F5] [F6].

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
  dismissal for attention that cannot be correlated. Dismissal is presentation
  state only; no frontend change is expected [F5].
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

The installed schema defines JSON-RPC `RequestId` as `string | int64` [A2]. A
safe projection must preserve that type before hashing, so integer `1` and
string `"1"` cannot alias. Raw ids never cross the adapter boundary.

The proxy does not yet project these methods. The generic Codex
`PermissionRequest` hook may also fire but exposes no identifier shared with an
app-server request or later `PostToolUse` [F2] [F4] [W2]. A private pairing
ledger cannot distinguish a delayed duplicate hook from a genuinely new
same-turn hook-only request. Therefore the two sources are arbitrated, never
reconciled heuristically.

Current semantic deduplication also excludes `attention_id` for Codex approval
requests [F2]. Exact protocol events must either include their projected id in
the semantic key or bypass semantic deduplication while retaining event-id
replay protection; otherwise concurrent exact approvals can collapse before
the reducer sees them.

### Claude evidence lane

Claude documents `Elicitation` for mid-task MCP user input and
`ElicitationResult` after response. Both schemas contain optional
`elicitation_id`; form and URL behavior must therefore be verified on installed
Claude 2.1.210 instead of assumed [W3] [A1].

When both callbacks carry the same non-empty id, v1 is sufficient: project the
id, request attention, then clear only that id. URL mode maps to
`authentication`; other admitted modes map to `clarification`. Message, URL,
requested schema, response content, and decisions are discarded.

When the installed or live callback pair lacks a matching id, the Claude lane
ends in a documented limited state: record a conservative request when safely
observable and do not emit an uncorrelated clear. Generic Claude permissions
remain different. `PermissionRequest` omits `tool_use_id`.
`PermissionDenied` includes one but covers an auto-mode classifier denial, not
the user resolving a manual permission dialog [W3].

## Required behavior

### Shared capability and failure boundary

1. Exact-attention admission is separate from the provider's baseline minimum
   version. Use a capability-specific audited version range or verified runtime
   probes for event shape, request-surface completeness, and source suppression;
   a newer or incomplete provider is conservative/unverified, not silently
   exact-capable.
2. Setup presence alone is not capability proof. Doctor and evidence docs must
   distinguish installed callbacks, verified correlated payloads, conservative
   support, and unhealthy projection.
3. Failure is asymmetric. A lost/malformed resolution never clears a latch. A
   recognizable request whose exact id cannot be retained must conservatively
   latch through a bounded fallback correlation or mark activity explicitly
   degraded/unknown before the proxy continues; it must not remain falsely
   `working`.
4. Exact live acceptance must observe a matching request and clear while the
   same turn remains active. A later `Stop` or new turn is not proof that the
   exact-clear adapter worked.

### Codex app-server adapter

1. At runtime creation/resume, select `protocol-authoritative` only when the
   managed app-server capability is admitted and healthy, its recognized
   request methods are proven complete for the supported interaction matrix,
   and runtime-injected authority makes the generic `PermissionRequest` hook
   reporter a no-op. Otherwise select `hook-authoritative`.
2. In protocol-authoritative mode, all covered attention comes from the
   app-server protocol. The generic permission reporter is suppressed at the
   source. If such a hook nevertheless reaches ingest, treat it as a capability
   invariant breach: emit neither request nor progress, mark activity
   degraded/unknown for the rest of the runtime, and do not restore health when
   a later protocol request arrives.
3. In hook-authoritative mode, generic permission hooks retain the existing
   conservative latch and protocol attention projection is not admitted.
4. Validate a recognized protocol request against the bound thread and active
   turn when present. Canonicalize the bounded request id as a typed value
   (`s:<value>` or `i:<canonical-i64>`) before runtime-scoped hashing.
5. Emit an authoritative provider-structured v1 request, derive the same
   projected id from `serverRequest/resolved`, and clear only that request.
   Unmatched/repeated resolutions are idempotent no-ops.
6. Preserve distinct exact ids through semantic deduplication. Event-id replay
   protection remains active.
7. Do not switch authority mid-runtime. If protocol projection becomes
   unhealthy or source suppression is violated, expose degraded/unknown
   activity without manufacturing a clear; a new runtime/resume may select
   conservative hook authority.

### Claude hook adapter

1. Add `Elicitation` and `ElicitationResult` to agent-session-owned Claude
   activity setup without disturbing unrelated configuration.
2. Capture sanitized installed-version fixtures proving whether form and URL
   flows carry the same non-empty id in both callbacks.
3. For proven pairs, use the projected id for stable request/clear events and
   preserve existing `AskUserQuestion` behavior.
4. If an id is absent or mismatched, record at most a conservative request; an
   uncorrelated result must not clear it. Report this as a valid limited
   capability outcome.
5. Discard content-bearing MCP fields before normalized event construction.
6. Roll back by disabling Elicitation admission first, leaving installed
   callbacks as harmless fail-open no-ops. Do not use global
   `activity setup --remove`, which removes unrelated agent-session-managed
   Claude hooks. If physical cleanup is required, ship a targeted forward
   migration that preserves existing hook entries before reverting the binary.

## Scope

- Shared v1 invariant and runtime authority-mode regression coverage.
- Codex app-server typed request-id projection, exact events, semantic-dedupe
  safety, source arbitration, failure behavior, capability evidence, and
  docs/doctor updates.
- Claude Elicitation capability probe, setup, exact normalization when proven,
  conservative terminal outcome otherwise, fixtures, rollback, and docs/doctor
  updates.
- Existing Agent Console polling/SSE behavior as read-only live acceptance.

## Non-scope

- No v2 schema, new public phase/source kind, or new adapter framework unless
  implementation proves v1 insufficient.
- No runtime-kit activity ownership change or expected Agent Console code
  change.
- No terminal/transcript/UI/content heuristic.
- No exact-clear claim for generic Codex or Claude permission dialogs.
- No hook/protocol pairing ledger, timing correlation, arbitrary
  `PostToolUse` clear, or producer clear from manual UI dismissal.

## Requirements

- R1 — Both providers use the same v1 invariant and public contract; provider
  differences remain at evidence normalization.
- R2 — Exact resolution clears only the request with the same type-preserving
  projected id, including concurrent pending requests.
- R3 — Each Codex runtime selects one attention authority at creation/resume.
  Protocol authority requires proved app-server request completeness and
  source-suppressed generic permission reporting. Hook authority latches those
  hooks conservatively. An unexpected hook in protocol mode degrades the
  runtime; no mid-runtime switch or heuristic source pairing is allowed.
- R4 — Claude Elicitation clears automatically only when installed/live
  evidence proves a shared non-empty id. Otherwise the lane completes with an
  explicit conservative limitation.
- R5 — Hook-only or identifier-less producer attention remains latched until a
  lifecycle boundary. Agent Console may independently suppress its rendered
  fingerprint; that is not a producer transition.
- R6 — Malformed, oversized, mismatched, stale, unsupported, or dropped exact
  evidence never manufactures a clear or leaves a recognized attention request
  falsely reported as healthy `working`.
- R7 — Only projected ids and allowlisted classification/timestamps reach
  persistence, diagnostics, views, or SSE.
- R8 — AskUserQuestion, raw/unmanaged Codex, generic permissions, Hermes, and
  Agent Console behavior do not regress.

## Acceptance criteria

- A managed protocol-authoritative Codex request enters `needs_input`; matching
  `serverRequest/resolved` clears only it within one proxy round-trip while the
  turn remains active.
- String and integer request ids, including concurrent integer `1` and string
  `"1"`, project distinctly and clear independently. Concurrent counts follow
  `2 -> 1 -> 0` even inside the semantic-dedupe window.
- Capability fixtures prove protocol authority is unavailable unless the
  admitted interaction matrix has complete protocol request coverage and the
  generic permission reporter is suppressed for that runtime.
- Protocol-authoritative fixtures prove the reporter no-ops at source. If a
  hook bypasses suppression, hook-only and hook-before-protocol traces both
  deterministically degrade/unknown without changing `last_progress_at` or
  recovering mid-runtime. Hook-authoritative fixtures prove the same hook is
  conservatively latched.
- Queue-full, oversized, malformed, replay, reconnect, restart, and schema-drift
  fixtures prove the asymmetric degraded/unknown and no-false-clear behavior.
- Claude acceptance follows the capability matrix: matching non-empty ids
  require exact request/clear evidence while the turn remains active; absent or
  mismatched ids require the documented conservative terminal outcome.
- Existing Claude AskUserQuestion exact clearing remains green.
- Generic provider permission fixtures prove `PostToolUse` advances progress
  without clearing the producer latch; consumer dismissal is tested separately
  as presentation suppression.
- List/glance and `/activity/events` retain v1, and retained evidence contains
  no raw ids or content-bearing fields.

## Validation plan

- Declare each provider lane's contract delta and affected tests, then capture
  meaningful lane-specific test-first failures before production edits.
- Run focused Rust tests for typed correlation, concurrency, deduplication,
  authority modes, capability drift, replay/restart, privacy, bounds, and
  asymmetric fail-close behavior.
- Run `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` and
  `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`.
- Re-audit installed Codex schema into `agent-out` and capture sanitized
  installed Claude form/URL Elicitation fixtures.
- Run managed Codex and capability-selected Claude live acceptance. Hold the
  turn open until a same-projected-id `attention_requested` /
  `attention_cleared` pair and polling/SSE revision are observed.

## Risks and guardrails

- Claude documents `elicitation_id` as optional; exact support is per proven
  payload shape, not a blanket provider claim.
- Codex hooks and app-server requests share no stable key. Authority selection,
  capability-proven protocol completeness, and runtime source suppression—not a
  reconciliation ledger—prevent double counting. If any premise fails, the
  runtime degrades or starts hook-authoritative.
- Exact provider capability has its own audited range/shape evidence. A generic
  minimum version or installed hook entry is insufficient.
- Changed or lost recognized request evidence degrades activity instead of
  silently leaving it `working`; changed/lost resolution evidence never clears.
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

- [U1] User direction, paraphrased: preserve one shared Codex/Claude design,
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

- Tracking level: L2 with independent provider tasks and reviewed PRs.
- Next-task source: Sprint 1, Task 1.1 in the recommended plan.
- Retention intent: archive with the completed L2 bundle.
