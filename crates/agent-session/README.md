# agent-session

## Overview

`agent-session` starts and manages tmux-backed Codex, Claude Code, and Hermes sessions for mobile handoff workflows. It is designed for
personal automation such as Hermes Telegram skills and the agent-console mobile control plane: a service can create the session with a full
prompt, then return a short tmux attach command for the user to continue from Termius, glance at the pane, or steer it with keystrokes.

## Package vs binary name

| Field        | Value                |
| ------------ | -------------------- |
| Package name | `nils-agent-session` |
| Binary name  | `agent-session`      |

## Usage

```bash
agent-session start --agent codex --cwd ~/Project/foo --prompt-file prompt.md
agent-session start --agent hermes --cwd ~
agent-session list
agent-session glance <id> --tail 40
agent-session send <id> --text yes --key enter
agent-session send <id> --key c-c
agent-session resume <id>
agent-session activity status <id> --format json
agent-session activity doctor --format json
agent-session activity setup --agent codex --dry-run
agent-session activity setup --agent codex --repair --dry-run
agent-session activity setup --agent codex --repair --expected-preview-digest sha256:<reviewed-plan-digest>
agent-session work-context claim --session <id> --file context.json --idempotency-key <key>
agent-session work-context check --session <id>
agent-session message inbox --session <id>
printf '%s' "$AGENT_SESSION_TOKEN" | agent-session serve --bind 127.0.0.1:8781 --token-stdin
agent-session command <id>
agent-session attach <id>
agent-session logs <id>
agent-session delete <id>
```

`send` pushes input to a live session: literal text (`--text` / `--text-stdin`) and/or repeatable named keys
(`--key enter|escape|backspace|c-c|up|down|left|right|tab`), so codex/claude approval prompts and terminal editing
remain usable from a phone.
`glance` returns the recent pane tail plus live status as a JSON contract for dashboard tiles (cheaper than a full attach).
`resume` recreates a missing tmux runtime only when the session has exact provider resume metadata; it never resumes the
latest provider conversation implicitly. Runtime metadata is persisted before launch so hooks see the new generation,
and the immutable tmux session/pane identity is persisted before a successful start or resume returns. Resume first
proves the current and every retained prior launch identity stopped, so a surviving provider process cannot be hidden by
a new runtime generation.
An older stopped record without that proof returns the same non-retryable manual-verification action as deletion; only
a generation durably marked as never launched can resume without a runtime identity. If tmux launch fails, the prior
runtime and activity snapshot are restored only after any possibly launched replacement is verified stopped. An
unverified replacement remains the current discoverable generation. `send` bumps `updated_at`, so `list` orders by real
control-plane activity.
`delete` removes provider runtime files and session metadata only after bounded checks verify the recorded runtime is
stopped. Before killing a live runtime, it inspects only the managed `0.0` pane, captures its immutable tmux session/pane
ids and process boundary, validates the runtime's `AGENT_SESSION_*` ownership markers, persists that identity for retry, and
uses one tmux-server conditional command to kill only if the captured session, pane, and pane PID still match.
If the managed pane is replaced, it durably retains every unresolved observed process identity before proceeding. It then
targets only the captured tmux session id. On Linux it snapshots each verified process-session member by PID and start time
and requires the pane to be in a leaf cgroup-v2 `tmux-spawn-*.scope`. It pins the process sets observed before and during a
bounded stabilization window after freezing that cgroup, revalidates the pane membership and cgroup inode, conditionally
kills the exact tmux identity, and
invokes `cgroup.kill` while the boundary remains frozen. Members present at either verified boundary cannot survive by
changing process session or by later cgroup migration. The cgroup identity also records the Linux boot id, so startup
recovery never applies an old PID/start-time or cgroup identity to a different boot. A durable ownership marker lets a
delete retry or a restarted server thaw only a scope that deletion changed from unfrozen to frozen; a scope that was already
frozen remains frozen. Without that distinct leaf cgroup, Linux deletion fails closed
before mutating tmux; other Unix platforms retain verify-only handling for the pane process-group boundary. Cleanup requires
tmux and every retained process boundary to be gone. A retry can
finish cleanup without another kill only when all persisted identities verify stopped. Live records created by an older
version can be upgraded from their ownership markers. A stopped pre-upgrade record without a provable launch identity remains
retained and returns `runtime-identity-unavailable`, `retryable: false`, and
`action: manual-runtime-verification-required`; an operator must verify its runtime manually before removing that state.
Kill failures, ambiguous tmux errors, ownership mismatches, and surviving processes retain all session state. Human
success output reports the verified stopped state; the v1 JSON `killed: true` field remains stable for successful deletion.
When tmux returns a successful but blank identity probe for a stale session, deletion confirms the exact session is absent
and still requires every persisted process boundary to verify stopped before removing metadata.
For a stopped session without a one-shot run log, `agent-session logs <id>` falls back to the private, tail-capped startup
diagnostic. Codex sessions retain that diagnostic after a startup failure or non-zero provider-client exit; a clean exit
after readiness discards it.
`--agent hermes` launches `hermes chat` interactively (one-shot `run` mode is codex/claude only).

