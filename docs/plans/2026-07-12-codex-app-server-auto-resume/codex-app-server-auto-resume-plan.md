# Plan: Codex app-server-backed usage-reset auto-resume

## Overview

Deliver production Codex auto-resume for agent-session-managed sessions by
binding the existing tmux TUI to an app-server-owned thread, ingesting the
official structured usage-limit failure, reusing the durable reset scheduler,
and proving the result with deterministic tests plus a real quota-exhaustion
canary.

## Read First

- Primary source:
  `docs/plans/2026-07-12-codex-app-server-auto-resume/codex-app-server-auto-resume-discussion-source.md`
- Source type: existing issue/spec
- Tracking issue: <https://github.com/sympoies/nils-cli/issues/1151>
- Open questions carried into execution: multi-client event delivery versus a
  transparent bridge; rendering of daemon-initiated `turn/start`; exact
  generated error schema; process restart topology; imported-session rollout;
  and normalized source-kind compatibility.

## Scope

- In scope: installed-Codex protocol spike; private Unix transport; runtime and
  thread ownership; structured lifecycle normalization; provider-scoped rate
  limits; exactly-once continuation; raw-TUI fallback; deterministic tests;
  privacy validation; real exhaustion and sibling-session negative control;
  docs; PR delivery; independent review; merge; tracker closeout.
- Out of scope: human-text or LLM classification; public/non-loopback app-server
  listeners; client-side scheduling; automatic reset-credit spending; support
  claims for raw standalone TUI sessions.

## Assumptions

1. Installed Codex app-server behavior matches its version-generated schema for
   the retained fields.
2. A private Unix endpoint can preserve the existing tmux/xterm terminal UX.
3. Missing exact identity, classification, finality, or acknowledgement fails
   closed.
4. The existing auto-resume v1 client projection is sufficient unless tests
   prove a new client-visible state is required.
5. The initial real quota acceptance may use the authorized `poies` account.
   The final-topology rerun and any future quota/reset validation use only the
   authorized `sym` account, never `gamania`, with each reset credit recorded.

## Sprint 1: Prove the app-server runtime boundary

**Goal**: Select a topology that lets agent-session observe and control the
exact remote-TUI thread without terminal-text inference.

### Task 1.1: Generate schemas and prove remote-TUI thread ownership

- **Location**:
  - `crates/agent-session`
  - runtime `agent-out` evidence
- **Description**: Generate version-matched app-server schemas, start one
  isolated private Unix endpoint, connect the installed Codex TUI remotely, and
  determine whether a second control client receives exact thread events or a
  transparent JSON-RPC bridge is required.
- **Dependencies**: none
- **Complexity**: 6
- **Acceptance criteria**:
  - Normal turn binds one exact app-server thread and turn to one runtime.
  - Selected topology observes ordered lifecycle metadata without retaining
    prompts, responses, tool data, error messages, auth, or transcripts.
  - Daemon-initiated `turn/start` visibility in the remote TUI is proven or the
    bridge path has an acknowledged equivalent.
- **Validation**: installed-Codex content-free normal-turn projection, schema
  manifest, privacy scan, and topology decision.

### Task 1.2: Capture authoritative real usage exhaustion

- **Location**:
  - isolated app-server-backed `poies` runtime
  - runtime `agent-out` evidence
- **Description**: Reach a real quota rejection and retain only projected
  thread/turn identity, structured error variant, final status, rate-limit
  reached state, and reset epoch. Include a same-account sibling session.
- **Dependencies**: Task 1.1
- **Complexity**: 8
- **Acceptance criteria**:
  - Exact matching turn reports `UsageLimitExceeded` and final `failed`.
  - Same-runtime account snapshot is authoritative and exhausted.
  - Sibling session is not selected or armed.
  - No human error text participates in classification.
- **Validation**: content-free real-provider projection and deterministic
  two-session correlation assertion.

## Sprint 2: Implement runtime and structured lifecycle

**Goal**: Add a capability-gated app-server-backed Codex session mode while
preserving raw TUI compatibility.

### Task 2.1: Add private app-server runtime supervision

- **Location**:
  - `crates/agent-session/src/lib.rs`
  - `crates/agent-session/src/serve.rs`
  - focused new crate-local modules
- **Description**: Start, probe, bind, reconnect, interrupt, and clean up the
  app-server process/socket and remote TUI. Persist only metadata required for
  runtime generation and thread recovery. Define safe behavior for new,
  resumed, imported, dead-process, stale-socket, and daemon-restart cases.
- **Dependencies**: Task 1.1
- **Complexity**: 9
- **Acceptance criteria**:
  - App-server mode is explicit and capability-gated.
  - Socket/process lifecycle is per-session, private, restart-safe, and cleaned
    on delete.
  - Raw TUI start/resume remains behaviorally compatible and unsupported for
    Codex auto-resume.
- **Validation**: fake-process integration tests and installed-Codex normal-turn
  smoke.

### Task 2.2: Normalize app-server turn failures safely

- **Location**:
  - `crates/agent-session/src/activity.rs`
  - provider-protocol adapter module
  - `crates/agent-session/docs/turn-state-contract.md`
- **Description**: Latch the exact turn's structured error, require the matching
  final failed status, project identifiers, and emit one authoritative
  normalized event. Decide and document the source-kind compatibility change.
- **Dependencies**: Task 2.1
- **Complexity**: 7
- **Acceptance criteria**:
  - Only exact `UsageLimitExceeded + failed` maps to `usage_exhausted`.
  - Wrong, stale, partial, malformed, reordered, duplicated, text-only, and
    non-quota cases cannot arm.
  - Raw provider content is discarded before durable state.
