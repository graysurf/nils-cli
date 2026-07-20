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
`failure_reason`, `attention_id`, `attention_kind`, `source_kind`, and
`provider_time`. Unknown
keys fail parsing. Identifiers are bounded, non-empty, and control-free.
`attention_requested` requires an opaque correlation id and one of `approval`,
`clarification`, `authentication`, or `other`; `attention_cleared` requires the
matching id.

`failure_reason` is valid only on an authoritative `turn_failed` event and is
limited to `usage_exhausted`, `authentication`, `organization`, `billing`,
`invalid_request`, `service`, `max_output_tokens`, or `unknown`. Claude Code's
documented structured `StopFailure.error` is reduced into that allowlist. Raw
error details, rendered provider errors, transcript paths, prompts, and
assistant content are discarded. A raw Codex interactive notification does not
carry an equivalent structured failure field. An agent-session-managed Codex
app-server v2 runtime reuses the stable v1 `source_kind: "provider_hook"` wire
value only after its live bound thread/turn reports terminal `failed` plus exact
`usageLimitExceeded`. Within v1, `provider_hook` denotes authoritative
provider-structured evidence from either a hook or the bound protocol; that
metadata-only projection can authoritatively arm usage-reset auto-resume.

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

## Attention correlation authority

The v1 reducer is provider-neutral. Exact provider adapters may request and
clear attention only with the same opaque, runtime-scoped correlation. A
different id and an uncorrelated `progress` event never prove resolution;
completion, failure, a new turn, or a new runtime remain the only boundaries
that may clear all outstanding attention.

Each runtime selects one attention authority when it is created or resumed and
keeps that authority for the lifetime of the runtime. Hook and protocol
observations are not paired by arrival time, event kind, or semantic similarity.
A provider protocol may be selected as the exact authority only when the
admitted interaction matrix proves complete request coverage and the runtime
suppresses the corresponding generic attention hook at its source. Otherwise
the runtime selects conservative hook authority. An event from a suppressed
attention source is an authority-invariant breach: it neither creates attention
nor advances `last_progress_at`, and the runtime degrades to unknown until a new
runtime or resume selects authority again.

Client dismissal remains presentation-only fingerprint suppression. It cannot
clear producer-owned attention or influence authority selection.

For Codex, a raw or unmanaged runtime selects `hook`; its generic
`PermissionRequest` remains a conservative approval latch. An audited managed
app-server runtime selects `protocol`, injects
`AGENT_SESSION_ATTENTION_AUTHORITY=protocol`, and suppresses that generic hook
before the helper is invoked, including when the installed helper predates
authority-aware ingest. Protocol authority is unavailable until that guarded
installed command is verified and no second direct unguarded reporter is
present; app-server transport may still run with hook authority. Its private
proxy admits only the audited blocking
request method allowlist and `serverRequest/resolved`. Request ids retain their
JSON `string` versus `int64` type only in the bounded in-memory pending table;
each admitted request occurrence receives a fresh opaque correlation token.
The raw request id is never persisted or exposed, and a provider may reuse the
same id after its prior request resolves without hitting durable replay
deduplication. A recognized malformed request, wrong-turn request, observation
loss, malformed proxy data, projection failure, or hook/record authority
mismatch writes a durable runtime-generation unhealthy marker. A private health
fence linearizes the scoped pending poison marker against activity commits and
the durable auto-resume submission claim; stable activity mirroring then uses
the session-record lock. The marker owns a stable degradation revision and
phase timestamp. Invalid, unreadable, or parseable-but-nondegraded marker states
fail closed instead of being exposed.
The public v1 state becomes `unknown`, auto-resume becomes unavailable, and
later same-runtime events are rejected; only a new runtime generation can
remove the marker, select authority, and recover. If an open turn has no
provider turn id, its first non-null exact attention request binds the turn;
later mismatches fail closed. Nullable MCP elicitation remains admitted without
inventing a turn id.

