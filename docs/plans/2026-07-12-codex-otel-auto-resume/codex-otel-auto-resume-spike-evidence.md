# Codex TUI OTel auto-resume spike evidence

## Verdict

Blocked for the current Codex TUI runtime. Installed Codex 0.144.1 exposes
enough content-free telemetry to bind a rejected attempt to one provider thread
and turn, but it does not expose a quota-specific structured failure field.
Combining a generic turn error with a simultaneously exhausted account cannot
prove that quota caused the turn to fail. Codex auto-resume must therefore stay
unsupported.

## Retained projection

The loopback receiver accepted OTLP JSON and retained only allowlisted ids,
event names/kinds, timestamps, transport status, and the boolean presence of an
error message. Prompt logging was disabled. Raw prompt, response, error text,
account id, email, token, and auth payload were never retained.

### Non-exhausted baseline

- Installed runtime: Codex `0.144.1`.
- Provider resume/thread id:
  `019f565c-ab86-7d02-b818-3c227c60628a`.
- Provider turn id: `019f565c-ac83-7183-a526-a46524cfc80b`.
- Turn trace id: `88e733ba821d7e760731cc547123bd57`.
- The `session_task.turn` span carried the exact thread and turn ids.
- The same trace carried `codex.websocket_request` with `success=true`.

### Exhausted TUI attempt

- Provider thread id: `019f565c-ab86-7d02-b818-3c227c60628a`.
- Provider turn id: `019f566d-c203-7031-9942-1d91cf38e8b3`.
- Turn trace id: `6b02be7f9eeae14a4acf53402f04b1af`.
- Turn interval (Unix nanoseconds):
  `1783861461510207516..1783861462584361552`.
- Within that interval the log projection contained:
  - `event.name=codex.sse_event`;
  - `event.kind=response.completed`;
  - `error_message_present=true`; and
  - `observed_time_unix_nano=1783861462579574131`.
- The transport request itself still reported `success=true`; it is not a
  valid exhaustion trigger.
- The exact isolated account snapshot at rejection reported:
  - `allowed=false`;
  - `limit_reached=true`;
  - `rate_limit_reached_type=workspace_member_credits_depleted`;
  - five-hour usage `100%`; and
  - reset epoch `1783875448`; and
  - reset credits unchanged at `2`.
- The rejected non-interactive control probe exited `1`; the interactive TUI
  displayed the rejection and remained alive long enough to flush telemetry.

The installed 0.144.1 source confirms that this event shape is emitted for a
generic failed completed response, not specifically for usage exhaustion:
<https://github.com/openai/codex/blob/rust-v0.144.1/codex-rs/otel/src/events/session_telemetry.rs>.
The adapter does not persist or parse that message.

### Structured-code control probe

A second exhausted TUI attempt used an ingestion-only classifier that attempted
to parse the complete `error.message` value as JSON and retained only JSON
success plus allowlisted `error.type` and `error.code` values. It retained:

- exact thread `019f567f-ad4b-7201-8fa2-431c160d32ba`;
- exact turn `019f567f-c780-7c82-a0b2-dfd41129f052`;
- `event.name=codex.sse_event`;
- `event.kind=response.completed`;
- `error_message_present=true`;
- `error_message_json=false`;
- `error.type=null`; and
- `error.code=null`.

The raw message was discarded at ingestion. This proves that installed Codex
does not carry a quota discriminator inside a structured JSON error value that
nils-cli could safely project.

## Correlation result

A deterministic two-session model used the exhausted observation and two real
provider thread ids known during the capture. Its proven scope is thread
attribution only:

- matching thread `019f565c-ab86-7d02-b818-3c227c60628a`: armed exactly once;
- non-matching thread `019f5669-d242-7d02-896f-0ee2361e530f`: unchanged; and
- account exhaustion without the matching thread observation: unchanged.

Validation output:

```text
PASS matching=armed non-matching=unchanged account-only=unchanged
```

The model does not prove quota causality, exact-turn membership, account
binding, freshness, or ambiguity handling. Those claims are intentionally not
carried into the verdict.

## Missing production contract

The observed content-free predicates are insufficient:

1. `session_task.turn` supplies exact thread and turn ids.
2. `codex.sse_event`, `event.kind=response.completed`, and an error present
   classify only a generic provider error.
3. The exact isolated account is authoritatively exhausted and supplies a reset
   epoch.
4. Nothing content-free connects that account state causally to the generic
   turn error.

A safe adapter needs a stable provider field equivalent to app-server
`UsageLimitExceeded`, such as an OTel `error.type` or `error.code`. Parsing the
human error message, terminal text, or localized wording is explicitly
rejected.

## Acceptance matrix

| Item | Result | Evidence |
| --- | --- | --- |
| Exact provider thread | pass | Baseline and rejection thread equal the captured resume id. |
| Exact provider turn | pass | Rejection has one `session_task.turn` turn id. |
| Content-free quota classification | fail | Only generic error presence; structured type/code are absent. |
| Exact exhausted account | pass | Isolated `poies` runtime, reached type, and reset epoch. |
| Second session remains unchanged | limited pass | Thread-only attribution model; no quota-causality claim. |
| Installed 0.144.1 | pass | Live runtime and matching tagged source. |

## Next tier

Do not open an L3 implementation plan and do not change Codex `supported()`.
Sustainable unblocks are:

1. Codex emits a stable quota-specific structured field for interactive TUI
   failures; or
2. agent-session replaces the external TUI ownership boundary with an
   app-server-owned client whose documented failed turn carries
   `UsageLimitExceeded`.

Changing nils-cli alone cannot safely manufacture the missing causal field.