- **Validation**: table-driven unit tests, replay/reorder tests, and privacy
  fixtures.

### Task 2.3: Integrate Codex usage scheduling and continuation

- **Location**:
  - `crates/agent-session/src/auto_resume.rs`
  - `crates/agent-session/src/serve.rs`
  - app-server control adapter
- **Description**: Replace the single global Claude usage collection path with
  provider/runtime-scoped reads. Query the failed session's app-server account,
  preserve reset scheduling/recheck semantics, and submit the fixed
  continuation using acknowledged `turn/start` on the bound thread.
- **Dependencies**: Task 2.2
- **Complexity**: 8
- **Acceptance criteria**:
  - Supported app-server Codex session can opt in and arm from its exact failed
    turn.
  - Latest exhausted reset controls wake, and the matching account is rechecked
    before submission.
  - Same-thread continuation is claimed and acknowledged exactly once.
  - Unknown outcome, manual input, cancellation, state revision change,
    restart, and duplicate tick remain fail-closed/no-duplicate.
- **Validation**: deterministic fake app-server integration suite and scheduler
  regression suite.

## Sprint 3: Complete acceptance and documentation

**Goal**: Prove every issue acceptance item locally before provider delivery.

### Task 3.1: Complete regression, privacy, and compatibility coverage

- **Location**:
  - `crates/agent-session/tests`
  - `crates/agent-session/README.md`
  - crate-local contract/runbook docs
- **Description**: Add the full fake JSON-RPC matrix, multi-session isolation,
  daemon/app-server restart, cancellation, manual input, unknown submission,
  capability degradation, imported-session decision, and no-content retention
  tests. Document version floor, mode selection, lifecycle, privacy, and
  troubleshooting.
- **Dependencies**: Task 2.3
- **Complexity**: 7
- **Acceptance criteria**:
  - Every test-first row from issue #1151 has an executable assertion.
  - Browser/native `agent-session.auto-resume.v1` compatibility is unchanged.
  - Documentation names every supported and degraded mode precisely.
- **Validation**: focused package tests, docs audits, and contract searches.

### Task 3.2: Run repository validation

- **Location**: nils-cli workspace
- **Description**: Run formatting, clippy, package/workspace tests selected by
  local-fast, docs/parity audits, and any dedicated app-server test harness.
- **Dependencies**: Task 3.1
- **Complexity**: 4
- **Acceptance criteria**:
  - Local-fast validation passes with complete test-first evidence.
  - No unrelated dirty work enters the branch.
- **Validation**: `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
  plus focused test commands recorded in the execution state.

## Sprint 4: Real canary, delivery, and closeout

**Goal**: Prove real-provider behavior, pass independent review and CI, merge,
and close the durable tracker with replayable evidence.

### Task 4.1: Run the end-to-end real exhaustion acceptance canary

- **Location**:
  - released/installed candidate runtime
  - metadata-only `agent-out` evidence
- **Description**: Run app-server-backed TUI and same-account sibling sessions,
  exhaust the authorized window, observe exact structured failure, verify one
  session arms, reopen through natural reset or one authorized idempotent reset,
  and prove exactly one visible same-thread continuation.
- **Dependencies**: Task 3.2
- **Complexity**: 9
- **Acceptance criteria**:
  - All real-provider acceptance rows from issue #1151 pass.
  - Content-free evidence retains schema/version, projected identities,
    statuses, reset state, exact-once result, and negative-control result.
  - Sensitive runtime state is removed after evidence projection.
- **Validation**: acceptance matrix, privacy scan, and cleanup verification.

### Task 4.2: Deliver, review, merge, and close

- **Location**:
  - feature PR
  - tracking issue #1151
  - plan archive handoff
- **Description**: Commit through semantic-commit, deliver without merge, pass
  required CI and independent specialist review, resolve all findings/threads/
  tasks, checkpoint review evidence, merge, pass strict close-ready, close and
  audit the issue, then perform archive discovery and dry-run migration.
- **Dependencies**: Task 4.1
- **Complexity**: 6
- **Acceptance criteria**:
  - Required GitHub `test`, `test_macos`, and `coverage` checks pass.
  - Testing, maintainability, and selected risk reviewers approve.
  - `tracking close-ready --expect-visible` returns ready with no blockers.
  - Closed issue read-back contains source, plan, state, validation, review,
    real-canary, merge, and closeout evidence.
- **Validation**: provider PR/issue read-back and plan-record audit.

## Testing Strategy

- Capture failing deterministic tests before production code for structured
  quota mapping, cross-session isolation, unsupported capability, provider-
  scoped usage, same-thread continuation, unknown outcome, duplicate/reordered
  events, restart/cancellation, and privacy non-retention.
- Use a bounded fake JSON-RPC app-server for exhaustive edge cases.
- Use installed Codex for topology and real-provider acceptance.
- Keep every human error string outside the authoritative assertion path.

## Risks & gotchas

- App-server event subscriptions may be connection-local, requiring a protocol
  bridge rather than a passive control client.
- Remote TUI may not render turns initiated by a second connection.
- Generated app-server schemas are version-specific; capability probing must
  fail closed across upgrades.
- Process/socket recovery must not bind a different thread or account after a
  daemon restart.
- A real quota window is external mutable state; the canary must record exact
  before/after snapshots and never infer success from terminal rendering.

## Rollback plan

- Keep app-server mode explicit and revert sessions to the existing raw TUI
  path without changing their provider history.
- Disable Codex `auto_resume.supported` if capability, identity, or structured
  failure proof is absent.
- Stop and remove only per-session app-server processes/sockets and metadata;
  do not alter global auth or unrelated tmux sessions.
- Preserve the existing Claude scheduler and v1 client contract.