## Session coordination

Every new managed runtime receives a private per-incarnation capability through
the 0600 file named by `AGENT_SESSION_CAPABILITY_FILE`. Session, incarnation,
claim, operation, and message identifiers are selectors and revision fences,
not credentials. Start, run, provider import, and resume create a held tmux pane;
the provider command is released only after the exact runtime identity and
private capability are durable. A sidecar heartbeat survives launcher exit,
rotates on resume, and removes its incarnation-specific capability after target
exit. Delete revokes the capability and releases active coordination state
before removing the session.

`work-context claim|show|check|renew|release` manages an authenticated
30-minute structured claim. Claims contain canonical repositories, private-keyed
worktree fingerprints, provider and plan references, and closed
`repository|path-exact|path-prefix` scopes. `check` is advisory;
`claim` evaluates and acquires under one bounded registry lock, so concurrent
definite contenders cannot both succeed. `work-context
admit|complete|reconcile` binds covered filesystem/provider mutation targets to
an execution token, exact activity/descendant evidence, and the persisted
runtime identity. Releasing or replacing the claim is rejected while that
operation is active or uncertain; a matching activity or descendant renews the
30-minute lease, and missed completion needs exact stopped-runtime proof or two
quiescent observations at least five seconds apart. Opaque, unbound-checkout,
or uncovered targets fail closed.
Peer summaries remain untrusted metadata and cannot authorize commands.

`message send|inbox|show|ack|reply|wait` provides the private bounded mailbox.
Only the authenticated recipient can read a body, returned as
`body.classification: "untrusted_peer_data"`; send results, inbox rows, errors,
list, and glance never contain it. V1 enforces the documented 16 KiB body,
24-hour default/7-day maximum expiry, 256-message/4 MiB per-session, 64 MiB
registry, 30-pair/minute with burst 10, opaque bounded cursors, 50/100-page,
60-second wait, and depth-16 reply limits.
Acknowledged entries retain metadata for 24 hours; expired entries have bounded
terminal retention, and the HTTP
surface admits at most 16 blocking waits at once.
The authoritative result is always queued mail. When the serve controller sees
an exact idle prompt-v2 target, it durably records one attempt before submitting
the fixed body-free notification; busy, replaced, unsupported, failed, and
rate-limited targets remain queue-only without raw terminal input or retries.

The complete schemas, scope truth table, state machines, error codes, and route
matrix are in
[`docs/specs/session-coordination-v1.md`](docs/specs/session-coordination-v1.md).
Managed calls normally use the capability path from the environment; external
CLI mutations pass `--capability-file`. HTTP public work-context/broker reads
require the serve bearer token; owner and mailbox mutations additionally require
`X-Agent-Session-Capability`, keeping operator and session authority separate.
List and glance add only claim state/id/expiry, unread
count, conflict severity, and coordination availability fields; the existing
`cwd` field remains unchanged.

## Durable turn state

Every new runtime receives a fresh opaque `AGENT_SESSION_RUNTIME_ID` alongside
`AGENT_SESSION_ID` and `AGENT_SESSION_STATE_DIR`. Supported provider hooks
project lifecycle metadata into a private, atomic activity snapshot and bounded
journal. Provider identifiers are runtime-scoped opaque projections, attention
and replay state are explicitly bounded, and interrupted snapshot/journal
writes repair before the next event or runtime transition. State is bound to
both the launch id and persisted runtime generation so a stale snapshot is
never shown after an interrupted resume. The replay index carries the same
runtime binding and a missing or swapped index degrades safely to Unknown
instead of reopening the dedupe horizon. Session views add optional
`runtime_started_at` and `turn_state`
fields, distinguishing `starting`, `working`, `waiting`, `needs_input`, and
`unknown` without storing prompt, assistant, terminal, command, tool, or
transcript content.

Provider setup is explicit and reversible. Preview it first, then apply only
after reviewing the provider trust/consent boundary:

