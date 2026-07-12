# Codex TUI OTel auto-resume spike evidence

## Verdict

Pass. Installed Codex 0.144.1 exposes enough content-free telemetry to bind a
real rejected TUI attempt to one provider thread and turn when it is combined
with an authoritative exhausted snapshot for that session's exact account.
The production adapter must fail closed unless every predicate below is
present.

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
  - reset credits unchanged at `2`.
- The rejected non-interactive control probe exited `1`; the interactive TUI
  displayed the rejection and remained alive long enough to flush telemetry.

The installed 0.144.1 source confirms that the content-free event kind is
emitted alongside an error message for failed completed responses:
<https://github.com/openai/codex/blob/rust-v0.144.1/codex-rs/otel/src/events/session_telemetry.rs>.
The adapter does not persist or parse that message.

## Correlation result

A deterministic two-session model used the exhausted observation and two real
provider thread ids known during the capture:

- matching thread `019f565c-ab86-7d02-b818-3c227c60628a`: armed exactly once;
- non-matching thread `019f5669-d242-7d02-896f-0ee2361e530f`: unchanged; and
- account exhaustion without the matching thread observation: unchanged.

Validation output:

```text
PASS matching=armed non-matching=unchanged account-only=unchanged
```

## Production contract

Codex auto-resume may arm only when all of the following hold:

1. the managed session has trace-safe Codex OTLP logs and traces enabled;
2. a completed `session_task.turn` span supplies exact `thread.id` and
   `turn.id` and the thread equals the session's provider resume id;
3. a `codex.sse_event` with `event.kind=response.completed` and an error
   message present is observed inside that exact turn interval;
4. the event's thread maps to exactly one active agent-session runtime;
5. the account bound to that runtime/turn has a fresh authoritative snapshot
   with `allowed=false`, `limit_reached=true`, and a supported exhausted reached
   type; and
6. no raw error string or content-bearing attribute crosses the receiver
   projection boundary.

Any missing or ambiguous predicate must leave the session unchanged and report
Codex auto-resume unsupported or unavailable for that runtime.

## Acceptance matrix

| Item | Result | Evidence |
| --- | --- | --- |
| Exact provider thread | pass | Baseline and rejection thread equal the captured resume id. |
| Exact provider turn | pass | Rejection has one `session_task.turn` turn id. |
| Content-free machine classification | pass | Event kind plus error-presence boolean; no text parsing or retention. |
| Exact exhausted account | pass | Isolated `poies` runtime and fresh authoritative reached-type snapshot. |
| Second session remains unchanged | pass | Deterministic two-thread correlation assertion. |
| Installed 0.144.1 | pass | Live runtime and matching tagged source. |

## Next tier

Open an L3 dispatch plan for the nils-cli production adapter, tests, release,
agent-console integration validation, and live enablement. The implementation
must include account binding and a loopback-only receiver; enabling Codex by
changing `supported()` alone would be unsafe.