For Claude Code, `AskUserQuestion` remains exact through `tool_use_id`.
`Elicitation` and `ElicitationResult` are also exact when both carry the same
non-empty `elicitation_id`: form mode maps to `clarification`, and URL mode maps
to `authentication`. Since Claude's hook contract makes the id optional, a
request without it is a conservative latch and a result without it is a no-op.
Generic permission and notification signals remain conservative. The managed
Claude setup excludes the uncorrelated `permission_prompt` notification because
current Claude versions emit it as a duplicate of `PermissionRequest`, and an
`AskUserQuestion` `PermissionRequest` is ignored because the same interaction is
already owned by exact PreToolUse/PostToolUse correlation.

Managed Claude setup also installs a general `PreToolUse` hook that normalizes
to uncorrelated `progress`: a continued turn last observed at `idle_prompt` can
re-establish `working` as soon as a tool starts. The exact `AskUserQuestion` arm
is evaluated first, so its `PreToolUse` remains `attention_requested` rather
than progress. `SubagentStop` is deliberately neither installed nor admitted as
progress because it identifies a completed subagent without correlating that
callback to active parent work; a late background callback must not resurrect a
genuinely waiting parent turn. Positive progress never clears pending attention.

## Deterministic transition rules

| Input | Rule |
| --- | --- |
| new runtime | interrupt an open turn, preserve it as last turn, clear attention, enter authoritative `starting` |
| `turn_started` | interrupt an older open turn, clear old attention, enter `working` |
| `attention_requested` | keep current start time, add one opaque pending request, enter `needs_input` |
| correlated `attention_cleared` | remove only that request, advance monotonic `last_progress_at`, and remain `needs_input` while any remain |
| uncorrelated `progress` | advance monotonic `last_progress_at`; may establish/retain `working`, but never clears attention |
| `stop_observed` | increment evidence revision and journal it; never changes to Waiting |
| matching `turn_completed` | close current turn, clear attention, enter `waiting`; authoritative Codex notifications require the exact open turn id |
| matching `turn_failed` | close current turn with failed outcome, clear attention, enter `waiting` |
| late completion for older turn | retain the newer current phase |
| duplicate exact-replay `event_id` | no state or revision change within the 4096-event active-runtime replay horizon; uncorrelated Claude progress instead uses the short semantic guard |
| missing/prior runtime id | reject before host timestamp or reducer |
| corrupt snapshot | expose safe `unknown`; list/serve/delete remain available |
| unhealthy authority/projection | expose `unknown`, accept no later event in the same runtime, recover only on a new runtime generation |

Claude `PermissionRequest` signals for tools other than `AskUserQuestion` mean
that a permission dialog is actually being shown, so they emit
`attention_requested` even when the payload reports `permission_mode:
"bypassPermissions"`. The mode hint does not override the observed prompt;
bypass mode retains a root/home deletion circuit breaker. Because these
approvals have no correlated clear event, they keep the conservative latch
above until completion, a new turn, or a runtime boundary. User-owned or
previously configured `permission_prompt` notification reporters normalize the
same way, but the managed setup does not install that duplicate source.

