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
printf '%s' "$AGENT_SESSION_TOKEN" | agent-session serve --bind 127.0.0.1:8781 --token-stdin
agent-session command <id>
agent-session attach <id>
agent-session logs <id>
agent-session delete <id>
```

`send` pushes input to a live session: literal text (`--text` / `--text-stdin`) and/or repeatable named keys
(`--key enter|escape|c-c|up|down|left|right|tab`), so codex/claude approval prompts are answerable from a phone.
`glance` returns the recent pane tail plus live status as a JSON contract for dashboard tiles (cheaper than a full attach).
`resume` recreates a missing tmux runtime only when the session has exact provider resume metadata; it never resumes the
latest provider conversation implicitly. Runtime metadata is persisted before launch so hooks see the new generation;
if tmux launch fails, the prior runtime and activity snapshot are restored. `send` bumps `updated_at`, so `list` orders
by real control-plane activity.
`--agent hermes` launches `hermes chat` interactively (one-shot `run` mode is codex/claude only).

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
agent-session activity setup --agent codex --apply
agent-session activity doctor --agent codex --format json
agent-session activity setup --agent codex --remove
```

The same commands support `claude` and `hermes`. Setup merges exact
agent-session-owned handlers into existing provider configuration, repeated
apply/repair is idempotent, and removal preserves unrelated hooks. Provider
setup also refuses an observed concurrent config change. For Codex, setup adds
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
conflicts. Both Codex files are parsed and planned before either is
written; if the guarded second write fails, the first write is restored or a
loud rollback error identifies the partial state. That provider-authored
notification must match the exact open runtime/thread/turn and is the
authoritative completion input. Raw Codex
`Stop` remains non-final observation. Hook/notification failure is fail-open and
old/unsupported providers retain the activity fallback. Doctor scans local
session evidence once and probes provider versions concurrently with a bounded
timeout, verifies the exact owned hook timeout and owned/composed Codex notify
argv, reports `notification_mode` as `absent`, `owned`, `composed`, `conflict`,
or `invalid`, surfaces sanitized configuration errors, and checks that the
configured helper resolves to an executable on the provider PATH.

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

- `GET /healthz`, `GET /sessions`, `GET /sessions/{id}/glance?tail=N` — reads, open on loopback. Sessions report
  `running`, `stopped`, or `unknown` live status plus a boolean `resumable` field and best-effort `repo_name` derived from
  the recorded `cwd`. New records also expose optional `runtime_started_at` and
  `turn_state`; old records omit them.
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
- `GET /workdirs?q=...&limit=N` — authenticated read; searches only the default operator roots (`$HOME/Project` and
  `$HOME/.config`) with bounded depth, count, and elapsed-time limits. Add `git_only=true&exclude_worktrees=true` for
  the curated project picker: only primary git working trees are returned, ordered by most-recent session cwd usage
  (`last_used`) and then name/path.
- `POST /sessions` (create), `PATCH /sessions/{id}` (title update), `POST /sessions/{id}/send`,
  `POST /sessions/{id}/resume`,
  `POST /sessions/{id}/attachments?filename=...`, `DELETE /sessions/{id}` — writes, require a bearer token.
- `POST /sessions` normally creates a fresh session from `agent`, optional `cwd`, `title`, `id`, `prompt`, and
  `agent_args`. When `provider_resume_id` is present (alias: `resume_id`), the daemon imports an existing Codex or
  Claude provider conversation instead: it resolves the original cwd from local provider history, persists exact
  `provider_resume` metadata, and starts tmux with the canonical resume command. In resume-id mode, omit `cwd`, `prompt`,
  and `agent_args`; invalid, missing, ambiguous, or unsupported provider ids return structured errors.
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
(`--token-stdin`, `--token`, or `AGENT_SESSION_TOKEN`) on all write and attach endpoints, compared without an early-exit
on the token bytes; when no token is configured (or it is empty) those endpoints fail closed (503). Prefer
`--token-stdin` for launcher integrations so token material does not appear in process arguments. It reads one trimmed
token from stdin, rejects empty input, rejects multiple newline-separated tokens, and rejects input over 8192 bytes.
`--token-stdin` conflicts with `--token`; both forms avoid printing token material in errors. Use a strong,
high-entropy token.

Trust model: the daemon binds loopback and *refuses* a non-loopback bind unless `--allow-non-loopback` is passed, because
it drives a remote shell. Session reads (`list` / `glance`) are intentionally open on the bind address, while path-bearing
reads (`workdirs`), writes, and attach require the bearer token. Front the daemon with the agent-console edge (which
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
