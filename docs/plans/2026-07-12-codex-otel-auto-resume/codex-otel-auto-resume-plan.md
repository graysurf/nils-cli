# Plan: Codex TUI OTel auto-resume feasibility

## Overview

Prove or reject a content-free Codex TUI usage-exhaustion trigger based on
trace-safe OpenTelemetry and the existing authoritative account usage reader.
The result decides whether nils-cli can safely add a Codex adapter without
replacing the tmux-backed interactive TUI.

## Read First

- Primary source:
  `docs/plans/2026-07-12-codex-otel-auto-resume/codex-otel-auto-resume-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Open questions carried into execution: whether installed Codex 0.144.1 emits
  a request/stream failure under an exact turn span and whether that observation
  can be classified without raw error text.

## Scope

- In scope: isolated loopback OTLP capture, privacy projection, non-exhausted
  baseline, real `poies` exhaustion, concurrent-session negative correlation,
  cleanup, and implementation-readiness verdict.
- Out of scope: production code, nils-cli release, agent-console deploy, reset
  credit consumption, or app-server runtime replacement.

## Assumptions

1. The installed Codex binary honors per-run OTel config without changing the
   global active account.
2. Trace-safe export omits account id/email and prompt content.
3. The user authorization covers exhausting only the remaining `poies`
   five-hour window.
4. Missing, ambiguous, or content-dependent classification fails closed.

## Sprint 1: Establish the trace-safe correlation baseline

**Goal**: Capture one normal TUI turn and prove the receiver can associate
provider thread and turn metadata without content.

### Task 1.1: Build the isolated OTLP capture harness

- **Location**:
  - runtime `agent-out` evidence directory
- **Description**: Run a loopback-only OTLP HTTP receiver that accepts the
  installed Codex JSON or protobuf export, drops non-allowlisted attributes,
  and persists only a content-free projection plus a schema manifest.
- **Dependencies**: none
- **Complexity**: 4
- **Acceptance criteria**:
  - Receiver is loopback-only and uses no external collector.
  - Projection rejects prompt, response, email, account id, auth, tool content,
    and raw error strings.
  - Raw request bodies are not retained after projection.
- **Validation**: receiver tests with synthetic safe and sensitive OTLP fixtures.

### Task 1.2: Capture a non-exhausted installed-TUI turn

- **Location**:
  - isolated `poies` Codex runtime
  - runtime `agent-out` evidence directory
- **Description**: Launch Codex 0.144.1 with trace-safe OTel export and submit
  one bounded no-tool prompt. Compare the trace thread id with the provider
  resume id captured by session metadata.
- **Dependencies**: Task 1.1
- **Complexity**: 4
- **Acceptance criteria**:
  - One exact thread id and turn id are retained.
  - Thread id equals the provider session/resume id.
  - No content-bearing field survives projection.
- **Validation**: metadata equality, allowlist scan, receiver shutdown flush.

## Sprint 2: Prove or reject the exhaustion trigger

**Goal**: Reach real exhaustion and decide whether the exact failed turn can arm
without text parsing or cross-session ambiguity.

### Task 2.1: Capture the real exhausted turn

- **Location**:
  - isolated `poies` TUI
  - runtime `agent-out` evidence directory
- **Description**: Use bounded synthetic turns until the five-hour window is
  exhausted, then submit one fresh attempt and correlate its trace with the
  authoritative account snapshot.
- **Dependencies**: Task 1.2
- **Complexity**: 6
- **Acceptance criteria**:
  - Exactly one provider thread and turn receive the failure observation.
  - The failure is machine-classifiable without raw `error.message`.
  - The same account snapshot reports an exhausted reached type and reset epoch.
- **Validation**: allowlisted OTLP projection plus safe rate-limit projection.

### Task 2.2: Test concurrent-session attribution

- **Location**:
  - runtime correlation model
- **Description**: Feed the observed exhausted event into a two-session model
  with the same account snapshot and prove only the matching thread/turn arms.
- **Dependencies**: Task 2.1
- **Complexity**: 3
- **Acceptance criteria**:
  - Matching session arms exactly once.
  - Non-matching session remains unchanged.
  - Account exhaustion alone cannot arm either session.
- **Validation**: deterministic correlation fixtures and assertions.

## Sprint 3: Close the feasibility decision

**Goal**: Clean runtime state and produce an actionable implementation or
upstream-blocked verdict.

### Task 3.1: Reconcile evidence and decide next tier

- **Location**:
  - this plan bundle
  - tracking issue
- **Description**: Remove raw OTLP/runtime state, retain only projections,
  record every acceptance item, and either open an L3 implementation plan or
  state the exact missing provider field.
- **Dependencies**:
  - Task 2.2
- **Complexity**: 3
- **Acceptance criteria**:
  - Sensitive runtime is removed and global auth remains unchanged.
  - The implementation verdict is explicit and evidence-backed.
  - A passing result names the nils-cli adapter contract; a failing result names
    the upstream or runtime boundary that blocks it.
- **Validation**: privacy scan, plan validation, issue checkpoint, and reviewer
  verification.

## Testing Strategy

- Test the receiver with synthetic OTLP payloads before a live turn.
- Retain only exact identifiers, event kinds, status/code fields, timestamps,
  and boolean correlation outcomes.
- Treat raw error strings as sensitive and non-contractual.
- Require a negative two-session attribution case.

## Risks & gotchas

- Installed Codex may support logs but not traces or may omit turn-span context.
- HTTP 429 can be ambiguous without a structured error code and exact exhausted
  account match.
- WebSocket transport may report failure in a different event category than
  HTTP/SSE transport.
- Export shutdown may lag; use bounded flush and never infer absence too early.

## Rollback plan

- Stop only the isolated TUI and loopback receiver.
- Remove only the dedicated isolated runtime and raw OTLP buffers.
- Do not modify global Codex auth, live agent-session records, services, or
  reset credits.
