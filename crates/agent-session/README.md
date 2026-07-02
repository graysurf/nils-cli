# agent-session

## Overview

`agent-session` starts and manages tmux-backed Codex and Claude Code sessions for mobile handoff workflows. It is designed for personal
automation such as Hermes Telegram skills: a service can create the session with a full prompt, then return a short tmux attach command for
the user to continue from Termius.

## Package vs binary name

| Field        | Value                |
| ------------ | -------------------- |
| Package name | `nils-agent-session` |
| Binary name  | `agent-session`      |

## Usage

```bash
agent-session start --agent codex --cwd ~/Project/foo --prompt-file prompt.md
agent-session list
agent-session command <id>
agent-session attach <id>
agent-session logs <id>
agent-session delete <id>
```

## Output contract

Human-readable text is the default. JSON is opt-in with `--format json` on command subcommands.

JSON output uses the workspace envelope: `schema_version`, `ok`, `data`, optional `warnings`, and `error` on failure.

## Secret-safety boundary

Prompts are stored under the local agent-session state directory and are not printed in command output. For sensitive prompts, prefer
interactive `start`; one-shot `run` may need to pass the prompt through the underlying agent process command line depending on that agent's
CLI capabilities.
