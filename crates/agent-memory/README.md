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
|-- profiles/<id>/
|-- candidates/<producer>/
|-- agents/<id>/
|-- personas/<id>/
`-- archive/superseded/
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
agent-memory check [SCOPE] [--all] [--strict] \
  [--max-index-bytes <bytes>] [--forbid-terms-file <path>] \
  [--format text|json]
agent-memory add [SCOPE] --name <slug> --type <t> --description <text> \
  [--title <text>] [--hook <text>] [--body <text>|-] [--body-file <path>] \
  [--session-id <uuid>] [--format text|json]
agent-memory list [SCOPE] [--type <t>] [--format text|json]
agent-memory search <term> [SCOPE] [--all] [--format text|json]
agent-memory recall startup [--max-bytes <bytes>] [--format text|json]
agent-memory recall on-demand <term> [--format text|json]
agent-memory recall candidates [producer] [--format text|json]
agent-memory candidate add <producer> --name <slug> \
  [--title <text>] [--hook <text>] [--body <text>|-] \
  [--body-file <path>] [--format text|json]
agent-memory candidate list [producer] [--format text|json]
agent-memory candidate promote <producer> <slug> --type <t> \
  --description <text> [--title <text>] [--hook <text>] \
  --session-id <uuid> [--apply] [--format text|json]
agent-memory archive list [--format text|json]
agent-memory archive search <term> [--format text|json]
agent-memory archive retire <slug> --reason <text> \
  --superseded-by <owner> --archived-at <YYYY-MM-DD> \
  [--apply] [--format text|json]
agent-memory completion zsh
```

`SCOPE` accepts `root`, `global`, `<id>`, `agents/<id>`, `personas/<id>`,
`profiles/<id>`, or `candidates/<producer>`. Bare IDs resolve to
`agents/<id>`. IDs must remain one path component; traversal separators and
unknown prefixes are rejected.

## Structural check

`doctor` verifies the store *layout*; `check` verifies a scope's *content
integrity* (default scope `global`, `--all` to sweep every scope):

- index/file parity — every note has a `MEMORY.md` entry and every index link
  resolves to a file;
- dangling `[[wikilinks]]` (warn — forward references are allowed);
- note frontmatter schema — `name`, `description`, and `metadata.type` (one of
  `user | feedback | project | reference`) are required; `metadata.node_type`
  and `metadata.originSessionId` are expected (warn).
- optional index byte budget (`--max-index-bytes`) — reports actual and maximum
  bytes as an error when `MEMORY.md` is too large;
- optional exact forbidden-term audit (`--forbid-terms-file`) — reads one term
  per line from a regular, non-symlink file and reports scope, file, line, and
  matching term. Blank lines and `#` comments are ignored.

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

## Recall profiles

`recall startup` reads only `profiles/startup/MEMORY.md`, treats the payload as
untrusted memory data, and fails closed when the file exceeds 3,072 bytes by
default. `--max-bytes` can set a stricter or explicitly configured boundary.
It never falls back to `global/MEMORY.md`.

`recall on-demand <term>` searches curated `global/*.md` notes only and does
not emit the full global index. `recall candidates [producer]` lists opaque
proposal files under producer-isolated candidate roots and labels the result
untrusted.

## Candidate lifecycle

`candidate add` creates or reuses `candidates/<producer>/`, writes one opaque
proposal file, and updates its candidate index. Candidate bodies do not need
canonical frontmatter because provider-native memory may use its own format.
Candidate roots, indexes, source files, and body-file inputs reject symlinks.

`candidate promote` is non-mutating unless `--apply` is present. The preview
validates the producer, source, destination, canonical type, and both indexes,
then reports the planned move. Apply wraps the source, destination, global
index, and candidate index in a rollback transaction: it creates a canonical
global note, appends the curated index line, removes the candidate index line,
and removes the source only when every replacement succeeds. Duplicate global
slugs, traversal, symlinks, and stale transaction paths fail closed.
Canonical description, title, hook, and required session provenance must be
non-empty single-line values so they cannot inject YAML or index lines.
Valid `global/` directory symlinks remain supported. Candidate directories,
files, and indexes remain non-symlink boundaries. Rollback failures are
reported as incomplete and preserve `.promote-backup` recovery files instead
of claiming success.

## Inactive history

`archive/superseded/` preserves provenance for curated global notes whose
operational reminder has been replaced by a current policy, hook, CLI, config,
test, or other deterministic owner. It is deliberately not a normal memory
scope: startup recall, on-demand recall, active `search --all`, `check --all`,
and scope completion never enumerate archive contents.

`archive retire` is non-mutating unless `--apply` is present. The preview
validates the source, reports every active index it would update, rejects
unresolved active note references, and refuses unsafe links or duplicate
archive targets. Apply moves the note, adds lifecycle/provenance metadata,
removes active index links, and updates `archive/MEMORY.md` as one rollback-safe
transaction. Repeat `--superseded-by` when more than one current owner applies.

Candidate selection is intentionally outside the CLI: a reviewer must establish
semantic equivalence and approve the presented set before apply. Use `archive
list` or `archive search` only when historical context is explicitly needed;
these commands do not make archived content active memory.

## Output

Human-readable output is the default and mirrors the original shell contract.
Primary command output goes to stdout; errors go to stderr.
For the `recall`, `candidate`, `archive`, and extended `check` surfaces,
`--format json` keeps runtime failures in the command's versioned JSON envelope
with `ok=false` and a stable `error.code`.

## Exit Codes

- `0`: success
- `1`: runtime failure, missing scope, missing `MEMORY.md`, or failed doctor
  (also no match for on-demand recall)
- `64`: command-line usage error

## Docs

- [Docs index](docs/README.md)
