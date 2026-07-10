# Provider turn signal evidence

## Status and scope

This report freezes the provider lifecycle evidence used by
`agent-session.turn-event.v1` and `agent-session.turn-state.v1`. It was audited
on 2026-07-11 against Codex CLI 0.144.1, Claude Code 2.1.206, Hermes Agent
0.18.0, and the agent-session 1.21.8 implementation baseline. The support floors are deliberately the
oldest versions directly covered by this audit, not guesses about earlier
releases.

The fixtures under `tests/fixtures/activity/` contain lifecycle identifiers and
event names only. Prompt text, assistant output, tool input/output, commands,
transcript paths, credentials, and terminal content are removed before the
normalized event is created.

## Evidence sources

- [Codex hooks](https://learn.chatgpt.com/docs/hooks) documents concurrent
  matching hooks, additive discovery across active config layers, trust review,
  `turn_id`, and the `UserPromptSubmit`, `PermissionRequest`, `PostToolUse`, and
  `Stop` payloads.
- [Codex notifications](https://learn.chatgpt.com/docs/config-file/config-advanced#notifications)
  documents the `agent-turn-complete` notify surface and its `thread-id` and
  `turn-id`. Setup installs the exact agent-session argv when `notify` is absent
  or already owned; a different singular user command is preserved and blocks
  automatic setup. Codex appends its full notification JSON to that argv;
  agent-session discards content after parsing and never persists it, but the
  provider-supplied argv is transiently visible to same-host process inspection.
- [Claude Code hooks](https://code.claude.com/docs/en/hooks) documents parallel
  matching hooks, exact `AskUserQuestion` matching, shared `tool_use_id` on
  `PreToolUse`/`PostToolUse`/`PostToolUseFailure`, `PermissionRequest` without
  that id, and notifications including `idle_prompt`.
- [Hermes hooks](https://github.com/NousResearch/hermes-agent/blob/main/website/docs/user-guide/features/hooks.md)
  documents `pre_llm_call`, `post_llm_call`, `pre_approval_request`,
  `post_approval_response`, shell-hook consent, and synthetic hook tests. The
  installed source was also checked because Hermes is not an agent-session-owned
  stable interface.

## Support matrix

| Provider | Audited floor | Classification | Start | Completion | Attention | Failure | Setup |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Codex | 0.144.1 | supported | `UserPromptSubmit`, observed | matching `agent-turn-complete`, authoritative; raw `Stop` remains journal evidence only | `PermissionRequest`, observed conservative latch | runtime/fallback only | additive hooks in `~/.codex/hooks.json` plus exact owned notify argv in `~/.codex/config.toml`; user-owned notify conflicts fail closed |
| Claude Code | 2.1.206 | partial | `UserPromptSubmit`, observed | `idle_prompt`, observed; raw `Stop` is journal evidence only | exact `AskUserQuestion` request/clear; `PermissionRequest`/notification conservative latch | `StopFailure`, observed; `AskUserQuestion` tool failure clears only its clarification | additive merge into `~/.claude/settings.json` |
| Hermes | 0.18.0 | supported | `pre_llm_call`, observed | successful non-interrupted `post_llm_call`, authoritative | pre/post approval hooks exist, but clearing remains conservative | runtime/fallback only | additive merge into `~/.hermes/config.yaml`; Hermes consent remains mandatory |

Versions below the audited floor remain usable. `activity doctor` reports them
as unverified and session views retain optional-field/activity fallback rather
than fabricating a phase.

## Finality and correlation findings

### Codex

Matching hooks from multiple sources run concurrently. A `Stop` observer cannot
know whether another matching hook will return a continuation decision, so raw
`Stop` cannot produce Waiting. The event is retained as `stop_observed` with
observed confidence. The official `agent-turn-complete` notification fires after
the completed agent turn and carries both thread and turn identifiers. The
adapter requires both, projects them through the same active-runtime namespace
as hook identifiers, and emits authoritative `turn_completed` only when the
runtime, provider session/thread, and open turn all match. A missing, malformed,
stale-runtime, wrong-thread, or wrong-turn notification cannot complete the
turn. Duplicate completion is idempotent.

`PermissionRequest` has `turn_id` but no request identifier shared with
`PostToolUse`; `PostToolUse.tool_use_id` cannot be correlated to the preceding
approval. Multiple approval requests therefore use agent-session-owned opaque
attention ids and remain latched. Unrelated progress never clears them.

### Claude Code

Matching hooks also run in parallel, and `Stop` hooks may continue the turn.
Raw `Stop` is therefore treated exactly like Codex raw Stop. `idle_prompt` is a
later provider notification explicitly meaning that Claude is done and waiting
for another prompt, so it may yield observed Waiting.

`PreToolUse`, `PostToolUse`, and `PostToolUseFailure` expose the same
`tool_use_id` for `AskUserQuestion`. The adapter projects that raw id through a
runtime-scoped SHA-256 namespace before persistence, records clarification
attention at PreToolUse, and clears only the matching clarification after
success or failure. The installed-version live probe retained only event names,
tool name, id presence, and a one-way comparison digest; its Pre/Post digests
matched.

`PermissionRequest` explicitly omits `tool_use_id`, even though later tool events
include one. A separate installed-version permission/progress probe reproduced
that asymmetry. Permission notifications also omit a stable request id. Those
signals remain uncorrelated conservative latches cleared only by a proven
completion, a new turn, or a runtime boundary; later progress may prove work is
continuing but never proves the request was answered.

### Hermes

The installed `post_llm_call` fires only after a successful final response and
does not fire for interruption, so it is authoritative completion at the
audited version. Approval hooks expose `session_key`, surface, and response
choice, but no stable per-request id. `post_approval_response` is recorded as
progress and cannot by itself clear Needs input; completion or a new turn clears
the latch.

## Concurrency, continuation, and privacy probes

The executable fixtures cover:

- two concurrent attention requests, correlated one-by-one clearing, and a
  metadata-only `pending_count`;
- exact AskUserQuestion request/success/failure correlation and independent
  clearing alongside unrelated generic attention;
- unrelated progress while attention is pending, including monotonic
  `current_turn.last_progress_at` without phase relaxation;
- Stop followed by continuation/new-turn evidence;
- raw Codex Stop followed by matching authoritative completion, plus wrong
  thread/turn, missing identifier, duplicate, stale-runtime, malformed, and
  oversized notification cases;
- duplicate and out-of-order normalized events;
- rejection of a delayed prior-runtime event before host timestamping;
- runtime/provider-session binding and runtime-scoped projection of raw
  provider identifiers;
- raw provider payloads containing content fields, proving those fields do not
  enter the snapshot or journal;
- bounded attention overflow, a fixed-size replay index independent from the
  journal horizon, recoverable split writes before events or runtime changes,
  runtime-generation/header binding, missing/swapped replay fail-safe behavior,
  repeated-hook semantic idempotency, additive-field preservation, private
  quarantine, deterministic current-runtime diagnostics, and mode-0600
  snapshot, replay, journal, diagnostic, and lock files;
- dry-run, additive apply, repeated apply, repair, and owned-entry-only removal
  for all three provider configs, including Codex notify absence/ownership and
  user-owned notify conflict preservation.

The live release probe uses a no-content marker turn for each installed provider
after the released binary is installed. Retained evidence records only provider
version, event names, phase/revision changes, and pass/fail status.

## Setup selection and failure behavior

No audited provider offers an invocation-scoped hook flag that covers all
interactive sessions started by agent-session. The selected mechanism is the
third preference: explicit `activity setup --dry-run`, followed by an additive,
idempotent merge into the provider's user config. Existing hook arrays and
unrelated config keys are preserved; removal deletes only the exact
agent-session-owned command entries. Setup refuses an observed concurrent
source-file change instead of replacing the newer configuration.

Codex setup owns two distinct files. Hooks remain an additive JSON merge in
`~/.codex/hooks.json`. Completion uses the provider's singular top-level TOML
`notify` argv in `~/.codex/config.toml`. Setup inserts only
`["agent-session", "activity", "notify", "--agent", "codex"]`, recognizes that
exact argv idempotently, and removes only that exact argv. Any other `notify`
value is user-owned: dry-run/apply/repair return a content-free conflict before
mutating the hooks file. Apply/repair/remove parse and plan both files before
either mutation; a guarded second-write failure restores the first write, while
a rollback race surfaces an explicit error naming both metadata-only paths. The
CLI never shells or retains a downstream command.

Claude setup adds exact `AskUserQuestion` matcher groups for `PreToolUse`,
`PostToolUse`, and `PostToolUseFailure` while retaining the general PostToolUse
progress hook. Claude Code deduplicates identical matching command handlers, so
an AskUserQuestion completion is ingested once even though the exact and general
PostToolUse groups both match.

Provider hooks and the Codex completion notification invoke the local binary
without network access. They no-op when
`AGENT_SESSION_ID` or `AGENT_SESSION_RUNTIME_ID` is absent, accept at most 64 KiB
of JSON, project only allowlisted metadata into a normalized event, and always
exit successfully to the provider. Telemetry failure can make state unknown or
stale, but cannot block a prompt, tool call, approval, or completion.

The connection-scoped `provider-prompt.v1` title channel remains independent.
Its prompt text, attach events, reconnect behavior, queue drops, and event ids
are never lifecycle input and never enter activity persistence.
