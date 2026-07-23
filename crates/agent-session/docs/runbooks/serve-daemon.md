# Serve Daemon Operations

This runbook covers safe startup and routine integration of
`agent-session serve`. The daemon exposes the local session control plane over
HTTP and WebSocket while reusing the same lifecycle implementation as the CLI.

## Start safely

Bind to loopback and pass the bearer token on stdin so it does not appear in
process arguments or the daemon environment. In this interactive example the
temporary shell variable must remain unexported:

```bash
read -r -s AGENT_SESSION_SERVE_TOKEN
printf '%s' "$AGENT_SESSION_SERVE_TOKEN" | \
  agent-session serve \
    --bind 127.0.0.1:8781 \
    --token-stdin
unset AGENT_SESSION_SERVE_TOKEN
```

The daemon refuses a non-loopback bind unless `--allow-non-loopback` is
explicitly passed. It controls a remote shell, so the recommended deployment is
an authenticated edge in front of the loopback daemon. Expose the edge, not the
raw serve port.

`--token-stdin` accepts one non-empty token of at most 8192 bytes and conflicts
with `--token`. The CLI still accepts `--token` and `AGENT_SESSION_TOKEN` for
compatibility, but do not use the environment form for a daemon that creates
managed sessions: the current child-launch path can inherit that variable and
thereby grant a provider the machine-operator bearer. Prefer a private
credential source connected to stdin. When no non-empty token is configured,
authenticated endpoints fail closed.

## Authentication boundaries

| Endpoint class | Authentication and exposure |
| --- | --- |
| `GET /healthz` | Open on the bind address; health metadata only. |
| `GET /sessions` | Open; may include working directories and recent prompt previews. |
| `GET /usage` | Open; returns provider usage projections. |
| `GET /sessions/{id}/glance` | Open; returns recent live pane content. |
| `GET /sessions/{id}/buffer` | Open; returns the tmux server's latest global clipboard buffer after only checking that the session exists. |
| `GET /sessions/{id}/auto-resume` | Open; returns that session's auto-resume state. |
| Path-bearing reads, account inventory, activity stream, writes, and WebSocket attach | Bearer token. |
| Public coordination and broker reads | Bearer token. |
| Session-owner coordination and mailbox mutations | Bearer token plus `X-Agent-Session-Capability`. |

Loopback prevents remote network access; it does not authenticate local
processes or users. Run the raw daemon only on a trusted single-user host or
behind a local access-control boundary that prevents untrusted same-host
principals from reaching the port. Any local caller that can connect can read
the open prompt, pane, path, usage, clipboard, and auto-resume projections.

The bearer token is machine-operator authority. The session capability is
per-incarnation owner authority. Session IDs and other request selectors do not
replace either credential.

Browser WebSocket clients cannot set an `Authorization` header. The edge must
proxy the attach and inject the bearer server-side; never put the token in a
WebSocket URL or query string.

## Prompt preview lifecycle

`GET /sessions` may return a running Codex or Claude session's `last_prompt`
only when the record carries an exact provider resume identity and the matching
regular transcript can be validated. On first discovery the daemon establishes
an append offset, queues an at-most-64-MiB cold recovery outside the list-response
path, then retains only the latest bounded preview in process memory. Recovery is
single-flight per session and daemon-wide concurrency-bounded. Later list
requests perform only a bounded freshness check; appended chunks are consumed in
the background instead of repeating the cold scan.

The preview cache is not daemon state: it is never written to the session
record, logs, or diagnostics, and a daemon restart reconstructs it from the
provider transcript. Eligible running sessions report `last_prompt_state`:

- `current` means the exact tracker is caught up. A missing `last_prompt` in
  this state authoritatively means no eligible user prompt exists.
- `pending` means exact discovery, cold recovery, or append catch-up is in
  progress. The preview stays omitted rather than exposing a stale cached
  prompt.
- `unavailable` means no exact source can currently be used or continuity was
  invalidated. The preview stays omitted and the old cached value cannot cross
  the API boundary. After invalidation it remains visible to every caller until
  one response can expose an authoritative `current` projection.

Eligible states also carry the opaque response-only
`last_prompt_continuity` token. It rotates when exact transcript continuity is
lost and on a daemon restart, so a consumer that missed an intermediate
`unavailable` response cannot re-display a preview from the prior source. The
token is 16-128 URL-safe ASCII characters and contains no prompt, path, provider
session id, or runtime identifier.

Transcript rotation, truncation, replacement, or identity drift clears the
cached value and requires exact rediscovery. Stable admission at the registry
bound prevents overflow sessions from repeatedly evicting warm recovery state.
A missing provider resume identity makes the session ineligible, so both state
and preview remain intentionally omitted rather than authorizing a likely
transcript scan.

Because `GET /sessions` is open on the loopback bind, treat prompt previews as
sensitive local-user data. Aggregate health checks should count preview presence
by provider without printing prompt text, session ids, resume ids, or transcript
paths.

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

Provider import uses `provider_resume_id` (compatibility alias: `resume_id`).
When `agent_profile` is omitted, discovery uses the daemon's default provider
history. When a profile is selected, it must advertise import support and
discovery is confined to that profile's provider root. In either import mode,
omit `cwd`, `prompt`, `agent_args`, and `codex_account`; the daemon resolves the
original working directory and exact provider metadata from the selected
history source.

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

Ordinary JSON HTTP responses use the `cli.agent-session.serve.v1` envelope.
Successful responses include the machine identity in `data.machine`, selected
by `--machine`, `AGENT_SESSION_MACHINE`, `--host`, or the hostname fallback;
current error envelopes omit it. The activity SSE stream and WebSocket attach
use their own streaming protocols rather than that JSON envelope.

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

The normative endpoint surface is in [Serve API v1](../specs/serve-api-v1.md).
Wire-level activity, coordination, turn-state, and maintenance contracts are
indexed in the crate [documentation map](../README.md).
