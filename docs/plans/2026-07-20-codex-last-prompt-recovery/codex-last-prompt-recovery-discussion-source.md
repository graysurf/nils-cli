# Codex Last-Prompt Recovery — Implementation Source

## Decision

Implement the fix in `nils-cli`'s `agent-session` daemon. Keep the Agent Console
UI and wire contract unchanged. For sessions with an exact provider transcript
identity, recover the latest prompt from a bounded cold window and then track
appended provider events incrementally in memory.

## Evidence

- [U1] The user reported that mobile cards show Claude's last prompt but omit Codex and authorized an L2 repair plus deployment for testing.
- [F1] `packages/ui/src/MobileSessionList.tsx` already renders
  `session.last_prompt?.text`; `packages/shared/src/api.ts` already accepts the
  provider-neutral capability and response field.
- [F2] `crates/agent-session/src/provider_prompt.rs` currently reads only the
  final 256 KiB, while the Codex parser looks for `event_msg` / `user_message`
  records.
- [F3] `crates/agent-session/src/serve.rs` performs that bounded tail lookup
  while enriching each session-list response and treats prompts outside the
  window as a tolerated miss.
- [A1] A privacy-safe live aggregate showed `last_prompt` for all three Claude
  sessions and none of four Codex sessions, despite the capability being
  enabled.
- [A2] Exact-identity structural inspection found one unique transcript for each
  of three Codex sessions and latest-user-prompt distances of approximately
  0.49 MiB, 9.1 MiB, and 17.6 MiB from EOF. No prompt text, session ID, resume
  ID, or transcript path was retained.
- [A3] The fourth Codex session had no provider resume identity, so the daemon correctly refused to guess a transcript.
- [I1] The asymmetry is caused by provider transcript shape and growth, not a
  mobile rendering bug: Codex long turns push the last user event outside
  256 KiB, while Claude commonly supplies a newer prompt-bearing event near EOF.

## Required behavior

1. Recover a resolvable Codex prompt when it is outside the legacy 256 KiB tail but inside a 64 MiB bound.
2. Open the append tail before cold recovery, then consume appended records
   incrementally so prompts written during or after recovery are not lost.
3. Retain only the latest bounded prompt in process memory and expose it only in the authenticated response.
4. Clear or rebuild state when the transcript source changes, truncates, rotates, or becomes invalid.
5. Continue omitting sessions that lack exact provider resume identity.
6. Do not alter the Agent Console API schema or mobile card rendering.

## Deployment boundary

The merged fix must use the canonical `sympoies-infra` nils-cli release workflow.
That workflow requires a stable version and mode to be previewed, followed by a
separate user message explicitly authorizing the exact displayed action. After
installation, the Agent Console user daemon is restarted only after confirming
`KillMode=process`, followed by the full smoke script and privacy-safe aggregate
verification.

## Execution

- Recommended plan: docs/plans/2026-07-20-codex-last-prompt-recovery/codex-last-prompt-recovery-plan.md
- Recommended execution state: docs/plans/2026-07-20-codex-last-prompt-recovery/codex-last-prompt-recovery-execution-state.md
- Status: execute through reviewed merge, then pause at the governed release preview consent boundary.
- Next-task source: Sprint 1, Task 1.1 in the recommended plan.