Hermes 0.18.2 shell hooks put approval kwargs under the allowlisted `extra`
object and may leave top-level `session_id` empty. Agent-session falls back to
`extra.session_key`, projects non-empty `extra.tool_call_id` as the exact
runtime-scoped pre/post correlation, and treats replayed exact callbacks
idempotently. The event kind and projected tool-call id derive a stable
runtime-scoped event id retained by the bounded replay index, so interleaving,
elapsed wall time, response clearing, process restart, and bounded journal
eviction cannot reopen a delivered callback. This lets identical command tuples
with different tool-call ids clear independently and out of order. Missing,
null, or empty tool-call ids use
the compatibility tuple fallback: `command`, `description`, `pattern_key`,
sorted/deduplicated `pattern_keys`, `session_key`, and `surface` are
canonicalized only in memory and their SHA-256 is projected. Identical fallback
tuples remain indistinguishable, so each observed pre callback increases
conservative pending multiplicity and an ambiguous remainder clears only at
completion, a new turn, or a runtime boundary. Raw approval kwargs never enter
activity storage; only documented response choices emit observed clear events.

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
journal entry in the atomic snapshot, updates the fixed replay index when the
event requires exact replay protection, appends the bounded journal
idempotently, and clears the pending marker. A later event or runtime transition
repairs an interrupted split write before reduction. The replay index is
separate from the shorter journal retention, gives expected O(1) duplicate
checks without growing the JSON snapshot, and rejects further exact-replay
events at its 4096-event capacity with resume guidance instead of forgetting old
ids. Uncorrelated Claude provider-hook `progress` has idempotent reducer
semantics and no stable provider event id, so it keeps bounded journal and
split-write repair coverage but relies on the short semantic replay guard rather
than consuming exact replay slots. The replay file header must match the
snapshot runtime tuple. A missing, truncated, or swapped index for a nonempty
exact-replay horizon fails closed and exposes Unknown; creating the index also
syncs its parent directory. Provider-hook events additionally use a short
metadata-only semantic replay guard so concurrent duplicate delivery cannot
interrupt the same turn, inflate an uncorrelated attention latch, or rewrite an
already observed completion.
Unknown additive JSON fields survive supported reads and writes. A corrupt or
future-version snapshot is moved to a private quarantine file before a fresh
runtime snapshot is written. Session deletion removes the entire session
directory.

## Privacy and provider adapter boundary

The schemas forbid prompt/assistant/terminal/transcript text, commands, tool
arguments/results, paths from provider transcripts, credentials, tokens, and
free-form provider errors. Raw hook payloads are parsed in memory and projected
onto the allowlist; the Codex notification adapter applies the same boundary to
the provider's single JSON argv. Content fields are never printed or serialized
by agent-session. Codex supplies that JSON as a process argument, so prompt,
assistant, and cwd content remains transiently observable through same-host
process inspection until the helper exits. Restricted process visibility is a
deployment requirement; eliminating this upstream argv exposure requires a
future provider-supported stdin/metadata-only transport or App Server boundary.

`provider-prompt.v1` is a separate, advisory attach/title protocol. It is not a
turn event source, it is not persisted into activity files, and a prompt-event
drop/reconnect/title timeout cannot change durable turn state.

## Setup and diagnostics

```text
agent-session activity setup --agent <provider> --dry-run
agent-session activity setup --agent <provider> --apply
agent-session activity setup --agent <provider> --repair
agent-session activity setup --agent codex --repair --dry-run
agent-session activity setup --agent codex --repair --expected-preview-digest sha256:<reviewed-plan-digest>
agent-session activity setup --agent <provider> --remove
agent-session activity doctor [--agent <provider>] --format json
```

`activity setup` always forwards to the shared `agent-hook setup` owner, using
`AGENT_HOOK_BIN` when explicitly set and otherwise resolving `agent-hook` on
`PATH`. It maps the compatibility provider and digest options, requests the versioned
JSON contract, and adds `compatibility_owner: "agent-hook"` to the returned
result. It never writes provider configuration itself.

If `agent-hook` is absent or cannot be started, setup returns the typed
`agent-hook-setup-unavailable` error with shared unavailable exit `69` and
install-and-repeat-preview guidance. A valid child error envelope preserves
the shared `1`, `64`, `65`, `69`, or `70` exit class returned by `agent-hook`;
malformed or unsupported child output remains a data-contract failure.
There is no embedded registration fallback, including for `--apply`,
`--repair`, or `--remove`, so a mixed-version installation cannot reactivate a
second writer.

`activity doctor` remains a read-only compatibility diagnostic. It recognizes
exact pre-dispatch `agent-session` hook and Codex notify shapes, including a
bounded audited Computer Use outer wrapper whose exact helper path is a regular
executable with no symlink below the active config root. It reports conflicts
without provider content, probes provider versions with bounded timeouts, and
selects the newest current-runtime diagnostic deterministically. The retained
`activity hook` and `activity notify` commands continue to ingest already
installed compatibility callbacks fail-open while provider registration converges on
`agent-hook`.
