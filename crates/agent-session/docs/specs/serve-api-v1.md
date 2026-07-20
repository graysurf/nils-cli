# Serve API v1

This specification owns the current HTTP and WebSocket surface exposed by
`agent-session serve`. The crate README is a non-normative product entrypoint;
the operations runbook owns deployment procedure.

## Route ownership

This index covers every route literal registered by the daemon. Braced
comma-separated route segments below are exact alternatives, not wildcards.
`Bearer + capability` means the server bearer plus
`X-Agent-Session-Capability`.

| Method and path | Authentication | Canonical contract |
| --- | --- | --- |
| `GET /healthz` | Open | This specification |
| `GET /sessions` | Open | This specification |
| `POST /sessions` | Bearer | This specification |
| `GET /codex/accounts` | Bearer | This specification |
| `GET /activity/events` | Bearer | [Activity stream v1](activity-stream-v1.md) |
| `GET /usage` | Open | This specification |
| `GET /workdirs` | Bearer | This specification |
| `GET /repos/remote-url` | Bearer | This specification |
| `GET /sessions/{id}/glance` | Open | This specification |
| `GET /sessions/{id}/work-context/v1` | Bearer | [Coordination HTTP coverage](session-coordination-v1.md#http-coverage) |
| `POST /sessions/{id}/work-context/check/v1` and `POST /coordination/work-context/check/v1` | Bearer; session capability optional where supported | [Coordination HTTP coverage](session-coordination-v1.md#http-coverage) |
| `POST /sessions/{id}/work-context/{claim,renew,release,admit,complete,reconcile}/v1` | Bearer + capability | [Coordination HTTP coverage](session-coordination-v1.md#http-coverage) |
| `GET /sessions/{id}/broker/v1` and `POST /sessions/{id}/broker/{adopt,reconcile}/v1` | Bearer; recovery proof is in the request body | [Coordination HTTP coverage](session-coordination-v1.md#http-coverage) |
| `GET and POST /sessions/{id}/messages/v1` | Bearer + capability | [Coordination HTTP coverage](session-coordination-v1.md#http-coverage) |
| `GET /sessions/{id}/messages/{message_id}/v1` | Bearer + capability | [Coordination HTTP coverage](session-coordination-v1.md#http-coverage) |
| `POST /sessions/{id}/messages/{message_id}/{ack,reply}/v1` | Bearer + capability | [Coordination HTTP coverage](session-coordination-v1.md#http-coverage) |
| `GET /sessions/{id}/messages/{message_id}/wait/v1` | Bearer + capability | [Coordination HTTP coverage](session-coordination-v1.md#http-coverage) |
| `GET /sessions/{id}/buffer` | Open | This specification |
| `POST /sessions/{id}/{send,prompt,prompt/v2,resume}` | Bearer | This specification |
| `GET /sessions/{id}/maintenance` and `POST /sessions/{id}/maintenance/actions` | Bearer | [Session maintenance v1](session-maintenance-v1.md#authentication-and-endpoints) |
| `PUT /sessions/{id}/account` | Bearer | This specification |
| `GET /sessions/{id}/auto-resume` | Open | This specification |
| `PUT and DELETE /sessions/{id}/auto-resume` | Bearer | This specification |
| `POST /sessions/{id}/attachments` | Bearer | This specification |
| `GET /sessions/{id}/attach` | Bearer | This specification |
| `PATCH and DELETE /sessions/{id}` | Bearer | This specification |

## Endpoint contracts

`agent-session serve` exposes the session control plane over loopback HTTP for a per-machine edge (e.g. the agent-console
web console). It builds its own tokio runtime and reuses the synchronous lifecycle functions via `spawn_blocking`, so there
is no second state model.

- `GET /healthz`, `GET /sessions`, `GET /sessions/{id}/glance?tail=N` — reads, open on loopback. `GET /sessions`
  additively reports `data.observed_at`, sampled from daemon time after the returned session state is assembled, plus
  `data.agent_profiles` containing only ready server-owned
  `{ id, label, agent, provider_resume_import_supported }` launch-profile
  summaries. `data.capabilities.profile_resume_import` advertises support for
  selecting one of those safe ids during provider import; executable paths,
  configuration roots, readiness commands, and environment remain private.
  `data.capabilities.managed_resume_command` remains false until an unqualified
  CLI resume can revalidate the daemon's active profile registry and readiness
  contract; consumers must copy the provider session ID during that skew.
  Sessions report
  `running`, `stopped`, or `unknown` live status plus a boolean `resumable` field and best-effort `repo_name` derived from
  the recorded `cwd`. New interactive records also expose optional
  `runtime_started_at`, `turn_state`, `last_prompt`, and `startup`; a profiled
  session also
  exposes its safe `agent_profile` id. When a known profile drift would make a
  stopped session fail resume, the daemon sets `resumable: false` plus one
  bounded `resume_blocked_reason` code. Old records and daemons omit the
  additive fields.
  `data.capabilities.last_prompt` advertises the list `last_prompt` preview: the
  most recent user prompt for a running Codex/Claude session, resolved on demand
  from the provider transcript so it reflects prompts submitted through any input
  path (web console, SSH/Termius, or raw `tmux attach`). The text is returned in
  the response only and is never logged or persisted by the daemon; it is omitted
  when no recent prompt falls within the bounded tail window read.
  `startup` is the metadata-only `agent-session.startup.v1` projection shared by
  create, list, and glance responses. Its state is `starting`, `ready`, or
  `failed`; its bounded stage is `record`, `tmux`, `runtime`, `app_server`,
  `proxy`, `provider_client`, or `initial_connection`. Failed projections add
  an RFC 3339 `occurred_at` captured from the private failure marker, boolean
  `retry_safe`, one reviewed message, and one
  allowlisted code: `runtime-helper-unavailable`, `agent-binary-unavailable`,
  `working-directory-unavailable`, `terminal-runtime-create-failed`,
  `app-server-start-failed`, `proxy-start-failed`, `provider-client-exited`,
  `provider-configuration-rejected`, `startup-timeout`, or `startup-exited`.
  Managed launchers retain only bounded stage/failure markers in the record and
  keep stderr in a private, tail-capped local diagnostic file for startup failures
  and non-zero Codex provider-client exits after readiness; clean exits discard it.
  The local `agent-session logs <id>` command can read that diagnostic, but it is
  never copied into the session projection. Raw argv, environment, provider responses,
  stderr, prompts, and filesystem paths are never copied into the projection. A
  record that reached `ready` keeps that
  state after an ordinary later stop, so consumers must not relabel normal
  session termination as startup failure. A resume starts a fresh startup
  lifecycle for its new runtime generation; synchronous launch rollback restores
  the prior projection and private diagnostic artifacts. A leftover resume
  backup from an interrupted process blocks another resume before mutation so
  the only copy of prior diagnostic state is not silently discarded.
- `GET /usage` — read-only provider usage report, open on loopback. The serve
  envelope contains `data.usage.schema_version: "agent-session.usage.v1"` and
  provider entries for Codex and Claude. Provider readers are bounded by
  `AGENT_SESSION_USAGE_TIMEOUT_MS` (default 45000). The Claude reader forwards
  that budget to its nested probe with five seconds reserved before the outer
  hard deadline when the budget is at least six seconds; shorter budgets use a
  one-second inner minimum. A positive
  `CLAUDE_PROMPT_SEGMENT_CLAUDE_TIMEOUT_SECONDS` override is kept when it fits
  and clamped when it exceeds that inner budget. Timed-out helpers are killed as
  a process group before their output pipes are read. Provider readers preserve
  partial success, preserve reset timestamps as `reset_at_epoch` epoch seconds
  plus textual `reset_at` when supplied by the helper, and redact tokens, local
  auth paths, and private account identifiers from scoped error messages.
  Failed providers may include the additive provider-neutral `reason_code`
  contract (`auth_required`, `auth_expired`, `billing_past_due`,
  `subscription_inactive`, `organization_disabled`, `permission_denied`,
  `rate_limited`, `service_unavailable`, `timeout`, or `unknown`) copied only
  from the helpers' allowlisted structured field.
- `GET /repos/remote-url?cwd=...` — authenticated repository lookup. `cwd` is
  required. The ordinary serve envelope returns `data.url` as a normalized
  credential-free HTTPS URL for any parseable Git origin host, or `null` when
  the directory has no supported remote.
- `GET /sessions/{id}/buffer` — open on loopback. Returns the tmux server's
  latest global clipboard buffer after using `id` only to verify that the
  requested session exists. The buffer is not scoped to that session.
- Every session view additively includes `auto_resume` using
  `agent-session.auto-resume.v1`. `GET /sessions/{id}/auto-resume` reads that
  status and is open on loopback; `PUT` with `{ "enabled": true|false }` opts a
  supported session in or out, and `DELETE` durably cancels pending work. Both
  mutations require the bearer token. Claude Code is supported through its
  authoritative structured
  `StopFailure.error == "rate_limit"` signal. Fresh interactive Codex sessions
  created through the serve API are also supported when the installed CLI
  capability probe selects the app-server v2 runtime. The daemon consumes the
  live metadata-only protocol
  and requires an exact bound thread/turn with terminal `status == "failed"`
  plus `codexErrorInfo == "usageLimitExceeded"`. Standalone/raw Codex TUI,
  imported Codex conversations, and resumed pre-app-server Codex sessions remain
  unsupported. Terminal text and assistant output are never treated as
  authority.
  The response object is `{ schema_version, supported, enabled, state,
  scheduled_at?, failure_reason? }`. `scheduled_at`, when present, is an
  RFC 3339 timestamp. The v1 `state` values are `disabled`, `enabled`, `armed`,
  `scheduled`, `checking`, `resumed`, `cancelled`, `transient_failure`, and
  `terminal_failure`. The allowlisted `failure_reason` values are
  `state_unavailable`, `manual_input`, `usage_unavailable`,
  `usage_window_not_exhausted`, `exhausted_reset_unavailable`,
  `session_state_changed`, `submission_outcome_unknown`, `provider_unsupported`,
  and `scheduler_error`. Consumers must preserve
  the object but render unknown future state or reason values as a safe generic
  unavailable/failure condition; they must not infer permission to submit from
  an unknown value. `scheduled_at` is present only while a reset wake is
  scheduled, and `failure_reason` is present only when the latest transition
  records a safe operational reason.
- The daemon owns scheduling. It waits for the latest reset among all exhausted
  windows, adds bounded deterministic jitter, re-collects usage at wake, checks
  that the session activity revision is still eligible, and durably claims the
  submission before sending one fixed product-owned continuation message.
  Restart recovery scans pending records; duplicate events/ticks cannot submit
  twice, cancellation is serialized against wake-up, and bounded retry ends in
  an observable terminal failure.
  Claude usage checks use the existing provider helper. Codex app-server
  sessions use `account/rateLimits/read` on their bound control connection and
  submit the continuation with `turn/start`; only a response carrying the
  acknowledged turn id counts as success. A timeout or disconnect after the
  durable claim is terminal `submission_outcome_unknown` and is never replayed.
- `GET /workdirs?q=...&limit=N` — authenticated read; searches only the default operator roots (`$HOME/Project` and
  `$HOME/.config`) with bounded depth, count, and elapsed-time limits. Add `git_only=true&exclude_worktrees=true` for
  the curated project picker: only primary git working trees are returned, ordered by most-recent session cwd usage
  (`last_used`) and then name/path.
- `GET /codex/accounts` — authenticated nickname-only account inventory from
  the configured host credential broker. The additive `readiness` projection
  reports whether the installed Codex version meets the minimum app-server
  floor and currently advertises Unix listen support, with only a canonical
  provider version and stable safe reason code. Newer stable Codex releases are
  accepted by capability instead of an exact-version allowlist; exact protocol
  attention remains limited to explicitly audited versions and otherwise falls
  back to hook authority. The response never contains access tokens, ChatGPT
  account ids, auth paths, or broker diagnostics.
- `GET /activity/events` — authenticated metadata-only SSE for activity snapshots and heartbeats. Events carry a daemon-boot
  `stream_id` and increasing `sequence`; `Last-Event-ID` enables count-and-byte-bounded replay, while stale/foreign cursors
  and lagged consumers receive a reset. Concurrent subscribers are daemon-capped and saturation returns a stable
  polling-fallback error. Provider hooks only update durable local activity files; a daemon filesystem watcher publishes changes
  through bounded nonblocking queues. Payloads and SSE frames are serialized once and shared; an oversized snapshot emits a
  transition-only content-free `oversized_snapshot` reset that requires immediate polling rather than entering replay or
  broadcast retention. Notification storms converge through a trailing quiet debounce and refresh starts spaced by an explicit
  minimum cadence. Backend rescan flags force a full refresh; sessions-root loss re-arms a replacement recursive watcher or
  degrades the stream. Watcher or snapshot-source failures send existing streams one reset before closing, stop
  heartbeats, and make new stream requests return the polling-fallback error. The stream and `/sessions` share one snapshot
  source; degraded resets use unique sequences while retaining the last successful snapshot observation anchor, and typed
  projection omits absent nested leaves while preserving session-level `turn_state: null`. The existing `/sessions` read remains
  the old-peer and gap-reconciliation path. The exact
  wire/privacy contract is [activity-stream-v1](activity-stream-v1.md).
- `POST /sessions` (create), `PATCH /sessions/{id}` (title update), `POST /sessions/{id}/send`,
  `POST /sessions/{id}/prompt`,
  `POST /sessions/{id}/resume`,
  `PUT /sessions/{id}/account`,
  `PUT /sessions/{id}/auto-resume`, `DELETE /sessions/{id}/auto-resume`,
  `POST /sessions/{id}/attachments?filename=...`, `DELETE /sessions/{id}` — writes, require a bearer token.
- `POST /sessions/{id}/prompt` submits exact prompt text through a supported provider control plane. The compatibility route accepts
  `{ "text": "...", "expected_session_incarnation": "launch-id" }`; the incarnation is optional for older clients, and
  a new daemon validates it against the authoritative runtime under the session-record lock before provider dispatch.
  Clients that require a cross-version fence use `POST /sessions/{id}/prompt/v2`, which requires both fields, rejects
  unknown fields, and is absent from older daemons so they fail before provider dispatch. A replacement returns HTTP 409
  `session-incarnation-conflict` without submitting. Success returns `submitted: true` plus the locked
  `session_incarnation`, while the provider turn id remains private. These mutations never send multiline text through
  terminal keys; unsupported or not-yet-ready sessions fail closed.
- `PUT /sessions/{id}/account` accepts
  `{ "account": "nickname", "expected_session_incarnation": "launch-id" }`
  only for
  an idle, serve-managed Codex app-server runtime. It does not recreate tmux or
  resume the provider conversation. The daemon resolves credentials through the
  host broker, sends Codex `account/login/start` with `chatgptAuthTokens`, and
  returns only after the durable binding is `bound` to the current launch id.
  Prompt, terminal-input, and auto-resume submission paths fail closed while a
  selected binding is `pending` or `failed`, so the next accepted prompt uses
  the newly selected account.
- `POST /sessions` normally creates a fresh session from `agent`, optional
  `cwd`, `title`, `title_state`, `id`, `prompt`, `coordination_mode`, and
  `agent_args`. `coordination_mode` accepts `advisory`, `enforce`, or `off` and
  defaults to `advisory`. A fresh create may add an advertised `agent_profile`; the id
  must match the supplied base `agent` and be ready when the request arrives.
  A profile whose summary reports `provider_resume_import_supported: true` may
  also be selected with `provider_resume_id`; discovery is then confined to
  that profile's provider root and never falls back to the daemon process's
  base root. The server resolves the profile's executable, provider config root, readiness command, and
  auto-resume capability; callers cannot submit or override those fields. A
  fresh Codex create may additionally provide
  `codex_account`; when a prompt is also present, the daemon completes account
  binding before submitting that prompt. `codex_account` is rejected for other
  providers and for provider-import mode. When `provider_resume_id` is present (alias: `resume_id`), the daemon imports an existing Codex or
  Claude provider conversation instead: it resolves the original cwd from the selected local provider history, persists exact
  `provider_resume` metadata, and starts tmux with the canonical resume command. In resume-id mode, omit `cwd`, `prompt`,
  and `agent_args`; invalid, missing, ambiguous, or unsupported provider ids return structured errors.
  For a fresh serve-managed Codex session, `agent-session` probes bounded
  `codex --version` and `codex app-server --help` process groups. App-server
  transport requires Codex `>= 0.144.1` and advertised Unix `--listen` support.
  Exact protocol-attention authority is audited only for Codex `0.144.1` and
  `0.144.3`; newer transport-compatible versions fall back to hook authority.
  An eligible CLI is launched as a remote TUI over a private short socket below an
  owned, non-symlinked mode-`0700` `XDG_RUNTIME_DIR`; otherwise auto mode
  degrades to the existing raw TUI. `AGENT_SESSION_CODEX_RUNTIME=raw` forces
  the fallback and `AGENT_SESSION_CODEX_RUNTIME=app-server` requires both the
  same capability probe and a private Unix socket. Standalone `agent-session
  start` remains raw because no serve daemon owns its control connection.
  The remote TUI connects through a private mode-`0600` WebSocket bridge to the
  private app-server socket. The bridge forwards frames unchanged, observes the
  exact TUI connection's structured lifecycle metadata through bounded
  background projection, and discards message content after in-memory
  reduction. Projection loss disables an existing claim without interrupting
  the TUI transport. Direct TUI thread/turn creation is launch-fenced against
  auto-resume before forwarding; a busy state lock rejects only that request so
  the user can retry. Control-plane Enter injection (either a named Enter key
  or the raw CR/LF frame emitted by an attached terminal) already performs the
  same cancellation while holding the lifecycle lock. A live proxy advertises
  this coordination capability with a private, launch-bound, file-locked
  marker; older live proxies reject HTTP submission, while attached input
  disconnects without mutating auto-resume and requires session recreation.
  Immediately before the submitting tmux operation, the sender opens a bounded
  manual-input section. If ordinary
  proxy cancellation reports Busy, only a valid `turn/start` for the exact
  bound thread may hold that section's gate while it is forwarded. Gate teardown
  completes before the sender releases the lifecycle lock and removes the
  marker, preventing stale authorization of another lock holder. These markers
  store no prompt, terminal content, or raw thread id; malformed, expired,
  dead-process, and replacement-runtime state fails closed. The visible TUI
  creates the fresh thread;
  neither the bridge nor the control client synthesizes a shell or model turn.
  A separate daemon control connection reads usage and submits a continuation
  on that bound thread. Only a mode-`0600` SHA-256 thread binding is persisted,
  so reconnects fail closed on a mismatch and raw thread ids are never stored.
  The bridge remains with the tmux runtime across daemon restarts. Runtime paths
  are namespaced by state and launch identity; delete and launch failure
  validate and remove the app-server socket, bridge socket, and marker paths.
  A selected account is persisted only as nickname, revision, public state
  (`unsupported`, `unbound`, `pending`, `bound`, or `failed`), and applied
  launch id. On daemon reconnect or stopped-session resume, the new control
  connection re-applies that nickname before accepting input. Codex
  `account/chatgptAuthTokens/refresh` with reason `unauthorized` triggers one
  forced broker refresh and the same durable pending/bound transition; failure
  becomes visible and remains fail closed.
- Session reads include a monotonic `title_revision`. `PATCH /sessions/{id}` may include
  `expected_title_revision`; a stale value returns `409 title-revision-conflict` without changing the title.
  Upgraded clients also send the runtime's random `session_incarnation` as `expected_session_incarnation` and the
  observed title as `expected_session_title`. A different runtime UUID rejects delayed requests aimed at a
  deleted-and-recreated or resumed session with `409 session-incarnation-conflict`; exact title comparison rejects
  changes made by older daemons that do not advance the revision with `409 title-state-conflict`.
  `expected_session_created_at` remains accepted for transitional clients. Omitting these fields preserves
  unconditional updates for backward-compatible clients.
- Session create and PATCH requests may provide `title_state` instead of deriving semantics from the rendered title.
  Its shape is `{ "topic": string|null, "topic_source": "none"|"auto"|"user", "references": ["#123"],
  "activity": string|null }`. A `user` topic is client-owned and stable; an `auto` topic may be revised as the session
  converges; `none` requires a null topic. The daemon validates at most two numeric work-item references and renders the
  compatibility `title` as `<topic and references> - <activity>`, or as the only non-empty side when one side is absent.
  Supplying both fields requires an exact canonical match. Title-only compatibility writes remain accepted and clear
  `title_state`, so old clients never leave structured provenance attached to an unrelated title. Reads omit stale
  structured state if an older writer changed only the compatibility title.
  Session and glance responses advertise `title_state_supported: true` independently of whether that session already
  has structured state, allowing upgraded clients to migrate title-only records conservatively.
- Attachment upload uses a raw binary request body (not multipart), capped at 25 MiB. The daemon writes the file under the
  session's private `attachments/` directory with a sanitized filename and returns the remote path in the serve envelope.
  Empty or null titles clear the custom session title so clients can fall back to the session id.
- `GET /sessions/{id}/attach` — a WebSocket PTY attach: a `capture-pane` snapshot then a live byte stream from one
  daemon-owned `tmux pipe-pane` broker per session (binary frames, renderable by xterm.js). Concurrent clients fan out
  from the same bounded in-memory stream; disconnecting one client leaves the others live, while a lagging client is
  disconnected so it cannot stall tmux or other clients. The broker uses a private ephemeral FIFO and retains no
  interactive-session terminal bytes after the final client disconnects. Snapshot capture drains live output into a
  bounded handoff buffer and performs one bounded fresh-snapshot recovery if that buffer overflows. After handoff, a
  supervised per-client pump keeps draining broker output independently of provider discovery, input, and resize work;
  a normal broker close drains already accepted frames under the WebSocket send bound, while lag/error teardown remains
  immediate. The client sends JSON control frames
  `{ "text": "...", "key": "enter", "keys": ["c-c"], "resize": { "cols": 80, "rows": 24 } }`. Token-gated; disconnect
  leaves the tmux session alive. Concurrent clients share the pane geometry; resize sequences are serialized and the
  last completed resize wins. A client may opt into authoritative Codex/Claude prompt events by sending
  `{ "subscribe": ["provider-prompt.v1"] }`. For a known, resumed, imported, or reconnected provider session, the daemon
  baselines the exact provider transcript at EOF. When a generation-1 fresh Codex/Claude runtime is still establishing its
  exact provider identity or transcript, the connection instead keeps a bounded, cancellable resolver alive, reloads the
  same launch's session metadata, and opens that fresh transcript from its beginning so the first prompt is not lost.
  Codex `UserPromptSubmit` hook metadata supplies the exact runtime-bound session identity; the pending attach path never
  promotes cwd/time history scans into beginning-of-transcript authority. Transcript discovery is shared by runtime across
  attach clients, is owned by the daemon across waiter cancellation, uses bounded exponential backoff, and admits at most
  four concurrent history scans. Active slots are never capacity-evicted; obsolete runtime keys and deleted sessions are
  evicted, and a fixed daemon-local entry cap bounds unrelated session churn. Reconnects
  baseline the revalidated cached exact source at EOF. Passive list/glance reads never persist heuristic Codex history
  into a live generation-1 runtime, and fresh Codex byte-zero recovery admits only `codex-user-prompt-submit-hook`
  identity. Explicit stopped-session resume may recover older provider history while holding the same per-session record lock for
  its complete resume/rollback transition. It
  replies with an `agent-session.attach.v1` `capability` text frame once resolution finishes, and only after that
  acknowledgement emits `prompt_submitted` events as bounded text frames; terminal snapshot/live output remains binary.
  The normative supported acknowledgement is:

  ```json
  {
    "schema_version": "agent-session.attach.v1",
    "type": "capability",
    "capability": "provider-prompt.v1",
    "supported": true,
    "provider": "codex",
    "prompt_max_bytes": 16384
  }
  ```

  `provider` is `"codex"` or `"claude"` when supported and `null` otherwise; an unsupported provider, unresolved exact
  transcript, unsafe transcript path, exhausted discovery budget, or expired fresh-runtime resolution returns the same
  object with `supported:false`. The fresh-runtime resolver is restricted to the original generation-1 launch identity:
  Codex must have had no provider identity when the client subscribed, and Claude must carry the daemon-generated
  `claude-explicit-session-id` capture method. Imported, resumed, replaced, and later-generation runtimes never enter the
  beginning-of-transcript path, so reconnect retains EOF/no-history behavior. While resolution is pending, terminal bytes,
  input, resize, and broker fanout continue independently; consumers may activate their bounded local fallback before the
  eventual acknowledgement.
  Clients that do not subscribe receive no event text frames. The normative event is:

  ```json
  {
    "schema_version": "agent-session.attach.event.v1",
    "type": "prompt_submitted",
    "event_id": "pp-opaque",
    "provider": "codex",
    "submitted_at": "2026-07-10T03:51:49Z",
    "text": "final provider-recorded prompt",
    "truncated": false
  }
  ```

  `event_id` is unique and opaque, `submitted_at` uses the provider timestamp when present (otherwise detection time), and
  `text` is UTF-8 bounded to `prompt_max_bytes`; `truncated` reports clipping. Events never contain transcript paths and are
  never logged or persisted by the daemon. Terminal and control queues are bounded: terminal frames receive bounded burst
  preference, while advisory prompt events may be dropped on saturation and must not delay terminal bytes. Consumers should
  retain their documented local fallback when capability is absent/false or an event does not arrive within its bounded
  fallback interval.

## Response and authentication

Ordinary JSON HTTP responses use the `cli.agent-session.serve.v1` envelope and
carry a `machine` identity (`--machine` / `AGENT_SESSION_MACHINE` / `--host` /
hostname) so an edge can aggregate several machines. The activity SSE stream
uses [activity-stream-v1](activity-stream-v1.md), while WebSocket attach uses
the binary/control and optional event frames documented above; neither
streaming transport uses the ordinary JSON envelope. Auth is a bearer token
(`--token-stdin`, `--token`, or `AGENT_SESSION_TOKEN`) on the activity stream plus all write and attach endpoints, compared without an early-exit
on the token bytes; when no token is configured (or it is empty) those endpoints fail closed (503). Prefer
`--token-stdin` for launcher integrations so token material does not appear in process arguments. It reads one trimmed
token from stdin, rejects empty input, rejects multiple newline-separated tokens, and rejects input over 8192 bytes.
`--token-stdin` conflicts with `--token`; both forms avoid printing token material in errors. Use a strong,
high-entropy token.

`--token` and `AGENT_SESSION_TOKEN` remain accepted compatibility inputs.
`--token` exposes the bearer to same-host process-argument inspection. The
environment form can be inherited by managed tmux/provider children because the
daemon does not currently scrub it before launch, which would grant those
children machine-operator authority. Deployments that create sessions must use
`--token-stdin` from a private, non-exported credential source.

## Codex account broker

Codex account switching is enabled by
`AGENT_SESSION_CODEX_ACCOUNT_BROKER`, whose value is a JSON argv array rather
than a shell command, for example
`["/opt/agent-console/bin/codex-account-broker"]`. The daemon invokes that argv
with either `list --format json`, `resolve --account <nickname> --format json`,
or `resolve --account <nickname> --force-refresh --format json`. Broker output
uses `agent-session.codex-auth-broker.v1`: list returns public `accounts`, while
resolve returns the exact nickname plus `access_token`, `chatgpt_account_id`,
and optional `plan`. Broker execution is process-group and time bounded with
bounded output. Credential values remain in memory only and are never added to
session documents or HTTP projections. Invalid configuration, malformed output,
duplicate or unsafe nicknames, timeout, and non-zero exit all fail closed.

## Launch profiles

Server-owned launch profiles are configured with
`AGENT_SESSION_LAUNCH_PROFILES`, a JSON array. For example:

```json
[{"id":"custom-claude","label":"Custom Claude","agent":"claude","agent_bin":"/opt/agent/bin/custom-claude","provider_config_dir":"/srv/agent/claude","readiness_args":["--check"],"auto_resume_supported":false}]
```

The daemon rejects malformed, duplicate, relative-path, or over-bounded
configuration at startup. A profile is advertised only when its executable is
an executable regular file, its optional provider config root is a directory,
and the optional readiness argv exits successfully within two seconds. The
readiness argv always runs against `agent_bin`; no shell is involved. Profile
discovery probes are single-flight; concurrent session-list reads share the
same in-flight result, while a later read probes fresh. Profile paths and readiness
details never enter HTTP responses. The safe id and any configured private
provider root persist with the runtime and its durable resume sidecar, so exact
binary and transcript discovery survive daemon restarts. Both daemon-managed
and standalone `agent-session resume <managed-id>` launches pin the persisted
provider root, when present, in the provider-specific environment before
invoking the durable launcher. The daemon resume endpoint additionally requires
the same id, base agent, executable, optional config root, auto-resume
capability, and readiness contract to remain present in the current server
registry; removing or changing a profile revokes resume through that endpoint.
Because standalone resume cannot enforce the live registry, the daemon does not
advertise it as a managed copy action. Set
`auto_resume_supported` only when the profile has authoritative usage semantics
for its provider; the default is fail-closed `false`.

## Trust model

The daemon binds loopback and *refuses* a non-loopback bind unless `--allow-non-loopback` is passed, because
it drives a remote shell. `GET /healthz`, `GET /sessions`, `GET /usage`,
`GET /sessions/{id}/glance`, `GET /sessions/{id}/buffer`, and
`GET /sessions/{id}/auto-resume` are intentionally open on the bind address.
Those routes can expose working directories, recent prompts, pane content,
provider usage, auto-resume state, and the server-global tmux clipboard.
Loopback blocks remote access but does not authenticate same-host principals;
the raw daemon therefore requires a trusted single-user host or an equivalent
local access-control boundary. Path-bearing
reads (`workdirs`), activity streaming, writes, and attach require the bearer token. Front the daemon with the agent-console edge (which
applies its own auth) and do **not** `tailscale serve` the raw serve port; expose only the edge, tailnet-only, no funnel.
Browser WebSocket clients cannot set an `Authorization` header, so the edge must proxy the attach and inject the bearer
server-side — never put the token in the `ws://` URL/query.

## Session survival across serve restarts

The daemon starts each session as a child `tmux new-session -d`, so the tmux
server shares the caller's cgroup. Under a systemd service that means it shares the unit cgroup, and stopping or restarting
the service can kill every live session. Set `AGENT_SESSION_TMUX_SCOPE=1` to launch the tmux server inside a transient
systemd user scope (`systemd-run --user --scope`) so it lands in its own cgroup instead — a sibling of the service, so
sessions survive a daemon restart or even an explicit cgroup-wide kill. It is opt-in (the serve launcher sets it) and only
engages when a systemd `--user` manager is reachable; on any other host (no user manager, missing `systemd-run`, non-Linux)
it falls back to launching tmux directly. Pairs with `KillMode=process` on the serve unit for defense in depth.
