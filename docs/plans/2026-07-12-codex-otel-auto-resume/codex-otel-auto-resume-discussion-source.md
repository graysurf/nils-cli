# Codex TUI OTel auto-resume feasibility source

## Decision

Run one issue-backed L2 spike against the installed Codex TUI. Determine
whether trace-safe OpenTelemetry plus an authoritative account rate-limit
snapshot can identify exactly one usage-exhausted turn without terminal-text or
error-message parsing. If the contract passes, hand the proven adapter shape to
an L3 implementation plan; if it fails, keep Codex auto-resume unsupported and
record the missing upstream field precisely.

## User outcome

The user asked the agent to continue autonomously until there is a conclusion,
to explain why Codex cannot support the feature if it remains impossible, and
to complete the implementation when the evidence proves it can be safe. The
prior canary authorization permits exhausting the remaining `poies` five-hour
window. This spike does not need another reset credit because reset acceptance
and same-thread continuation were already observed.

## Known facts

1. The previous real TUI canary reached account exhaustion, but a separate
   app-server read persisted both rejected TUI turns as `completed` with no
   error. Account reset and same-thread continuation succeeded afterward [A1].
2. `agent-session` owns durable auto-resume scheduling and currently supports
   only Claude because Codex has no proven authoritative interactive
   usage-exhaustion signal [F1].
3. Codex app-server documents `UsageLimitExceeded` and a failed terminal turn
   for app-server-owned turns [W1]. That does not prove that a separate process
   can recover the same classification from an interactive TUI rollout.
4. Codex can export trace-safe OTel events. Request events include status and
   error details, while turn spans carry `thread.id` and `turn.id` [W2] [W3].
5. The current OTel request event source exposes `http.response.status_code`
   and `error.message`, but no dedicated `UsageLimitExceeded` field [W3]. A
   spike is therefore required before treating OTel plus an account snapshot as
   an exact content-free correlation contract.

## Safety contract

- Use only `poies`; never consume `gamania` or `sym` quota.
- Bind the test OTLP receiver to loopback and retain only allowlisted metadata.
- Keep prompt logging disabled. Do not retain account id, email, prompt,
  response, transcript, tool content, raw error message, auth payload, or token.
- A passing signal must map one agent-session session to one provider thread
  and one provider turn.
- Require both a turn-scoped request failure and an authoritative exhausted
  snapshot for the same account. A bare HTTP 429, a bare account snapshot, or
  terminal text is insufficient.
- Exercise two simultaneously known Codex sessions in the correlation model;
  only the session that received the failed turn may arm.
- Do not consume a reset credit during this spike unless a later implementation
  acceptance explicitly requires it. The reset/same-thread mechanics are
  already known.

## Acceptance

The spike passes only if a content-free retained projection proves all of:

1. the provider thread id equals the session's captured Codex resume id;
2. the failure is nested under an exact provider turn id;
3. the request/stream observation is machine-classifiable without persisting or
   parsing human error text;
4. the exact session account is authoritatively exhausted at that observation;
5. a second session sharing the host is not armed; and
6. the same projection shape is available on installed Codex 0.144.1, not only
   in upstream `main` source.

If any item fails, the implementation verdict is blocked for the current TUI
runtime. The sustainable unblocks are an upstream structured TUI failure event
or replacing the TUI runtime with an app-server-owned client.

## Scope

- In scope: loopback OTLP receiver, trace/log allowlist projection, installed
  TUI non-exhausted baseline, real `poies` exhaustion, exact feasibility
  verdict, and privacy cleanup.
- Conditional next scope: a new L3 plan for nils-cli implementation, release,
  agent-console compatibility, and live deployment only after this contract
  passes.
- Out of scope for this spike: enabling `supported=true`, heuristic text
  parsing, changing the live daemon, deploying a collector, or consuming a
  second reset credit.

## Sources

- [U1] User instruction in this conversation to continue through a proven
  conclusion and implement the feature if it is feasible.
- [A1] <https://github.com/sympoies/agent-console/issues/276> and
  <https://github.com/sympoies/agent-console/pull/277>.
- [F1] `sympoies/nils-cli` `crates/agent-session/src/auto_resume.rs` on `main`,
  especially the provider `supported` guard.
- [W1] <https://learn.chatgpt.com/docs/app-server#errors>.
- [W2] <https://learn.chatgpt.com/docs/agent-approvals-security#event-categories>.
- [W3] <https://github.com/openai/codex/blob/main/codex-rs/otel/src/events/session_telemetry.rs>
  and
  <https://github.com/openai/codex/blob/main/codex-rs/core/src/tasks/mod.rs>.

## Execution

- Recommended plan:
  `docs/plans/2026-07-12-codex-otel-auto-resume/codex-otel-auto-resume-plan.md`
- Recommended execution state:
  `docs/plans/2026-07-12-codex-otel-auto-resume/codex-otel-auto-resume-execution-state.md`
