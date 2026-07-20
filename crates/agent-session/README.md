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

## Documentation map

- Start here for positioning, common commands, and links: this README.
- Operate collision awareness and work permissions:
  [Work coordination](docs/runbooks/work-coordination.md).
- Deploy the HTTP/WebSocket control plane:
  [Serve daemon operations](docs/runbooks/serve-daemon.md).
- Integrate stable schemas and state machines:
  [Serve API v1](docs/specs/serve-api-v1.md),
  [Session coordination v1](docs/specs/session-coordination-v1.md),
  [turn-state contract](docs/turn-state-contract.md), and
  [activity stream v1](docs/specs/activity-stream-v1.md).
- Browse every crate-local document by purpose:
  [agent-session documentation](docs/README.md).

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
agent-session work-context status --format json
agent-session work-context set --tier L2 --issue 123 --summary "Implement the tracked fix"
agent-session work-context advise --format json
agent-session work-context acknowledge --for 30m
agent-session work-context clear
agent-session message inbox --session <id>
agent-session serve --bind 127.0.0.1:8781 --token-stdin
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

## Work coordination

Work coordination is advisory by default. Managed sessions publish
privacy-safe presence and can declare repository-relative exact paths or
prefixes; overlapping work warns without blocking unless the session starts
with `--coordination-mode enforce`. Session IDs and claim IDs are selectors,
not credentials. Owner operations use the private per-incarnation capability.

Coordination does not grant or revoke user authorization, repository
permission, provider consent, or workflow authority. In default `advisory`
mode, missing context, unavailable coordination, and overlap reports remain
non-blocking. `work-context set` adds optional public task metadata so warnings
are more precise; it is not a permission request. Only a launch that explicitly
selects `--coordination-mode enforce` turns claims, admission, and physical
checkout leases into mutation requirements.

Use [Work coordination](docs/runbooks/work-coordination.md) for the operator
workflow and path syntax. The normative schemas, state machines, authorization
rules, limits, error codes, and HTTP coverage live in
[Session coordination v1](docs/specs/session-coordination-v1.md).

The canonical agent-facing policy, including how an agent responds to overlap
advice, lives in agent-runtime-kit's
[`session-coordination.md`](https://github.com/graysurf/agent-runtime-kit/blob/main/core/policies/session-coordination.md).
This README defines CLI and operator semantics only.

## Turn-state integration

Supported provider hooks project metadata-only lifecycle events into a private,
runtime-bound activity snapshot. Setup is explicit and reversible:

```bash
agent-session activity setup --agent codex --dry-run
agent-session activity setup --agent codex --apply
agent-session activity doctor --agent codex --format json
```

Use the [turn-state contract](docs/turn-state-contract.md) for persistence,
privacy, setup, repair, and provider behavior. The evidence behind supported
provider signals is recorded in
[provider turn-signal evidence](docs/provider-turn-signal-evidence.md).

## Serve daemon

`agent-session serve` exposes the local session control plane over HTTP and
WebSocket for an authenticated per-machine edge. Bind it to loopback, provide
the bearer token through stdin, and expose the edge rather than the raw port:

```bash
read -r -s AGENT_SESSION_SERVE_TOKEN
printf '%s' "$AGENT_SESSION_SERVE_TOKEN" | \
  agent-session serve --bind 127.0.0.1:8781 --token-stdin
unset AGENT_SESSION_SERVE_TOKEN
```

Keep that temporary shell variable unexported. The accepted
`AGENT_SESSION_TOKEN` compatibility input can be inherited by managed child
sessions, so it is not a safe credential source for a daemon that creates them.

Use [Serve daemon operations](docs/runbooks/serve-daemon.md) for deployment,
authentication boundaries, session creation, and restart survival. Integrators
should use the normative [Serve API v1](docs/specs/serve-api-v1.md), plus the
[activity stream](docs/specs/activity-stream-v1.md) and
[coordination](docs/specs/session-coordination-v1.md) contracts.

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
