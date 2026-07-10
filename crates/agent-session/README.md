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
latest provider conversation implicitly. `send` bumps `updated_at`, so `list` orders by real control-plane activity.
`--agent hermes` launches `hermes chat` interactively (one-shot `run` mode is codex/claude only).

## Serve daemon

`agent-session serve` exposes the session control plane over loopback HTTP for a per-machine edge (e.g. the agent-console
web console). It builds its own tokio runtime and reuses the synchronous lifecycle functions via `spawn_blocking`, so there
is no second state model.

- `GET /healthz`, `GET /sessions`, `GET /sessions/{id}/glance?tail=N` — reads, open on loopback. Sessions report
  `running`, `stopped`, or `unknown` live status plus a boolean `resumable` field and best-effort `repo_name` derived from
  the recorded `cwd`.
- `GET /usage` — read-only provider usage report, open on loopback. The serve
  envelope contains `data.usage.schema_version: "agent-session.usage.v1"` and
  provider entries for Codex and Claude. Provider readers are bounded by
  `AGENT_SESSION_USAGE_TIMEOUT_MS` (default 12000), preserve partial success,
  preserve reset timestamps as `reset_at_epoch` epoch seconds plus textual
  `reset_at` when supplied by the helper, and redact tokens, local auth paths,
  and private account identifiers from scoped error messages.
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
  interactive-session terminal bytes after the final client disconnects. The client sends JSON control frames
  `{ "text": "...", "key": "enter", "keys": ["c-c"], "resize": { "cols": 80, "rows": 24 } }`. Token-gated; disconnect
  leaves the tmux session alive. Concurrent clients share the pane geometry; resize sequences are serialized and the
  last completed resize wins.

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
