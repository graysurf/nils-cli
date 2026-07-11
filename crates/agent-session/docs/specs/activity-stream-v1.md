# Activity stream v1

## Purpose

`agent-session serve` exposes clock-safe observation anchors and near-real-time
activity metadata without coupling provider hooks to network clients. This is an
additive contract: existing clients may continue polling `GET /sessions`, and
the terminal attach WebSocket is unchanged.

## Session observation anchor

Successful `GET /sessions` responses retain
`schema_version: cli.agent-session.serve.v1` and add
`data.observed_at`, an RFC 3339 daemon timestamp sampled after the returned
session list is assembled and immediately before response serialization.
Consumers use that value as the wall-clock anchor for the adjacent state, then
advance elapsed time from a local monotonic clock. The existing poll remains
the authoritative reconciliation path after stream gaps and for old daemons.

## Endpoint and authentication

`GET /activity/events` returns `text/event-stream`. It requires the same Bearer
token as write and attach endpoints: a missing/invalid token returns 401, and a
daemon without a configured token returns 503. Tokens never appear in URLs or
event data. Responses use `Cache-Control: no-cache, no-transform` and
`X-Accel-Buffering: no`.

If the platform filesystem watcher cannot be initialized, the daemon preserves
all existing endpoints and returns 503 `activity-stream-unavailable` from this
route so clients explicitly fall back to session polling rather than treating
heartbeats from a degraded stream as healthy.

Every SSE frame uses:

- `id: <stream_id>:<sequence>`
- `event: snapshot`, `heartbeat`, or `reset`
- one JSON object in `data`

The JSON shape is:

```json
{
  "schema_version": "agent-session.activity-stream.event.v1",
  "type": "snapshot",
  "stream_id": "opaque-daemon-boot-id",
  "sequence": 42,
  "machine": "sympoies",
  "observed_at": "2026-07-11T17:00:00Z",
  "sessions": [
    {
      "id": "session-id",
      "turn_state": {
        "schema_version": "agent-session.turn-state.v1",
        "phase": "waiting",
        "phase_changed_at": "2026-07-11T16:59:00Z",
        "revision": 7,
        "source": {
          "kind": "provider_hook",
          "provider": "codex",
          "confidence": "authoritative"
        },
        "current_turn": null,
        "last_turn": {
          "provider_turn_id": "opaque-id",
          "started_at": "2026-07-11T16:58:00Z",
          "completed_at": "2026-07-11T16:59:00Z",
          "outcome": "completed"
        }
      }
    }
  ]
}
```

`snapshot` and `reset` contain a full list of daemon session ids. `turn_state`
is `null` when no valid activity snapshot exists. `heartbeat` omits `sessions`
and is emitted every 15 seconds. `stream_id` is stable for one daemon process;
`sequence` strictly increases across snapshots and heartbeats.

## Replay, gaps, and backpressure

Without `Last-Event-ID`, a subscriber first receives the latest full snapshot.
A retained cursor for the current stream replays events whose sequence is
greater than the cursor. A malformed cursor, another daemon boot id, a cursor
beyond the current sequence, or an evicted cursor receives the latest full
state as `reset`. The replay window retains at most 128 frames and at most
512 KiB of serialized event data. Oldest frames are evicted until both limits
hold; if any sequence needed by a cursor was evicted (including an individual
snapshot larger than the byte budget), the subscriber receives a full reset.

Consumers deduplicate by `(machine, stream_id, sequence)`. A sequence gap or a
`reset` triggers immediate `GET /sessions` reconciliation. The regular session
poll remains active for convergence, daemon health, and old-peer fallback.

The daemon uses a bounded 32-frame broadcast queue. Producers never await a
subscriber. A lagged subscriber receives a full reset from the latest state.
At most 64 concurrent SSE subscribers are admitted. Further authenticated
requests receive 429 `activity-stream-capacity`; disconnecting a subscriber
releases its permit and polling remains available throughout saturation.
Filesystem notifications for `activity.json` and session lifecycle changes are
coalesced before snapshot projection; HTTP polling is not the transition source.

## Privacy boundary

Stream state is constructed from an allowlist. It may contain session id,
phase/timestamps/revision, provider source/confidence, opaque projected turn id,
attention kind/time/count, and outcome. Forward-compatible unknown fields from
durable snapshots are deliberately excluded.

Prompt, response, command, tool payload, terminal output, transcript/config
paths or contents, and credentials are forbidden. Provider hook processes only
perform their existing bounded local durable writes; they never contact the
daemon, wait for a subscriber, or perform network I/O for streaming.
