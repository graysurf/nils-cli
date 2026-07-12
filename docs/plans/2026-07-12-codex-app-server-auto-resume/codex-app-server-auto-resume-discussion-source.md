# Codex app-server auto-resume implementation source

## Decision

Implement Codex usage-reset auto-resume by moving agent-session-managed Codex
sessions onto an app-server-backed runtime while preserving the tmux-hosted
terminal UI through `codex --remote` and a private Unix socket. The
authoritative trigger must be the exact app-server turn finishing failed after
the same turn reports structured `UsageLimitExceeded`. Human error text, regex,
local LLMs, and remote LLM classifiers remain non-authoritative and out of the
control path.

## User outcome

The user asked to execute nils-cli issue #1151 completely, pass every feature
acceptance gate, and leave durable evidence. If an external provider property
prevents full acceptance, the execution must retain the exact failed
requirement and the next bounded attempt rather than claiming partial success.

## Known facts

1. Agent-session already owns a durable `agent-session.auto-resume.v1` state
   machine for Claude, including opt-in, reset scheduling, restart recovery,
   cancellation serialization, pre-submit claim, bounded retry, and
   no-duplicate submission [F1].
2. Standalone Codex 0.144.1 TUI quota rejection was proven not to preserve a
   structured usage failure in hooks, OTel, or separately read app-server turn
   history. Production support therefore remains fail-closed on that runtime
   [A1] [A2].
3. The same real-provider canary proved authoritative rate-limit reset epochs,
   idempotent earned reset consumption, and same-thread continuation after a
   reset [A2].
4. Official Codex app-server documents structured failed turns including
   `UsageLimitExceeded`, persistent `thread/resume`, `turn/start`, account rate
   limits, reset credits, and a remote TUI mode [W1].
5. The installed Codex 0.144.1 binary exposes both
   `app-server --listen unix://PATH` and `--remote unix://PATH` [A3].

## Required contract delta

- One local agent-session runtime binds one exact app-server thread and rejects
  wrong-thread, wrong-turn, stale-runtime, incomplete, malformed, and duplicate
  lifecycle sequences.
- Only a structured `UsageLimitExceeded` classification followed by the exact
  matching final failed status emits authoritative
  `turn_failed / usage_exhausted`.
- Rate-limit reads and continuation submission use the same app-server/account
  and thread binding as the failed turn.
- New app-server-backed Codex sessions may report auto-resume supported only
  after capability probing. Standalone TUI and unsupported versions remain
  unsupported.
- The fixed continuation is submitted exactly once to the same thread after
  authoritative reset recheck. Unknown submission outcome never retries.
- Only allowlisted status, timestamps, structured classification, capability
  state, and runtime-scoped opaque identifier projections may persist or be
  exposed.
- Browser and native clients keep the existing daemon-owned v1 contract and do
  not gain timers or prompt-submission responsibility.

## Safety and rollout

- Prefer a private Unix socket; do not expose the experimental TCP WebSocket
  transport.
- Do not place tokens or credentials in argv, logs, fixtures, plan evidence, or
  API projections.
- Start behind an explicit runtime/capability gate and keep the raw TUI path
  compatible.
- Do not automatically consume earned reset credits. A real canary may consume
  one only under the user's explicit authorization already recorded for this
  work.
- Do not enable support from synthetic tests alone. Final acceptance requires a
  real installed-Codex quota failure with content-free exact-thread/turn
  evidence and a same-account sibling negative control.

## Evidence sources

- [U1] User instruction in this conversation to complete issue #1151 and leave
  durable success or next-attempt evidence.
- [F1] `crates/agent-session/src/auto_resume.rs`, `activity.rs`, `serve.rs`, and
  `lib.rs` on `origin/main`.
- [A1] <https://github.com/sympoies/nils-cli/issues/1144> and merged PRs #1147
  and #1149.
- [A2] <https://github.com/sympoies/agent-console/issues/276> and merged PR
  #277.
- [A3] Installed `codex-cli 0.144.1` command help observed on 2026-07-12.
- [W1] <https://learn.chatgpt.com/docs/app-server>.
- [I1] Issue #1151 concludes that app-server is the only current structured,
  TUI-preserving production path.

## Execution

- Tracking issue: <https://github.com/sympoies/nils-cli/issues/1151>
- Recommended plan:
  `docs/plans/2026-07-12-codex-app-server-auto-resume/codex-app-server-auto-resume-plan.md`
- Recommended execution state:
  `docs/plans/2026-07-12-codex-app-server-auto-resume/codex-app-server-auto-resume-execution-state.md`
