# Serve Daemon Operations

This runbook covers safe startup and routine integration of
`agent-session serve`. The daemon exposes the local session control plane over
HTTP and WebSocket while reusing the same lifecycle implementation as the CLI.

## Start safely

Bind to loopback and pass the bearer token on stdin so it does not appear in
process arguments:

```bash
printf '%s' "$AGENT_SESSION_TOKEN" | \
  agent-session serve \
    --bind 127.0.0.1:8781 \
    --token-stdin
```

The daemon refuses a non-loopback bind unless `--allow-non-loopback` is
explicitly passed. It controls a remote shell, so the recommended deployment is
an authenticated edge in front of the loopback daemon. Expose the edge, not the
raw serve port.

`--token-stdin` accepts one non-empty token of at most 8192 bytes and conflicts
with `--token`. `AGENT_SESSION_TOKEN` is the environment alternative. When no
non-empty token is configured, authenticated endpoints fail closed.

## Authentication boundaries

| Endpoint class | Authentication |
| --- | --- |
| Loopback health and ordinary session projections such as `/healthz`, `/sessions`, session glance, and `/usage` | Open on the bind address. |
| Path-bearing reads, account inventory, activity stream, writes, and WebSocket attach | Bearer token. |
| Public coordination and broker reads | Bearer token. |
| Session-owner coordination and mailbox mutations | Bearer token plus `X-Agent-Session-Capability`. |

The bearer token is machine-operator authority. The session capability is
per-incarnation owner authority. Session IDs and other request selectors do not
replace either credential.

Browser WebSocket clients cannot set an `Authorization` header. The edge must
proxy the attach and inject the bearer server-side; never put the token in a
WebSocket URL or query string.

## Create a session

`POST /sessions` accepts JSON. For example:

```json
{
  "agent": "codex",
  "cwd": "/workspace/example",
  "title": "Review the API",
  "prompt": "Inspect the current change",
  "coordination_mode": "advisory",
  "agent_args": []
}
```

The required field is `agent`. Fresh creation may also use `cwd`, `title`,
`title_state`, `id`, `prompt`, `coordination_mode`, `agent_args`, an advertised
`agent_profile`, and—for supported fresh Codex sessions—`codex_account`.
`coordination_mode` accepts `advisory`, `enforce`, or `off` and defaults to
`advisory`.

Provider import uses `provider_resume_id` (compatibility alias: `resume_id`)
with an advertised profile that supports import. In import mode, omit `cwd`,
`prompt`, `agent_args`, and `codex_account`; the daemon resolves the original
working directory and exact provider metadata from the selected local profile.

The server owns executable paths, provider configuration roots, readiness
commands, and auto-resume capability. Clients select only advertised safe IDs
and cannot override those private fields.

## Use the control plane

The main endpoint groups are:

- session inventory, glance, usage, work-directory discovery, and activity
  stream;
- session create, title update, send, prompt, resume, account selection,
  auto-resume, attachment upload, and delete;
- WebSocket PTY attach;
- raw work-context, broker recovery, and mailbox operations.

All responses use the `cli.agent-session.serve.v1` envelope and include a
machine identity selected by `--machine`, `AGENT_SESSION_MACHINE`, `--host`, or
the hostname fallback.

Use `POST /sessions/{id}/prompt/v2` when a client needs a cross-version fence.
It requires both exact prompt text and the expected session incarnation,
rejects unknown fields, and is absent from older daemons. A replaced runtime
returns `409 session-incarnation-conflict` before provider dispatch.

For work coordination, consult [Work coordination](work-coordination.md).
High-level self-targeting CLI operations such as `work-context set` do not have
HTTP convenience routes; the daemon exposes the raw v1 operations documented
in the [coordination route matrix](../specs/session-coordination-v1.md#http-coverage).

## Keep sessions alive across daemon restarts

By default, a child tmux server shares the caller's cgroup. Under systemd this
can allow a service restart to kill live sessions. Set
`AGENT_SESSION_TMUX_SCOPE=1` to request a transient systemd user scope for the
tmux server:

```bash
AGENT_SESSION_TMUX_SCOPE=1 agent-session serve ...
```

When a user systemd manager or `systemd-run` is unavailable, the daemon falls
back to direct tmux launch. Pair the isolated scope with `KillMode=process` on
the serve service for defense in depth.

## Optional integrations

- `AGENT_SESSION_CODEX_ACCOUNT_BROKER`: JSON argv array for a bounded host
  credential broker. Credentials remain in memory and are not projected into
  session documents or HTTP responses.
- `AGENT_SESSION_LAUNCH_PROFILES`: JSON array of server-owned launch profiles.
  Only profiles whose executable, optional provider root, and readiness probe
  pass are advertised.
- `AGENT_SESSION_CODEX_RUNTIME=raw|app-server`: force the Codex runtime choice.
  The default probes the installed CLI and degrades to raw TUI when the audited
  app-server capability is unavailable.
- `AGENT_SESSION_USAGE_TIMEOUT_MS`: bounds provider usage collection.

## Operational checks

1. Confirm `GET /healthz` succeeds on loopback.
2. Confirm unauthenticated writes and attach fail.
3. Confirm authenticated `GET /sessions` reports the expected machine and
   capability projection.
4. Create a disposable advisory session with JSON `coordination_mode`.
5. Verify list/glance, prompt or send, and delete through the edge.
6. If coordination is integrated, separately verify operator-only reads and
   bearer-plus-capability owner mutations.
7. Restart the daemon and confirm expected tmux survival before relying on the
   service configuration.

The crate [README](../../README.md#serve-daemon) remains the complete current
surface overview. Wire-level activity, coordination, turn-state, and
maintenance contracts are indexed in the crate [documentation map](../README.md).
