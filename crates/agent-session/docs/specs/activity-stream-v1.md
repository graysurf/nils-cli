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

The broker lifecycle is explicit: `starting` advances to `ready` only after the
filesystem watcher and initial session snapshot both succeed; a watcher callback
error or later snapshot-collection failure moves it permanently to `degraded`.
On degradation, an existing stream receives one full `reset` and then closes,
heartbeats stop, and new requests return 503 `activity-stream-unavailable`.
The daemon preserves all existing endpoints, so clients fall back to session
polling rather than treating a degraded stream as healthy. `GET /sessions` and
the broker use the same injected session snapshot source, keeping polling and
stream projection aligned.

Each degraded terminal `reset` receives a new sequence greater than every
previous frame from that stream, so consumer deduplication cannot discard the
reconciliation signal. Its `observed_at` is deliberately the anchor of the last
successful snapshot collection represented by `sessions`, not the later time
when degradation was detected. A subsequent successful `GET /sessions` carries
its own post-assembly anchor and therefore outranks that cached reset. Consumers
must not treat a reset's delivery time as evidence that its cached sessions are
fresh.

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

`snapshot` and ordinary `reset` frames contain a full list of daemon session
ids. `turn_state` is `null` when no valid activity snapshot exists. `heartbeat`
omits `sessions` and is emitted every 15 seconds. `stream_id` is stable for one
daemon process; `sequence` strictly increases across emitted frames.

A projected snapshot whose complete SSE wire frame would exceed 512 KiB is not
retained or broadcast. The producer emits this bounded content-free reset
instead:

```json
{
  "schema_version": "agent-session.activity-stream.event.v1",
  "type": "reset",
  "stream_id": "opaque-daemon-boot-id",
  "sequence": 43,
  "machine": "sympoies",
  "observed_at": "2026-07-11T17:00:00Z",
  "reason": "oversized_snapshot"
}
```

Only this exact `reset` reason permits `sessions` to be omitted. It is a
content-free invalidation, not an empty session list, and requires immediate
`GET /sessions` reconciliation. The reset is emitted once when state crosses
from bounded to oversized; later oversized refreshes update polling state but
do not repeat the reset or advance the stream sequence. A later bounded
snapshot is emitted as the recovery transition. Polling remains authoritative
and available.

Nested optional `turn_state` leaves use the same omission semantics as
`GET /sessions`: absent provider ids, progress timestamps, attention, current
turn, and last turn are omitted rather than serialized as `null`. The
session-level `turn_state: null` remains intentional and means no valid activity
snapshot. The exact multi-session Rust producer fixture consumed by downstream
contract tests is
[`tests/fixtures/activity/activity-stream-v1-multi-session.json`](../../tests/fixtures/activity/activity-stream-v1-multi-session.json).

## Replay, gaps, and backpressure

Without `Last-Event-ID`, a subscriber first receives the latest full snapshot.
A retained cursor for the current stream replays events whose sequence is
greater than the cursor. A malformed cursor, another daemon boot id, a cursor
beyond the current sequence, or an evicted cursor receives the latest full
state as `reset`. The replay window retains at most 128 frames and at most
512 KiB of pre-framed SSE wire bytes. Oldest frames are evicted until both
limits hold; if any sequence needed by a cursor was evicted, the subscriber
receives a reset. An oversized latest snapshot receives the content-free reset
described above.

Consumers deduplicate by `(machine, stream_id, sequence)`. A sequence gap or a
`reset` triggers immediate `GET /sessions` reconciliation. Degraded resets have
unique increasing sequences even when several subscribers stop concurrently.
The regular session poll remains active for convergence, daemon health, and
old-peer fallback.

The producer serializes each event payload and complete SSE frame once, caches
its wire length, and shares the same immutable frame across replay, broadcast,
and every subscriber. A single-frame broadcast slot prevents the queue from
retaining 32 large frames outside the 512 KiB replay budget; producers never
await a subscriber, and a lagged subscriber receives a cached reset from the
latest state. At most 64 concurrent SSE subscribers are admitted. Further
authenticated requests receive 429 `activity-stream-capacity`; disconnecting a
subscriber releases its permit and polling remains available throughout
saturation.

Filesystem notifications for `activity.json` and session lifecycle changes use
a capacity-one dirty bit. The first isolated refresh waits for a trailing 25 ms
quiet window. Under a continuous burst, a refresh starts by the 250 ms cadence;
after any refresh starts, the next refresh cannot start for at least 250 ms.
Notifications arriving during a scan stay dirty and converge in a later
rate-bounded refresh.

A notify event marked `need_rescan()` forces the same full snapshot collection
even when it has no relevant path. Removal or rename of the watched sessions
root first recreates the directory and replaces the recursive watcher before a
full refresh. If that root-loss invalidation cannot be queued or the watcher
cannot be re-armed, the broker degrades: existing streams receive their final
reset and EOF, heartbeats stop, and new stream requests receive the polling
fallback response. HTTP polling remains available throughout and is not the
normal transition source.

## Privacy boundary

Stream state is constructed from an allowlist. It may contain session id,
phase/timestamps/revision, provider source/confidence, opaque projected turn id,
attention kind/time/count, and outcome. Forward-compatible unknown fields from
durable snapshots are deliberately excluded.

Prompt, response, command, tool payload, terminal output, transcript/config
paths or contents, and credentials are forbidden. Provider hook processes only
perform their existing bounded local durable writes; they never contact the
daemon, wait for a subscriber, or perform network I/O for streaming.
