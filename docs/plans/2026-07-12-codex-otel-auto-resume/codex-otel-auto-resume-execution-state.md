# Codex TUI OTel auto-resume Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: complete; tracking issue closed
- Outcome: blocked for the current Codex TUI runtime
- Target scope: prove or reject a content-free OTel trigger for exact Codex TUI
  usage exhaustion.
- Current task: none; the quota-specific classification item failed.
- Next task: none until Codex exposes a structured TUI quota failure field or
  the runtime boundary moves to an app-server-owned client.
- Last updated: 2026-07-12
- Branch: `feat/codex-otel-auto-resume`
- Source document:
  `docs/plans/2026-07-12-codex-otel-auto-resume/codex-otel-auto-resume-discussion-source.md`
- Plan document:
  `docs/plans/2026-07-12-codex-otel-auto-resume/codex-otel-auto-resume-plan.md`
- Tracking issue: <https://github.com/sympoies/nils-cli/issues/1144>
- Branch/commit/PR: sympoies/nils-cli#1147 merged (<https://github.com/sympoies/nils-cli/pull/1147>)

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | done | Build the isolated OTLP capture harness | `codex-otel-auto-resume-spike-evidence.md` | Loopback-only; raw fields discarded at ingestion. |
| 1.2 | done | Capture a non-exhausted installed-TUI turn | baseline projection | Exact resume/thread and turn ids matched. |
| 2.1 | done | Capture the real exhausted turn | exhausted projection | `poies` reached 100%; reset credits remained 2. |
| 2.2 | done | Test concurrent-session attribution | thread-attribution model | Thread attribution passed; causal classification remained unproved. |
| 3.1 | done | Reconcile evidence and decide next tier | blocked acceptance matrix | Blocked; keep Codex unsupported. |

## Session Log

- 2026-07-12: User authorized autonomous continuation until a proven conclusion
  and implementation when feasible.
- 2026-07-12: Previous persisted-turn canary was negative, while reset and
  same-thread continuation were positive. OTel is the remaining TUI-preserving
  signal candidate.
- 2026-07-12: Installed Codex 0.144.1 baseline correlated exact thread and turn
  ids through `session_task.turn`.
- 2026-07-12: Real `poies` exhaustion produced a content-free failed-completion
  observation within the exact turn interval while the authoritative account
  snapshot reported `workspace_member_credits_depleted`.
- 2026-07-12: Two-session negative attribution passed for thread attribution,
  but independent reviews rejected the generic-error-plus-exhausted-account
  predicate as non-causal.
- 2026-07-12: A final ingestion-only structured-code probe found
  `error_message_json=false` with no `error.type` or `error.code`. The L2
  verdict is blocked for the current TUI runtime.

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `agent-docs preflight --intent project-dev` | pass | Required nils-cli docs and local-fast validation contract resolved. | local preflight |
| Plan archive searches | pass | No archived Codex OTel auto-resume plan found. | local query |
| Synthetic projection fixture | not retained | The temporary receiver discarded sensitive fields, but its fixture was not retained in the PR boundary. | acknowledged evidence limit |
| Installed Codex baseline | pass | Exact resume/thread id and provider turn id correlated. | safe OTLP projection |
| Real exhaustion capture | pass | Exact rejected turn plus exact-account exhausted snapshot; no reset used. | `codex-otel-auto-resume-spike-evidence.md` |
| Two-session correlation | limited pass | Matching thread selected; non-matching and account-only unchanged. Exact turn/account claims were not tested by the model. | runtime correlation model |
| Structured quota discriminator | fail | Error value was not JSON and exposed no type/code; only generic error presence remained. | `codex-otel-auto-resume-spike-evidence.md` |
