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

Provider hooks and direct `activity event` callers may supply bounded raw opaque
session or turn identifiers. Ingestion projects them to runtime-scoped SHA-256
opaque values before storage or exposure; an already projected `local:v1:` value
must carry exactly 64 hexadecimal digest characters. When exact provider resume
identity is known it must match; when it is not known, the first non-empty
projected provider session id binds the runtime; later changes or identity-less
events are rejected.

The host receive time is canonical. Provider time is accepted only as inert
metadata in v1 and never advances state ahead of host observation. Runtime id
and provider mismatch are rejected before timestamping, journaling, or reducing.

For the existing Codex tmux/TUI integration, the provider appends one bounded
JSON argument to this owned command:

```text
agent-session activity notify --agent codex <payload>
```

Only `type == "agent-turn-complete"` is recognized. Both `thread-id` and
`turn-id` are required and projected through the same runtime-scoped namespace
as hook identifiers. A matching notification emits authoritative
`turn_completed`; raw `Stop` remains `stop_observed`. Fields such as `cwd`,
`input-messages`, and `last-assistant-message` are discarded before the
normalized event is built. Unknown notification types no-op; invalid, oversized,
stale, or mismatched payloads fail open to Codex and cannot complete the turn.

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
    "last_progress_at": "2026-07-10T12:31:41Z",
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

`current_turn.last_progress_at` is optional additive v1 metadata. It advances
monotonically only when accepted provider-hook evidence proves progress for the
active runtime and open turn. It never uses terminal output, spinner text,
focus, browser clocks, or provider-supplied timestamps. Exact
`AskUserQuestion` completion/failure counts as progress while clearing only its
own runtime-scoped clarification correlation. Old snapshots omit the field and
remain valid.

## Deterministic transition rules

| Input | Rule |
| --- | --- |
| new runtime | interrupt an open turn, preserve it as last turn, clear attention, enter authoritative `starting` |
| `turn_started` | interrupt an older open turn, clear old attention, enter `working` |
| `attention_requested` | keep current start time, add one opaque pending request, enter `needs_input` |
| correlated `attention_cleared` | remove only that request, advance monotonic `last_progress_at`, and remain `needs_input` while any remain |
| uncorrelated `progress` | advance monotonic `last_progress_at`; may establish/retain `working`, but never clears attention |
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

## Client presentation projection

Durable phase remains conservative for old-client safety. A new client may
derive simultaneous work plus attention from the additive timestamps without
rewriting server state:

| Durable evidence | Presentation |
| --- | --- |
| open turn, no pending attention | Working from `started_at` |
| attention pending, no later provider progress | Needs input from `requested_at` |
| attention pending and `last_progress_at > requested_at` | Working plus input requested, keeping both timers |
| exact clarification clear removes the final request | Working from the original `started_at` |
| proven completion or failure | Waiting from `completed_at` |

Unavailable, stopped, resumable, and connecting health/runtime states remain
higher priority than this activity projection. Progress never implies that an
uncorrelated permission or notification request was answered.

## Persistence and concurrency

Each session owns:

- `activity.json`: atomic mode-0600 snapshot;
- `activity.journal.jsonl`: atomic mode-0600 metadata journal, bounded to 256
  events and 64 KiB;
- `activity.replay.bin`: fixed-size mode-0600 open-addressed replay index for
  4096 runtime-scoped event-id digests, with a versioned launch-id/generation
  header;
- `.activity.lock`: mode-0600 cross-process advisory lock.

Activity files are separate from `session.json`, so title/resume writes and hook
writes cannot overwrite each other. Every reducer transaction holds the lock,
validates both the active launch id and runtime generation, records one pending
journal entry in the atomic snapshot, updates the fixed replay index and bounded
journal idempotently, and clears the pending marker. A later event or runtime
transition repairs an interrupted split write before reduction. The replay
index is separate from the shorter journal retention, gives expected O(1)
duplicate checks without growing the JSON snapshot, and rejects further events
at its 4096-event capacity with resume guidance instead of forgetting old ids.
The replay file header must match the snapshot runtime tuple. A missing,
truncated, or swapped index for a nonempty snapshot fails closed and exposes
Unknown; creating the index also syncs its parent directory. Provider-hook
events additionally use a short metadata-only semantic replay guard so
concurrent duplicate delivery cannot interrupt the same turn, inflate an
uncorrelated attention latch, or rewrite an already observed completion.
Unknown additive JSON fields survive supported reads and writes. A corrupt or
future-version snapshot is moved to a private quarantine file before a fresh
runtime snapshot is written. Session deletion removes the entire session
directory.

## Privacy and provider adapter boundary

The schemas forbid prompt/assistant/terminal/transcript text, commands, tool
arguments/results, paths from provider transcripts, credentials, tokens, and
free-form provider errors. Raw hook payloads are parsed in memory and projected
onto the allowlist; the Codex notification adapter applies the same boundary to
the provider's single JSON argv. Content fields are never serialized.

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
and never auto-accepts Codex trust or Hermes consent. Codex setup additionally
owns the exact `agent-session activity notify --agent codex` argv in the
singular top-level `notify` field of `~/.codex/config.toml`: it inserts the argv
only when absent, recognizes it idempotently, removes only that exact value, and
preserves/refuses every user-owned value before changing the hooks file. Doctor scans session
records once, probes provider versions concurrently with a two-second bound per
provider, and reports installed version or a bounded probe error, audited
classification, config status, finality and correlation limits, trust
requirements, and repair guidance without emitting provider config content.
Configured status requires every exact owned hook command/timeout and, for
Codex, the exact notify argv; helper health resolves the bare `agent-session`
command on PATH. Hook/notification diagnostics are bound to the active
launch id/generation and the newest current-runtime diagnostic is selected
deterministically across sessions.

Setup JSON distinguishes current and prospective state: `configured` and
`changed` describe the file after the command, while `would_configure` and
`would_change` describe the requested transformation. Dry-run never reports a
file change and leaves `configured` at the current value.
