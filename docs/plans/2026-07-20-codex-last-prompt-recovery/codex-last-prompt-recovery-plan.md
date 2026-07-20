# Plan: Recover Codex Last Prompt for Long Transcripts

## Overview

Replace the fixed 256 KiB best-effort lookup used by the session list with a bounded cold recovery plus incremental in-memory tracking for provider transcripts. The Agent Console API and UI contract stay unchanged: only sessions with an exact provider transcript identity may expose a response-only `last_prompt`, and prompt text is never persisted by the daemon. The delivery includes a reviewed `nils-cli` release, Agent Console daemon restart, and aggregate live verification.

## Read First

- Primary source: `docs/plans/2026-07-20-codex-last-prompt-recovery/codex-last-prompt-recovery-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Open questions carried into execution: none

## Scope

- In scope: reproduce the long-Codex-turn miss, add bounded cold recovery and incremental prompt tracking to `agent-session`, document the runtime contract, validate and review the change, merge the PR, release the resulting `nils-cli`, restart the governed Agent Console daemon, and verify aggregate provider coverage without exposing prompt or session content.
- Out of scope: Agent Console UI changes, durable prompt persistence, heuristic transcript guessing when provider resume identity is absent, unbounded transcript reads, and displaying or retaining live prompt text in evidence.

## Assumptions

1. A 64 MiB cold-recovery ceiling is sufficient for the observed long-turn gap while remaining bounded; live exact-identity Codex gaps observed during diagnosis were approximately 0.49 MiB to 17.6 MiB.
2. Opening the incremental tail at EOF before cold recovery prevents prompts appended during recovery from being lost.
3. Sessions without an exact provider resume identity must continue to omit `last_prompt`; fixing identity capture is a separate concern.
4. The release version and exact deployment command require the repository-owned two-stage consent flow before execution.

## Sprint 1: Runtime Recovery

**Goal**: Make resolvable Codex sessions retain their latest prompt across long transcript growth without repeated large reads.
**Demo/Validation**:

- Commands: `cargo test -p nils-agent-session last_prompt -- --nocapture`
- Verify: a prompt more than 256 KiB behind EOF is recovered once, later appended prompts are tracked incrementally, and replacement/truncation cannot leave stale prompt state.

### Task 1.1: Capture the regression with a meaningful red test

- **Location**:
  - `crates/agent-session/src/provider_prompt.rs`
- **Description**: Add a focused Codex transcript fixture whose latest user prompt falls outside the legacy 256 KiB tail window and record the failing assertion before production edits.
- **Dependencies**:
  - none
- **Complexity**: 3
- **Acceptance criteria**:
  - The focused test fails against the current implementation for the expected missing-prompt reason.
  - Test-first evidence records the exact command, failing test, expected result, and observed result.
- **Validation**:
  - `cargo test -p nils-agent-session last_prompt_recovery_window_covers_long_codex_turn -- --nocapture`

### Task 1.2: Add bounded recovery and incremental tracking

- **Location**:
  - `crates/agent-session/src/provider_prompt.rs`
  - `crates/agent-session/src/serve.rs`
- **Description**: Cache one provider prompt tracker per exact discovery key. Initialize its append tail at EOF, cold-read at most 64 MiB for the newest prompt, then consume only appended records on later polls. Reset safely when the transcript identity, size, or file identity changes.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 7
- **Acceptance criteria**:
  - Long Codex turns no longer lose a resolvable latest prompt after the legacy window is exceeded.
  - Subsequent prompts are detected incrementally without rereading the cold-recovery window.
  - Rotation, truncation, and discovery invalidation cannot expose stale prompt text.
  - Prompt data remains memory-only and response-only.
  - Identity-less sessions remain omitted rather than guessed.
- **Validation**:
  - `cargo test -p nils-agent-session last_prompt -- --nocapture`

### Task 1.3: Document the bounded runtime contract

- **Location**:
  - `crates/agent-session/docs/specs/serve-api-v1.md`
  - `crates/agent-session/docs/runbooks/serve-daemon.md`
- **Description**: Replace the fixed-tail limitation with the bounded cold-recovery, incremental tracking, reset, and exact-identity semantics.
- **Dependencies**:
  - Task 1.2
- **Complexity**: 2
- **Acceptance criteria**:
  - API and daemon runbooks agree with the implementation and preserve privacy constraints.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`

