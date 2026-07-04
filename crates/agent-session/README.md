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
agent-session command <id>
agent-session attach <id>
agent-session logs <id>
agent-session delete <id>
```

`send` pushes input to a live session: literal text (`--text` / `--text-stdin`) and/or repeatable named keys
(`--key enter|escape|c-c|up|down|left|right|tab`), so codex/claude approval prompts are answerable from a phone. `glance` returns the recent
pane tail plus live status as a JSON contract for dashboard tiles (cheaper than a full attach). `send` bumps `updated_at`, so `list` orders by
real control-plane activity. `--agent hermes` launches `hermes chat` interactively (one-shot `run` mode is codex/claude only).

## Output contract

Human-readable text is the default. JSON is opt-in with `--format json` on command subcommands.

JSON output uses the workspace envelope: `schema_version`, `ok`, `data`, optional `warnings`, and `error` on failure.

## Secret-safety boundary

Prompts are stored under the local agent-session state directory and are not printed in command output. For sensitive prompts, prefer
interactive `start`; one-shot `run` may need to pass the prompt through the underlying agent process command line depending on that agent's
CLI capabilities. `send` routes literal text through a private (0600) buffer file loaded into tmux, so it never appears in the tmux command
line or command output; the JSON contract reports only `sent_text` (a boolean) and the special-key names, never the text itself. For secrets,
prefer `--text-stdin`: `--text <value>` still places the literal in agent-session's own process arguments (visible in `ps` to same-user
processes), exactly as the existing `--prompt` flag does. `send` is not idempotent — keystrokes are delivered before the command returns, so a
retry after a mid-delivery failure can re-send; callers that auto-retry should account for this.
