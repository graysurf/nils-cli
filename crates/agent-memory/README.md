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
agent-memory check [SCOPE] [--all] [--strict] [--format text|json]
agent-memory add [SCOPE] --name <slug> --type <t> --description <text> \
  [--title <text>] [--hook <text>] [--body <text>|-] [--body-file <path>] \
  [--session-id <uuid>] [--format text|json]
agent-memory list [SCOPE] [--type <t>] [--format text|json]
agent-memory search <term> [SCOPE] [--all] [--format text|json]
agent-memory completion zsh
```

`SCOPE` accepts `root`, `global`, `<id>`, `agents/<id>`, or `personas/<id>`.
Bare IDs resolve to `agents/<id>`.

## Structural check

`doctor` verifies the store *layout*; `check` verifies a scope's *content
integrity* (default scope `global`, `--all` to sweep every scope):

- index/file parity — every note has a `MEMORY.md` entry and every index link
  resolves to a file;
- dangling `[[wikilinks]]` (warn — forward references are allowed);
- note frontmatter schema — `name`, `description`, and `metadata.type` (one of
  `user | feedback | project | reference`) are required; `metadata.node_type`
  and `metadata.originSessionId` are expected (warn).

`--format json` emits machine-readable findings (each carrying `{scope, kind,
file, detail, severity}`, under a `schema_version` envelope; `--json` is a
hidden alias). `--strict` promotes warnings to failures. A clean scope exits
`0`; any error-level finding (or any finding under `--strict`) exits `1`.

## Writing and querying notes

`add` is the single guarded writer: it creates `<scope>/<slug>.md` with correct
frontmatter and appends a matching `MEMORY.md` index line in one operation
(rolling the note back if the index write fails), so the two never drift. It
refuses a duplicate slug and validates `--type` against the enum;
`metadata.originSessionId` is written only when `--session-id` is supplied. The
body comes from `--body <text>`, `--body -` (stdin), or `--body-file <path>`.
`check <scope>` is clean immediately after.

`list --format json` emits one record per note
(`{path, name, description, type, mtime}`, under a `schema_version` envelope);
`--type <t>` filters by frontmatter type. The default text output (note
filenames, including `MEMORY.md`) is unchanged.

`search <term>` does a case-insensitive substring scan over note content
(frontmatter — including the `description` — and the body) across a scope, or
`--all` scopes, printing `scope/file:line: text`. It exits `0` when there are
matches and `1` when there are none.

## Output

Human-readable output is the default and mirrors the original shell contract.
Primary command output goes to stdout; errors go to stderr.

## Exit Codes

- `0`: success
- `1`: runtime failure, missing scope, missing `MEMORY.md`, or failed doctor
- `64`: command-line usage error

## Docs

- [Docs index](docs/README.md)