## Sprint 2: Delivery and Live Acceptance

**Goal**: Independently review, merge, release, deploy, and validate the fix through the governed workflows.
**Demo/Validation**:

- Commands: `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`; repository-owned release wrapper; `systemctl --user restart agent-console-serve.service`; `scripts/smoke-agent-console.sh`
- Verify: PR review is approved, the release is installed, the daemon uses the new binary, all smoke stages pass, and aggregate live data shows `last_prompt` for resolvable Codex sessions without printing prompt text or session identifiers.

### Task 2.1: Validate, independently review, and merge

- **Location**:
  - repository-wide change set
- **Description**: Run the declared finish-line checks, deliver the PR without merge, obtain testing and maintainability specialist reviews, repair any valid findings, post a combined outcome, and merge through the L2 workflow.
- **Dependencies**:
  - Task 1.1
  - Task 1.2
  - Task 1.3
- **Complexity**: 5
- **Acceptance criteria**:
  - `test-first-evidence verify` and docs-impact verification pass.
  - Required validation passes.
  - Specialist review outcomes are posted and the combined result is `APPROVE`.
  - The PR is merged through the governed provider workflow.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`

### Task 2.2: Release, deploy, and verify live behavior

- **Location**:
  - `serenvia/sympoies-infra` governed release and Agent Console runtime helpers
- **Description**: Present the exact release command through the mandatory consent preview, execute only after explicit stage-two authorization, restart the Agent Console user service, run the full smoke script, and inspect privacy-safe aggregate coverage.
- **Dependencies**:
  - Task 2.1
- **Complexity**: 5
- **Acceptance criteria**:
  - The user explicitly authorizes the exact stable version and mode after preview.
  - The canonical release/deploy helper succeeds once.
  - `KillMode=process` is verified before restart.
  - The full Agent Console smoke script passes after restart.
  - Resolvable Codex sessions expose `last_prompt`; identity-less sessions remain safely omitted.
- **Validation**:
  - Canonical release helper output
  - `systemctl --user show agent-console-serve.service --property=KillMode --property=ActiveState --property=SubState --property=Result`
  - `/home/terry/Project/serenvia/sympoies-infra/scripts/smoke-agent-console.sh`
  - Aggregate authenticated `/sessions` capability/provider coverage check with content redacted

## Testing Strategy

- Unit: synthetic Codex JSONL recovery, append tracking, truncation/replacement reset, malformed records, and bounded-window behavior.
- Integration: existing `agent-session` session-list and provider-discovery tests plus the repository `--local-fast` gate.
- E2E/manual: governed release and daemon restart, complete smoke script, then aggregate-only `/sessions` validation.

## Risks & gotchas

- A cold scan must remain bounded so a very large transcript cannot create unbounded latency or memory use.
- A prompt appended between recovery and tail initialization would be missed; initialize the tail before recovery.
- Cached prompt state must be bound to the exact discovery key and cleared on source invalidation, replacement, or truncation.
- Never log, persist, or include live prompt text, session IDs, transcript paths, or resume IDs in issue/PR evidence.
- The release cannot be executed from the initial user request alone; the canonical two-stage consent flow requires a later explicit authorization of the displayed version and mode.

## Rollback plan

- Revert the merged runtime commit and publish the next governed stable patch release.
- Restart `agent-console-serve.service` through the canonical runtime procedure and rerun the complete smoke script.
- The API remains backward compatible throughout; older daemons simply resume omitting long-distance prompts.