```bash
agent-session activity setup --agent codex --dry-run
agent-session activity setup --agent codex --repair --dry-run
agent-session activity setup --agent codex --repair --expected-preview-digest sha256:<reviewed-plan-digest>
agent-session activity setup --agent codex --apply
agent-session activity doctor --agent codex --format json
agent-session activity setup --agent codex --remove
```

Ordinary `--dry-run`, `--apply`, `--repair`, and `--remove` also support
`claude` and `hermes`; the combined `--repair --dry-run` reviewed-plan workflow
is Codex-only and rejects other providers. Setup merges exact agent-session-owned
handlers into existing provider configuration, repeated apply/repair is
idempotent, and removal preserves unrelated hooks. Provider setup also refuses
an observed concurrent config change. For Codex, setup selects the lifecycle
representation already active in the user layer: JSON-only installs stay in
`~/.codex/hooks.json`, while existing inline `[[hooks.<Event>]]` groups cause
exact agent-session handlers to migrate into a separate marker-bounded block in
`~/.codex/config.toml`. An owned-only JSON source is deleted; unrelated content
is preserved. User-owned lifecycle handlers in both sources produce a
content-free conflict and block dry-run/apply/repair migration without mutation;
`--remove` may still clean exact agent-session handlers from both sources without
moving user content. Setup and doctor report representation, migration,
conflict, and the active hook path. Migrated definitions have new
Codex trust identities, so review them with `/hooks` and verify a fresh session
does not emit the dual-representation warning. Fields added to an otherwise
matching Codex command handler are user-owned:
dry-run/apply/repair fail closed instead of replacing them, while `--remove`
preserves that complete handler. Setup also adds
the official `agent-turn-complete` notify argv to `~/.codex/config.toml` when
`notify` is absent, recognizes exact ownership idempotently, or wraps a safe
user-owned singular argv in an agent-session-owned fan-out. The fan-out invokes
the preserved argv directly without a shell, suppresses its output, and kills
it after a two-second bound. A depth marker prevents nested fan-out, and
activity lock contention cannot delay the preserved notifier; downstream or
telemetry failure never blocks Codex. A contended authoritative completion is
handed to a detached metadata-only `activity event` retry that waits up to five
seconds for the same durable transaction lock without retaining provider
content. The worker clears diagnostics only after durable ingestion and records
a sanitized terminal code on timeout or failure. Setup composes
only when simulated removal proves the complete original TOML bytes can be
restored. Unsafe, oversized,
non-string, recursive, or non-reversible values are preserved and reported as
conflicts. Both Codex files are parsed and planned before either is written or
deleted; if the guarded second write fails, the first mutation is restored or a
loud rollback error identifies the partial state. The repair preview returns a
content-free plan digest over the current and proposed
bytes of both Codex files. Applying repair requires that exact digest and fails
before either write if either file changed after review. That provider-authored
notification must match the exact open runtime/thread/turn and is the
authoritative completion input. Raw Codex
`Stop` remains non-final observation. Hook/notification failure is fail-open and
old/unsupported providers retain the activity fallback. Doctor scans local
session evidence once and probes provider versions concurrently with a bounded
timeout, verifies the exact owned hook timeout and owned/composed Codex notify
argv, reports `notification_mode` as `absent`, `owned`, `composed`, `conflict`,
or `invalid`, surfaces sanitized configuration errors, and checks that the
configured helper resolves to an executable on the provider PATH.

Hermes 0.18.2 approval shell hooks are normalized from their nested `extra`
envelope. A non-empty tool-call id is projected into an exact runtime-scoped
pre/post correlation; missing or empty ids retain the conservative compatibility tuple
fallback. Empty top-level session ids fall back to the nested session key. Raw
approval kwargs are discarded before persistence, and malformed metadata stays
fail-open with a sanitized diagnostic code. Exact callbacks also derive a
kind-specific stable event id from the projected tool-call id, so replay remains
idempotent across interleaving, process restarts, and bounded journal eviction.

Codex itself appends the full notification JSON to the configured command argv.
Agent-session discards content after parsing and never prints or persists it,
but prompt, assistant, and cwd fields are transiently visible to same-host
process inspection while the helper runs. A composed user notifier receives the
same full JSON as its final argv that Codex previously supplied directly; the
fan-out does not add a shell or persist the payload. Use this integration only
where process visibility is restricted to the same trusted user. A provider-supported
stdin or metadata-only notification transport, or an App Server migration,
would be required to remove this upstream argv boundary.
See [the stable turn-state contract](docs/turn-state-contract.md) and
[provider evidence matrix](docs/provider-turn-signal-evidence.md).

