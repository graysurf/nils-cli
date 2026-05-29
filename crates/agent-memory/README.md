# nils-agent-memory

`agent-memory` resolves and manages the local git-backed memory-store layout
used by local AI agents.

Default root:

```text
${AGENT_MEMORY_HOME:-${XDG_CONFIG_HOME:-$HOME/.config}/agent-memory}
```

## Layout

```text
agent-memory/
|-- global/
|-- agents/<id>/
`-- personas/<id>/
```

## Commands

```bash
agent-memory path [SCOPE]
agent-memory list [SCOPE]
agent-memory index [SCOPE]
agent-memory agents
agent-memory personas
agent-memory init-agent <id>
agent-memory init-persona <id>
agent-memory resolve <id>
agent-memory env
agent-memory doctor
agent-memory completion zsh
```

`SCOPE` accepts `root`, `global`, `<id>`, `agents/<id>`, or `personas/<id>`.
Bare IDs resolve to `agents/<id>`.

## Output

Human-readable output is the default and mirrors the original shell contract.
Primary command output goes to stdout; errors go to stderr.

## Exit Codes

- `0`: success
- `1`: runtime failure, missing scope, missing `MEMORY.md`, or failed doctor
- `64`: command-line usage error

## Docs

- [Docs index](docs/README.md)
