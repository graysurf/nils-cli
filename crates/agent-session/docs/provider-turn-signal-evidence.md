# Provider turn signal evidence

## Status and scope

This report freezes the provider lifecycle evidence used by
`agent-session.turn-event.v1` and `agent-session.turn-state.v1`. It was audited
on 2026-07-11 against Codex CLI 0.144.1, Claude Code 2.1.206, Hermes Agent
0.18.2, and the agent-session 1.21.17 implementation baseline. The exact
attention addendum was audited on 2026-07-15 against Codex CLI 0.144.3 and
Claude Code 2.1.210. The support floors are deliberately the
oldest versions directly covered by this audit, not guesses about earlier
releases.

The fixtures under `tests/fixtures/activity/` contain lifecycle identifiers and
event names. The dedicated Hermes approval fixtures additionally freeze the
0.18.2 shell `_serialize_payload` envelope and carry explicit discarded
sentinel values for the matching metadata fields required by the provider
contract; tests prove those values do not survive normalization.
Prompt text, assistant output, tool input/output, commands, transcript paths,
credentials, and terminal content are removed before the normalized event is
created.

## Evidence sources

- [Codex hooks](https://learn.chatgpt.com/docs/hooks) documents concurrent
  matching hooks, additive discovery across active config layers, trust review,
  `turn_id`, and the `UserPromptSubmit`, `PermissionRequest`, `PostToolUse`, and
  `Stop` payloads.
- [Codex notifications](https://learn.chatgpt.com/docs/config-file/config-advanced#notifications)
  documents the `agent-turn-complete` notify surface and its `thread-id` and
  `turn-id`. Setup installs the exact agent-session argv when `notify` is absent
  or already owned. A bounded safe singular user argv is composed through the
  owned helper only when later removal can restore the complete config bytes;
  unsafe, oversized, nested-forward, or non-reversible values remain unchanged
  and block automatic setup. Codex appends its full notification JSON to that
  argv; agent-session discards content after parsing and never persists it, but
  the provider-supplied argv is transiently visible to same-host process inspection.
- [Codex app-server](https://learn.chatgpt.com/docs/app-server) documents
  structured turn failures and server requests. The audited 0.144.1/0.144.3
  schemas and live Unix
  WebSocket probe expose `Turn.status`, `Turn.error.codexErrorInfo`, the
  `error` notification, `account/rateLimits/read`, `turn/start`, typed
  `RequestId` (`string | int64`), five admitted blocking request methods, and
  `serverRequest/resolved`.
- [Claude Code hooks](https://code.claude.com/docs/en/hooks) documents parallel
  matching hooks, exact `AskUserQuestion` matching, shared `tool_use_id` on
  `PreToolUse`/`PostToolUse`/`PostToolUseFailure`, `PermissionRequest` without
  that id, notifications including `idle_prompt`, and `Elicitation` /
  `ElicitationResult` with an optional `elicitation_id` on both callbacks.
- [Hermes hooks](https://github.com/NousResearch/hermes-agent/blob/main/website/docs/user-guide/features/hooks.md)
  documents `pre_llm_call`, `post_llm_call`, `pre_approval_request`,
  `post_approval_response`, shell-hook consent, and synthetic hook tests. The
  installed source was also checked because Hermes is not an agent-session-owned
  stable interface.

## Support matrix

| Provider | Audited floor | Classification | Start | Completion | Attention | Failure | Setup |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Codex | 0.144.1 baseline; exact-attention versions 0.144.1 and 0.144.3 | supported; exact attention and usage failure require an audited agent-session app-server v2 runtime | `UserPromptSubmit`, observed | matching `agent-turn-complete`, authoritative; raw `Stop` remains journal evidence only | managed protocol authority: typed exact request/resolution; raw/unmanaged hook authority: `PermissionRequest` conservative latch | live app-server `failed` + `usageLimitExceeded`, authoritative; raw TUI remains unavailable | additive hooks/notify plus capability-probed private Unix app-server runtime for fresh sessions |
| Claude Code | 2.1.206 baseline; Elicitation audit 2.1.210 | partial; usage failure supported; Elicitation exact only when both callbacks carry the same non-empty id | `UserPromptSubmit`, observed | `idle_prompt`, observed; raw `Stop` is journal evidence only | exact `AskUserQuestion`; conditional exact `Elicitation`; `PermissionRequest`/notification conservative latch | structured `StopFailure.error`, authoritative; only `rate_limit` can arm auto-resume | additive merge into `~/.claude/settings.json` |
| Hermes | 0.18.2 | supported | `pre_llm_call`, observed | successful non-interrupted `post_llm_call`, authoritative | non-empty shell `extra.tool_call_id` projects to exact pre/post correlation; missing/empty-id tuple fallback remains conservative | runtime/fallback only | additive merge into `~/.hermes/config.yaml`; Hermes consent remains mandatory |

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
approval. Raw or unmanaged runtimes therefore use agent-session-owned opaque
attention ids and remain latched. Unrelated progress never clears them.

An audited managed app-server runtime instead preselects protocol attention
authority. The metadata-only proxy admits
`item/commandExecution/requestApproval`, `item/fileChange/requestApproval`,
`item/permissions/requestApproval`, `item/tool/requestUserInput`, and
`mcpServer/elicitation/request`, then clears only the same typed request id on
`serverRequest/resolved`. MCP elicitation `form` and `openai/form` map to
clarification, while `url` maps to authentication; an unknown or malformed mode
fails closed. JSON integer `1` and string `"1"` remain distinct in the bounded
in-memory pending table. Each occurrence receives a fresh opaque correlation
token, so sequential provider id reuse remains exact without retaining the raw
id in the activity snapshot, journal, replay index, or public state. The runtime
injects its authority into tmux; the installed generic `PermissionRequest`
command checks that environment before invoking the helper, which also protects
against an older helper during rollback. Protocol authority is selected only
when that exact guarded command is present and no second direct unguarded
reporter can bypass it; an unguarded or missing command keeps hook authority even
when app-server transport is available. Missing or
conflicting authority, wrong-turn or malformed recognized metadata, malformed
or lost proxy transport, a projection queue gap, or an unexpected permission
hook writes a durable generation-scoped unhealthy marker and degrades activity
to `unknown` until a new runtime generation. A dedicated health fence orders the
pending marker against activity commits and the durable auto-resume claim; the
stable mirror is then reconciled under the session-record lock. The marker
retains a monotonic public revision/timestamp and fails closed if unreadable or
if a parseable state is not itself a valid runtime-owned `unknown` state.
Authority never switches within a runtime.

The same runtime injection passes the daemon's current `PATH` to each new tmux
session. This is deliberately session-scoped: a tmux server may outlive the
serve daemon and retain its older global environment, but provider hooks in a
new pane must resolve the staged `agent-session` helper selected by the current
launcher.

For fresh agent-session-managed sessions, bounded version/help probes admit
only an explicit app-server transport allowlist plus Unix-listen support before
launching `codex app-server` and connecting the visible TUI through a private
metadata-projecting Unix WebSocket bridge. The bridge observes the exact TUI
connection and forwards its frames unchanged; bounded background projection
retains no message content, and any projection gap launch-fences and disables
an existing claim without closing the TUI. Direct TUI thread/turn creation is
serialized against manual-input cancellation before it reaches app-server.
The visible TUI creates the thread without a synthetic shell or model turn.
The daemon holds a second control connection for usage reads and continuation
submission. Both paths require the same bound thread, persist only a private
SHA-256 binding, and fail closed on mismatch. A
matching non-retrying `error` notification followed by `turn/completed`, or a
terminal failed Turn carrying the same structured error, maps
`usageLimitExceeded` to `usage_exhausted`. Wrong threads, wrong turns,
non-terminal statuses, retrying errors, reordered partial envelopes, unknown
values, malformed usage snapshots, and monitor gaps cannot arm or submit. Raw
thread/turn ids are runtime-scoped SHA-256 projections before persistence;
human error text, prompts, output, and auth/account payloads are discarded.

The persisted rollout history is not used to recover failures because the live
probe demonstrated that a failed quota turn can later appear as completed with
no error. Continuation therefore uses the same bound live connection and is
successful only after `turn/start` acknowledges a new turn id. Unknown outcomes
are terminal and never replayed.

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

`Elicitation` and `ElicitationResult` are installed as an additional exact-or-
conservative pair. When both callbacks expose the same non-empty
`elicitation_id`, the adapter projects it into the same v1 exact-correlation
namespace; form mode maps to clarification and URL mode maps to authentication.
Because the provider contract makes the id optional, an identifier-less request
remains a conservative latch and an identifier-less result is ignored. Message,
URL, requested schema, response content, MCP server name, and raw id never enter
the normalized event.

The sanitized Claude Code 2.1.210 form canary observed both callbacks but no
`elicitation_id` on either side, so the installed form path is classified
conservative. A separate URL canary did not observe either callback and is
classified unverified-conservative. These installed results do not weaken the
conditional adapter: a future callback pair is admitted as exact only when the
same non-empty id is actually present; no version floor alone claims support.

`PermissionRequest` explicitly omits `tool_use_id`, even though later tool events
include one. A separate installed-version permission/progress probe reproduced
that asymmetry. Permission notifications also omit a stable request id. Those
signals remain uncorrelated conservative latches cleared only by a proven
completion, a new turn, or a runtime boundary; later progress may prove work is
continuing but never proves the request was answered.

`PermissionRequest` runs when a permission dialog is about to be shown, and a
`permission_prompt` notification likewise reports an actual prompt. Current
Claude emits both for one dialog, however, and the notification has neither a
request id nor a distinct resolution. The managed setup therefore owns
`PermissionRequest` and does not install the duplicate notification reporter;
user-owned or previously configured notification reporters still normalize
conservatively.

`AskUserQuestion` permission shadows are also ignored because exact
PreToolUse/PostToolUse correlation already owns that interaction. Other
permission requests remain authoritative over the payload's `permission_mode`
hint, including the `bypassPermissions` root/home deletion circuit breaker.

`StopFailure` exposes a documented finite `error` enum. The adapter treats that
enum as authoritative failure classification, maps it to the metadata-only
`failure_reason` allowlist, and discards `error_details` and
`last_assistant_message`. Only `error == "rate_limit"` maps to
`usage_exhausted`; authentication, organization, billing, invalid request,
server, max-output-token, and unknown controls remain non-resumable. The
sanitized `auto-resume-failures.jsonl` fixture freezes this matrix.

### Hermes

The installed `post_llm_call` fires only after a successful final response and
does not fire for interruption, so it is authoritative completion at the
audited version. The 0.18.2 shell serializer places approval kwargs under
`extra`, emits an empty top-level `session_id` for this callback, and carries
the same non-empty `tool_call_id` on pre/post. The adapter reads only allowlisted
extra fields, falls back to non-empty `extra.session_key`, and projects the
tool-call id as an exact runtime-scoped correlation. Identical commands with
different tool-call ids therefore clear independently and out of order;
the event kind and projected tool-call id also derive a stable event id in the
runtime replay index, so exact callbacks stay idempotent across interleaving,
elapsed time, clearing, restart, and bounded journal eviction. Missing, null,
or empty tool-call ids
retain the older tuple fallback over `command`, `description`, `pattern_key`,
`pattern_keys`, `session_key`, and `surface`. That tuple is canonicalized only
in memory and projected by SHA-256; duplicate-identical fallback concurrency
remains conservatively latched until authoritative completion, a new turn, or a
runtime boundary. Raw kwargs never persist. Missing, malformed, or undocumented
response choices fail open with sanitized diagnostics and do not clear.

## Concurrency, continuation, and privacy probes

The executable fixtures cover:

- two concurrent attention requests, correlated one-by-one clearing, and a
  metadata-only `pending_count`;
- the frozen Hermes 0.18.2 shell envelope with nested `extra`, empty top-level
  session id, exact tool-call replay/ordering, missing/empty-id fallback, stale
  runtime rejection, sanitized diagnostics, raw-field non-persistence, restart,
  and bounded journal eviction;
- exact AskUserQuestion request/success/failure correlation and independent
  clearing alongside unrelated generic attention;
- conditional exact Claude Elicitation form/URL correlation, identifier-less
  conservative fallback, setup parity, and content-field rejection;
- all five admitted Codex server-request methods, typed integer/string id
  separation, per-occurrence tokens with sequential id reuse, MCP mode mapping,
  concurrent `2 -> 1 -> 0` clearing before completion, idempotent unmatched or
  repeated resolution, queue/shape bounds, wrong-turn rejection, authority
  suppression, source-guard capability selection, transport-loss and malformed-
  frame degradation, stable marker revision across unrelated record updates,
  invalid/nondegraded-marker rejection, held-session-lock authority breach,
  guarded-plus-unguarded source rejection, and no same-runtime recovery;
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
  for all three provider configs, including Codex notify absence/ownership,
  reversible user-owned argv composition, literal no-shell forwarding, bounded
  downstream hangs/failures, and unsafe/recursive conflict preservation.
- `--repair --dry-run` emits a content-free Codex notification preview. Safe
  foreign argv that cannot satisfy byte-exact removal is identified only by
  argument count and SHA-256 of compact JSON plus one LF; apply remains blocked
  and both config files remain byte-identical. The preview also emits a plan
  digest over the exact current and proposed bytes of both Codex files; repair
  requires the same digest and rejects missing or stale review evidence before
  either write.

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
exact argv idempotently, and removes only that exact argv. A safe user-owned
string argv is encoded into the owned command only after a simulated removal
reproduces the full original TOML bytes. The helper executes that argv directly
without a shell, appends the provider payload literally, suppresses child I/O,
and kills the child process group after two seconds. A depth marker and nested
forward-flag rejection prevent recursive fan-out. Notification ingestion uses a
non-blocking activity lock so contention cannot postpone the preserved notifier;
on contention, a detached `activity event` retry receives only the normalized
metadata through stdin and waits up to five seconds for that lock. It clears the
diagnostic after durable success or records a sanitized timeout/ingest code on
terminal failure, preventing unbounded blocked workers while preserving the
authoritative completion across transient contention. Removal decodes and
restores the original argv. Unsafe, oversized, non-string, nested-forward, or
non-reversible
values return a content-free conflict before mutating the hooks file.
For repair review, `activity setup --agent codex --repair --dry-run` keeps both
files untouched and reports current/candidate mode, exact-reversal status,
argument count, and a compact-JSON-plus-LF SHA-256 without exposing argv. A
non-reversible serialization remains blocked; the preview does not authorize or
perform normalization. The preview's separate plan digest binds the exact
current and candidate bytes for both files. Applying repair requires it via
`--expected-preview-digest`; any missing, malformed, or stale digest fails
before mutation.
Claude and Hermes do not have this two-file reviewed-plan contract, so the same
combined flags reject with `provider-repair-preview-unsupported`; ordinary
dry-run and repair remain separate supported actions for those providers.
Apply/repair/remove parse and plan both files before either mutation; a guarded
second-write failure restores the first write, while a rollback race surfaces an
explicit error naming both metadata-only paths.

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
