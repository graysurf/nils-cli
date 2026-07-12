# Codex app-server auto-resume spike evidence

## Scope

This spike audited Codex CLI 0.144.1 and the app-server v2 protocol before the
production implementation. Evidence retains only protocol names, enums,
percentages, reset epochs, boolean outcomes, and one-way identifier digests.
Account identifiers, auth material, prompts, assistant output, human error
messages, raw thread/turn ids, and terminal content are excluded.

## Generated protocol contract

The CLI-generated JSON Schema and TypeScript bindings establish these exact
wire values:

- `Turn.status`: `completed`, `interrupted`, `failed`, or `inProgress`;
- `Turn.error.codexErrorInfo`: includes exact `usageLimitExceeded`;
- `error` notification: `error`, `willRetry`, `threadId`, and `turnId`;
- control requests: `thread/loaded/list`, `thread/resume`,
  `account/rateLimits/read`, and `turn/start`;
- Unix WebSocket transport is advertised by `codex app-server --listen`.

## Real exhausted-account capture

An isolated app-server connected to an account whose five-hour window was
already exhausted. One rejected turn produced this sanitized live projection:

```json
{
  "error_event": {
    "codex_error_info": "usageLimitExceeded",
    "will_retry": false
  },
  "turn_completed": {
    "status": "failed",
    "codex_error_info": "usageLimitExceeded"
  },
  "rate_limit": {
    "primary_used_percent": 100,
    "secondary_used_percent": 17,
    "primary_reset_epoch_present": true,
    "reset_credits_available": 2
  },
  "thread_digest_present": true,
  "turn_digest_present": true
}
```

No reset credit was consumed during the spike capture.

## Remote TUI topology

The app-server listened on a private short Unix socket. A real Codex TUI
connected through `--remote unix://...`, and a second control client initialized,
listed exactly one loaded thread, resumed that thread, and started a turn. The
TUI rendered the control client's prompt and the quota failure, proving that a
separate metadata-only control connection can observe and submit on the same
thread without a transparent protocol bridge.

The live sequence was:

1. `turn/started`;
2. metadata-only item lifecycle notifications;
3. `account/rateLimits/updated`;
4. `error` with exact `usageLimitExceeded` and `willRetry: false`;
5. `turn/completed` with matching turn id and `status: failed`.

## Persistence divergence

A later `thread/read` with `includeTurns: true` represented the same failed
turn as completed and omitted the structured error. The implementation must
therefore consume the continuously live protocol. Persisted Codex rollout
history is not authoritative for reconstructing an outage that occurred while
the control monitor was disconnected.

## Implementation consequences

- Fresh capability-probed Codex sessions may use app-server v2; raw TUI,
  imported sessions, and legacy resumes remain fail-closed.
- Failure reduction requires one exact bound thread/turn and terminal failed
  structured quota evidence. Human text classification is forbidden.
- Rate-limit reads and continuation submission use the same live connection.
- Submission is successful only after `turn/start` acknowledges a turn id.
- Any crash, timeout, or disconnect after the durable submission claim is an
  unknown outcome and is never retried automatically.