## Serve daemon

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
- Every session view additively includes `auto_resume` using
  `agent-session.auto-resume.v1`. `GET /sessions/{id}/auto-resume` reads that
  status; `PUT` with `{ "enabled": true|false }` opts a supported session in or
  out, and `DELETE` durably cancels pending work. Both mutations require the
  bearer token. Claude Code is supported through its authoritative structured
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
  wire/privacy contract is [activity-stream-v1](docs/specs/activity-stream-v1.md).
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
- `POST /sessions` normally creates a fresh session from `agent`, optional `cwd`, `title`, `id`, `prompt`, and
  `agent_args`. A fresh create may add an advertised `agent_profile`; the id
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
  `codex --version` and `codex app-server --help` process groups. The audited
  versions are exactly Codex `0.144.1` and `0.144.3`, and help must advertise
  Unix `--listen` support. A
  matching CLI is launched as a remote TUI over a private short socket below an
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

Every response uses the `cli.agent-session.serve.v1` envelope and carries a `machine` identity (`--machine` /
`AGENT_SESSION_MACHINE` / `--host` / hostname) so an edge can aggregate several machines. Auth is a bearer token
(`--token-stdin`, `--token`, or `AGENT_SESSION_TOKEN`) on the activity stream plus all write and attach endpoints, compared without an early-exit
on the token bytes; when no token is configured (or it is empty) those endpoints fail closed (503). Prefer
`--token-stdin` for launcher integrations so token material does not appear in process arguments. It reads one trimmed
token from stdin, rejects empty input, rejects multiple newline-separated tokens, and rejects input over 8192 bytes.
`--token-stdin` conflicts with `--token`; both forms avoid printing token material in errors. Use a strong,
high-entropy token.

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

Trust model: the daemon binds loopback and *refuses* a non-loopback bind unless `--allow-non-loopback` is passed, because
it drives a remote shell. Session reads (`list` / `glance`) are intentionally open on the bind address, while path-bearing
reads (`workdirs`), activity streaming, writes, and attach require the bearer token. Front the daemon with the agent-console edge (which
applies its own auth) and do **not** `tailscale serve` the raw serve port; expose only the edge, tailnet-only, no funnel.
Browser WebSocket clients cannot set an `Authorization` header, so the edge must proxy the attach and inject the bearer
server-side — never put the token in the `ws://` URL/query.

Session survival across serve restarts: the daemon starts each session as a child `tmux new-session -d`, so the tmux
server shares the caller's cgroup. Under a systemd service that means it shares the unit cgroup, and stopping or restarting
the service can kill every live session. Set `AGENT_SESSION_TMUX_SCOPE=1` to launch the tmux server inside a transient
systemd user scope (`systemd-run --user --scope`) so it lands in its own cgroup instead — a sibling of the service, so
sessions survive a daemon restart or even an explicit cgroup-wide kill. It is opt-in (the serve launcher sets it) and only
engages when a systemd `--user` manager is reachable; on any other host (no user manager, missing `systemd-run`, non-Linux)
it falls back to launching tmux directly. Pairs with `KillMode=process` on the serve unit for defense in depth.

## Output contract

Human-readable text is the default. JSON is opt-in with `--format json` on command subcommands.

JSON output uses the workspace envelope: `schema_version`, `ok`, `data`, optional `warnings`, and `error` on failure.

## Secret-safety boundary

Prompts are stored under the local agent-session state directory and are not printed in command output. For sensitive prompts, prefer
interactive `start`; one-shot `run` may need to pass the prompt through the underlying agent process command line depending on that agent's
CLI capabilities. `send` routes literal text through a private (0600) buffer file loaded into tmux, so it never appears in the tmux
command line or command output; the JSON contract reports only `sent_text` (a boolean) and the special-key names, never the text itself.
Values passed with `--agent-arg` are persisted in the private session record so durable resume can recreate the same provider invocation.
Do not put secrets in provider arguments. For Claude sessions, provider identity/resume flags such as `--session-id`, `--resume`/`-r`,
`--continue`/`-c`, `--fork-session`, and `--from-pr` are reserved for agent-session so the stored resume identity stays exact.
For secrets, prefer `--text-stdin`: `--text <value>` still places the literal in agent-session's own process arguments (visible in `ps`
to same-user processes), exactly as the existing `--prompt` flag does. `send` is not idempotent — keystrokes are delivered before the
command returns, so a retry after a mid-delivery failure can re-send; callers that auto-retry should account for this.
