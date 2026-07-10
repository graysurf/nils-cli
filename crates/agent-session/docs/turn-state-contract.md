# Agent-session turn state contract

## Compatibility

`agent-session.turn-event.v1` is the local ingestion contract and
`agent-session.turn-state.v1` is the optional session-view contract. New
`agent-session` versions add `turn_state` and `runtime_started_at` to start,
resume, list, command, glance, and serve responses. Old records and damaged or
unsupported provider integrations remain readable; absent activity is omitted,
and corrupt activity degrades to `unknown` without breaking session lifecycle.

Consumers must ignore additive unknown fields. A future, unrecognized
`turn_state.schema_version` must be treated as unknown rather than interpreted
as a v1 phase.

## Normalized turn event

The local-only command accepts one JSON object on stdin:

```text
agent-session activity event <session-id> --stdin --format json
```

Required fields:

| Field | Contract |
| --- | --- |
| `schema_version` | exactly `agent-session.turn-event.v1` |
| `event_id` | opaque id used for idempotency |
| `runtime_id` | exact active `AGENT_SESSION_RUNTIME_ID` |
| `provider` | `codex`, `claude`, or `hermes`; must match the session |
| `kind` | `turn_started`, `attention_requested`, `attention_cleared`, `progress`, `stop_observed`, `turn_completed`, or `turn_failed` |
| `confidence` | `authoritative`, `observed`, or `inferred` |

Optional allowlisted fields are `provider_session_id`, `provider_turn_id`,
`attention_id`, `attention_kind`, `source_kind`, and `provider_time`. Unknown
keys fail parsing. Identifiers are bounded, non-empty, and control-free.
`attention_requested` requires an opaque correlation id and one of `approval`,
`clarification`, `authentication`, or `other`; `attention_cleared` requires the
matching id.

Provider hooks never persist their raw session or turn identifiers. They are
projected to runtime-scoped SHA-256 opaque values before validation, storage,
or exposure. When exact provider resume identity is known it must match; when
it is not known, the first non-empty projected provider session id binds the
runtime; later changes or identity-less events are rejected.

The host receive time is canonical. Provider time is accepted only as inert
metadata in v1 and never advances state ahead of host observation. Runtime id
and provider mismatch are rejected before timestamping, journaling, or reducing.

## Turn state

Example:

```json
{
  "schema_version": "agent-session.turn-state.v1",
  "phase": "needs_input",
  "phase_changed_at": "2026-07-10T12:31:28Z",
  "revision": 8,
  "source": {
    "kind": "provider_hook",
    "provider": "codex",
    "confidence": "observed"
  },
  "current_turn": {
    "provider_turn_id": "turn-id",
    "started_at": "2026-07-10T12:31:08Z",
    "attention": {
      "kind": "approval",
      "requested_at": "2026-07-10T12:31:28Z",
      "pending_count": 2
    }
  },
  "last_turn": {
    "provider_turn_id": "previous-turn-id",
    "started_at": "2026-07-10T12:24:31Z",
    "completed_at": "2026-07-10T12:28:04Z",
    "outcome": "completed"
  }
}
```

Phases are `starting`, `working`, `waiting`, `needs_input`, and `unknown`.
Source kinds are `provider_hook`, `console_observation`,
`terminal_heuristic`, and `runtime`. Last-turn outcomes are `completed`,
`interrupted`, `failed`, and `unknown`.

`pending_count` is the only client-visible attention correlation summary. Only
runtime-scoped projections of provider session/turn identifiers may be exposed.
Provider request ids and the active runtime id remain protected in local
activity storage. At most 64 attention correlations are retained in the
snapshot; additional requests contribute only to a bounded overflow summary
and keep `needs_input` conservatively latched until a new turn or completion.

## Deterministic transition rules

| Input | Rule |
| --- | --- |
| new runtime | interrupt an open turn, preserve it as last turn, clear attention, enter authoritative `starting` |
| `turn_started` | interrupt an older open turn, clear old attention, enter `working` |
| `attention_requested` | keep current start time, add one opaque pending request, enter `needs_input` |
| correlated `attention_cleared` | remove only that request; remain `needs_input` while any remain |
| uncorrelated `progress` | may establish/retain `working`, but never clears attention |
| `stop_observed` | increment evidence revision and journal it; never changes to Waiting |
| matching `turn_completed` | close current turn, clear attention, enter `waiting` |
| matching `turn_failed` | close current turn with failed outcome, clear attention, enter `waiting` |
| late completion for older turn | retain the newer current phase |
| duplicate `event_id` | no state or revision change within the 4096-event active-runtime replay horizon |
| missing/prior runtime id | reject before host timestamp or reducer |
| corrupt snapshot | expose safe `unknown`; list/serve/delete remain available |

Revision is monotonic for each accepted non-duplicate event and runtime
boundary. Phase timestamps change only when the phase changes. Durations are
derived by clients and are never persisted separately.

## Persistence and concurrency

Each session owns:

- `activity.json`: atomic mode-0600 snapshot;
- `activity.journal.jsonl`: atomic mode-0600 metadata journal, bounded to 256
  events and 64 KiB;
- `.activity.lock`: mode-0600 cross-process advisory lock.

Activity files are separate from `session.json`, so title/resume writes and hook
writes cannot overwrite each other. Every reducer transaction holds the lock,
validates the active runtime, records one pending journal entry in the atomic
snapshot, updates the bounded journal idempotently, and clears the pending
marker. A later event repairs an interrupted split write before reduction.
Event-id digests use a separate 4096-entry runtime replay horizon rather than
the shorter journal retention; reaching the horizon rejects further events with
resume guidance instead of forgetting old ids. Session deletion removes the
entire session directory.

## Privacy and provider adapter boundary

The schemas forbid prompt/assistant/terminal/transcript text, commands, tool
arguments/results, paths from provider transcripts, credentials, tokens, and
free-form provider errors. Raw hook payloads are parsed in memory and projected
onto the allowlist; content fields are never serialized.

`provider-prompt.v1` is a separate, advisory attach/title protocol. It is not a
turn event source, it is not persisted into activity files, and a prompt-event
drop/reconnect/title timeout cannot change durable turn state.

## Setup and diagnostics

```text
agent-session activity setup --agent <provider> --dry-run
agent-session activity setup --agent <provider> --apply
agent-session activity setup --agent <provider> --repair
agent-session activity setup --agent <provider> --remove
agent-session activity doctor [--agent <provider>] --format json
```

Setup is explicit, additive, idempotent, and reversible. It preserves unrelated
provider config, detects an observed concurrent modification before replacement,
and never auto-accepts Codex trust or Hermes consent. Doctor
reports installed version, audited classification, config status, finality and
correlation limits, trust requirements, and repair guidance without emitting
provider config content.
