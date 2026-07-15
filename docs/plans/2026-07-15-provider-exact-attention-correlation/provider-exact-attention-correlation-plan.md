# Plan: Provider Exact Attention Correlation

## Overview

Finish exact needs-input clearing for provider interactions that expose a stable
request correlation. Keep the existing provider-neutral v1 reducer and public
contract, use one preselected attention authority per Codex runtime, add a
capability-verified Claude Elicitation pair when available, and preserve
conservative latching for generic permission dialogs.

## Read First

- Primary source:
  `docs/plans/2026-07-15-provider-exact-attention-correlation/provider-exact-attention-correlation-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Existing foundation: archived plan
  `2026-07-12-codex-app-server-auto-resume`, issue #1151, and merged PR #1154.
- Open questions carried into execution: whether installed Claude 2.1.210 emits the same
  non-empty `elicitation_id` on both callbacks for form and URL flows. Exact and
  conservative results are both valid terminal outcomes for the Claude lane.

## Scope

- In scope: shared v1 invariant and authority-mode coverage; Codex app-server
  typed request/resolution projection; exact-id-safe semantic deduplication;
  Codex runtime source arbitration and failure degradation; Claude Elicitation
  capability evidence, setup, and exact-or-limited delivery; provider-specific
  doctor/docs updates; live Agent Console acceptance.
- Out of scope: v2 schemas, a new provider framework, hook/protocol pairing,
  runtime-kit activity ownership, Agent Console feature work, terminal/content
  heuristics, and exact clearing of generic provider permission dialogs.

## Assumptions

1. `agent-session.turn-event.v1` is sufficient because it already carries
   correlated request and clear events and the reducer enforces exact removal.
2. Exact-attention admission needs capability-specific audited evidence; the
   provider's baseline version floor or an installed hook entry is insufficient.
3. Codex chooses protocol or hook attention authority at runtime creation or
   resume and never changes it mid-runtime.
4. Claude exact Elicitation is enabled only for installed/live payload shapes
   carrying the same non-empty id in request and result.
5. Current Agent Console v1 behavior is an acceptance target, not an expected
   edit target; local dismissal is presentation suppression, not producer clear.

## Sprint 1: Freeze the Shared Boundary

**Goal**: Record the provider-neutral invariant, runtime authority rule, and
test ownership before either provider lane changes production behavior.

**Demo/Validation**:

- Commands: focused existing activity reducer tests and plan validation.
- Verify: exact id clears only itself; progress never clears; provider test-first
  deltas belong to their independent lanes.

### Task 1.1: Freeze the shared invariant and authority-mode contract

- **Location**:
  - `crates/agent-session/docs/turn-state-contract.md`
  - `crates/agent-session/src/activity.rs`
  - provider lane test-decision records
- **Description**: Confirm the already-green v1 reducer regression for exact
  clear and progress-never-clears behavior. Record that each runtime has one
  attention authority, that source authority never changes mid-runtime, and
  that consumer dismissal cannot mutate producer state. Assign every new
  failing fixture to the Codex or Claude lane that will implement it.
- **Dependencies**:
  - none
- **Complexity**: 3
- **Acceptance criteria**:
  - Existing exact-clear, lifecycle cleanup, and progress regression tests pass.
  - The authority-mode contract has no hook/protocol pairing or timing rule.
  - Codex and Claude can proceed independently after this task.
  - A blocked or conservative Claude outcome cannot prevent a green Codex PR.
- **Validation**:
  - focused `cargo test -p nils-agent-session` activity reducer filters.

## Sprint 2: Deliver Codex Exact Attention

**Goal**: Make admitted managed app-server requests exactly clearable while
keeping raw/unmanaged Codex conservative and avoiding ambiguous source mixing.

**Demo/Validation**:

- Commands: installed schema audit plus focused projection, reducer,
  deduplication, authority, failure, replay, and privacy tests.
- Verify: typed concurrent ids clear independently; one source owns attention
  for the lifetime of each runtime.

### Task 2.1: Verify Codex capability and capture test-first evidence

- **Location**:
  - runtime `agent-out` evidence directory
  - `crates/agent-session/src/codex_app_server.rs`
  - `crates/agent-session/src/activity.rs`
  - Codex fixtures and affected-test decision record
- **Description**: Regenerate the installed Codex schema, define a
  capability-specific audited version range or verified shape probe, and add
  failing fixtures for every admitted request/resolution method, typed
  `RequestId`, semantic-dedupe independence, authority modes, schema drift, and
  asymmetric observation loss before editing Codex production logic.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 6
- **Acceptance criteria**:
  - Installed schema evidence records `RequestId` as `string | int64` and the
    matching `serverRequest/resolved` shape without retaining provider ids.
  - Failing tests distinguish integer `1` from string `"1"` and cover matching
    clears for both variants.
  - Failing concurrency tests require two same-kind requests inside one second
    to count `2 -> 1 -> 0` rather than collapse in semantic deduplication.
  - Failing failure tests cover recognized requests and resolutions under
    malformed, oversized, stale, and queue-full observations.
  - Versions or shapes outside exact-attention audit evidence are expected to
    report unverified/conservative, not supported.
- **Validation**:
  - Allocate evidence with
    `agent-out project --repo . --topic provider-exact-attention-schema --mkdir`
    and run `codex app-server generate-json-schema --experimental` into it.
  - focused red `cargo test -p nils-agent-session` Codex filters.

### Task 2.2: Implement typed Codex projection and fail-closed reduction

- **Location**:
  - `crates/agent-session/src/codex_app_server.rs`
  - `crates/agent-session/src/activity.rs`
- **Description**: Admit recognized blocking request methods and
  `serverRequest/resolved`; validate thread/turn scope; canonicalize bounded
  request ids as type-tagged string or canonical int64 before runtime-scoped
  hashing; emit v1 request/clear events; and preserve exact ids through semantic
  deduplication while retaining event-id replay protection. Implement asymmetric
  degradation for evidence loss.
- **Dependencies**:
  - Task 2.1
- **Complexity**: 8
- **Acceptance criteria**:
  - Each admitted method produces the correct v1 attention kind.
  - Matching resolution clears only the same typed projected id; unmatched or
    repeated resolutions are idempotent no-ops.
  - Integer `1` and string `"1"` never alias, including concurrent delivery.
  - Distinct exact requests survive semantic deduplication; event replay remains
    suppressed.
  - Lost/malformed resolutions retain existing latches. A recognizable request
    whose exact id cannot be retained produces a bounded conservative latch or
    explicit degraded/unknown activity before the proxy continues; it never
    remains falsely healthy `working`.
  - Only allowlisted classification, projected ids, and timestamps leave the
    proxy adapter.
- **Validation**:
  - focused projection, reducer, deduplication, concurrency, bounds, privacy,
    replay, and queue-loss tests.

### Task 2.3: Implement Codex runtime source arbitration and capability status

- **Location**:
  - `crates/agent-session/src/activity.rs`
  - managed Codex runtime/proxy lifecycle
  - activity doctor output and fixtures
- **Description**: Select attention authority at runtime creation/resume. In an
  admitted healthy managed app-server runtime, protocol requests are the sole
  attention source and generic `PermissionRequest` hooks are diagnostic/progress
  only. In raw/unmanaged or preselected conservative mode, hooks retain their
  latch and exact protocol projection is not admitted. Never switch authority
  mid-runtime; projection failure marks the runtime's exact-attention state
  unhealthy/degraded until a new runtime/resume.
- **Dependencies**:
  - Task 2.2
- **Complexity**: 8
- **Acceptance criteria**:
  - Protocol-authoritative fixtures ignore generic hooks for attention and
    clear independent exact requests through protocol resolution.
  - Hook-authoritative fixtures latch generic permission hooks and never claim
    an exact clear.
  - Adversarial hook-only A plus protocol-only B, delayed hook, reconnect,
    restart, and replay fixtures use deterministic source authority rather than
    pairing, tombstones, or arrival timing.
  - Runtime authority cannot change after creation/resume.
  - Exact-attention status uses its own audited range/shape evidence; a newer
    unverified provider and an unhealthy projection are not reported supported.
- **Validation**:
  - focused authority-mode, runtime lifecycle, doctor, adversarial mixed-loss,
    reconnect, restart, and replay tests.

## Sprint 3: Deliver Claude Exact or Conservative Elicitation

**Goal**: Use Claude's explicit request/result lifecycle when a matching
`elicitation_id` is proven, and otherwise finish the lane with an accurate
conservative capability result.

**Demo/Validation**:

- Commands: sanitized installed Claude canary plus branch-selected setup,
  normalizer, doctor, and rollback tests.
- Verify: exact Elicitation clears before `Stop` when supported; the limited
  branch never emits an uncorrelated clear.

### Task 3.1: Verify Claude capability and capture branch-specific test-first evidence

- **Location**:
  - runtime `agent-out` evidence directory
  - `crates/agent-session/docs/provider-turn-signal-evidence.md`
  - Claude setup/normalizer fixtures and affected-test decision record
- **Description**: Capture sanitized form and URL `Elicitation` /
  `ElicitationResult` callbacks on installed Claude 2.1.210. Select the exact
  branch only when both sides carry the same non-empty `elicitation_id`; select
  the conservative branch otherwise. Add meaningful failing fixtures for the
  selected production delta before editing Claude production logic.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 5
- **Acceptance criteria**:
  - Form and URL flows are independently classified exact or conservative.
  - Exact selection has a failing same-id request/clear fixture and capability
    status fixture.
  - Conservative selection has a failing limitation/doctor fixture and proves
    identifier-less results never clear a latch.
  - Setup presence and a generic minimum version are not treated as proof that
    the callback carries a correlated id.
  - Captured evidence excludes message, URL, schema, response, prompt, decision,
    and transcript content.
- **Validation**:
  - sanitized fixture schema/allowlist checks and focused branch-selected red
    `cargo test -p nils-agent-session` filters.

### Task 3.2: Implement the selected Claude exact or conservative branch

- **Location**:
  - `crates/agent-session/src/activity.rs`
  - agent-session-owned Claude setup specification and fixtures
  - activity doctor output
- **Description**: For exact-capable shapes, add Elicitation setup entries and
  normalize matching ids into stable v1 request/clear events, mapping URL mode
  to authentication and other admitted modes to clarification. For a limited
  shape, publish conservative status and never manufacture a clear. In both
  branches, preserve `AskUserQuestion`, generic permission safety, privacy, and
  an executable selective rollback.
- **Dependencies**:
  - Task 3.1
- **Complexity**: 7
- **Acceptance criteria**:
  - Exact branch: request/result clear only the same projected id while the turn
    remains active; missing/mismatched results do not clear.
  - Conservative branch: the lane reaches a valid terminal state with explicit
    docs/doctor limitation and no false exact-support claim.
  - Existing `AskUserQuestion` exact handling and unrelated Claude config remain
    unchanged.
  - Add/apply/repair are additive and idempotent. Rollback first disables
    Elicitation admission so installed callbacks become harmless no-ops.
  - Global `activity setup --remove` is not used for selective rollback. Any
    physical cleanup is a targeted forward migration that preserves existing
    AskUserQuestion, permission, stop, and notification entries before binary
    rollback.
- **Validation**:
  - focused Claude normalization, setup parity, capability, privacy, and
    selective rollback tests.

## Sprint 4: Integrate and Deliver

**Goal**: Publish the shared capability matrix, prove both independent provider
outcomes through existing surfaces, and merge reviewed PRs without closing the
tracker early.

**Demo/Validation**:

- Commands: docs/doctor checks, repository gates, managed provider sessions,
  polling/SSE readback, and current Agent Console inspection.
- Verify: exact same-id clears occur before lifecycle boundaries where
  supported; conservative outcomes remain accurately limited.

### Task 4.1: Publish capability status and run integration acceptance

- **Location**:
  - `crates/agent-session/docs/turn-state-contract.md`
  - `crates/agent-session/docs/provider-turn-signal-evidence.md`
  - activity doctor output
  - managed Codex and Claude sessions
  - current Agent Console deployment
- **Description**: Publish exact, conservative, unverified, and unhealthy paths;
  run repository gates; then perform live Codex and branch-selected Claude
  acceptance through list/glance, SSE, and the unchanged consumer. Exact live
  canaries must retain same-projected-id request/clear evidence and keep the turn
  open until the clear is visible.
- **Dependencies**:
  - Task 2.3
  - Task 3.2
- **Complexity**: 7
- **Acceptance criteria**:
  - Docs/doctor distinguish managed Codex protocol authority, raw/unmanaged
    hook authority, Claude exact AskUserQuestion/Elicitation, Claude limited
    Elicitation, and generic uncorrelated permissions.
  - Exact Codex acceptance shows `needs_input -> working` on the matching clear
    before `Stop`, with the corresponding polling/SSE revision sequence.
  - Exact Claude branch shows the same pre-`Stop` proof; conservative branch
    records the limitation and verifies no uncorrelated clear.
  - Polling/SSE expose unchanged v1 payloads and Agent Console needs no feature
    change.
  - Producer latches remain until exact clear or lifecycle cleanup. Consumer
    dismissal is documented/tested separately as fingerprint suppression.
  - `--local-fast` and docs-only gates pass.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
  - retained provider version, normalized same-id pair, polling/SSE revision,
    and consumer acceptance evidence without raw provider ids.

### Task 4.2: Deliver implementation PRs and close the tracker

- **Location**:
  - `sympoies/nils-cli` shared/Codex/Claude/final integration PRs
  - L2 tracking issue and execution-state dashboard
- **Description**: Deliver the shared baseline, Codex lane, Claude exact-or-
  limited lane, and final integration updates as independently reviewable PRs.
  Run specialist review and provider checks, checkpoint after each merge, and
  close only after a completion audit proves every task and validation.
- **Dependencies**:
  - Task 4.1
- **Complexity**: 5
- **Acceptance criteria**:
  - Required checks, review threads/tasks, and tracker audit are green.
  - All implementation PRs are merged to `main`.
  - Execution state is terminal and provider limitations remain accurately
    recorded.
- **Validation**:
  - `forge-cli pr deliver --no-merge`, specialist review, merge, and
    `plan-issue tracking audit --require-complete`.

## Testing Strategy

- Shared reducer: exact clear, lifecycle cleanup, progress-never-clears, and
  presentation-dismissal boundary.
- Codex: typed ids, projection, exact mapping, deduplication, authority modes,
  schema drift, queue/size loss, concurrency, replay, restart, and reconnect.
- Claude: capability matrix, branch-selected red/green evidence, setup parity,
  privacy, exact/limited normalization, and selective rollback.
- Integration: app-server proxy, local hook ingest, persistence, list/glance,
  SSE, doctor, and capability reporting.
- Live/manual: managed Codex exact clear and exact-or-limited Claude outcome;
  exact canaries prove same-id clear before `Stop`.

## Risks & gotchas

- Claude documents `elicitation_id` as optional. The conservative branch is a
  valid delivery result, not a reason to block Codex.
- Codex hook and protocol events have no shared key. Runtime authority selection
  replaces the impossible reconciliation ledger.
- Exact Codex ids must survive semantic deduplication and preserve JSON-RPC
  string/int64 type before hashing.
- Provider schemas can drift above a baseline version floor. Exact capability
  uses separately audited evidence and degrades outside it.
- Request-observation loss can create a false negative, while resolution loss
  can create a stuck latch. The two failure directions require different tests
  and conservative outcomes.
- Raw payloads contain sensitive content. Retain only synthetic fixtures or
  sanitized metadata projections.
- Any discovered public breaking need triggers re-triage before code changes.

## Rollback plan

- Codex: disable exact-attention admission for new/resumed runtimes and select
  conservative hook authority there. Existing runtimes never switch authority
  mid-flight; unhealthy protocol-authoritative runtimes expose degraded/unknown
  state until replaced.
- Claude: disable Elicitation normalization first so installed callbacks are
  harmless fail-open no-ops. Do not use global `activity setup --remove`. If
  physical removal is necessary, ship a targeted forward cleanup preserving all
  prior agent-session-managed Claude hooks, then revert the binary.
- Retain the v1 reducer and conservative generic hook behavior throughout.
