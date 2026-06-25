# agent-memory Docs

`agent-memory` owns the executable command surface for the local agent memory
store. The private memory-store repository owns the data and policy content.

The first Rust implementation preserves the existing shell command contract:

- `path`
- `list` / `ls`
- `index` / `idx`
- `agents`
- `personas`
- `init-agent`
- `init-persona`
- `resolve`
- `env`
- `doctor`

The Rust CLI also exposes `completion <bash|zsh>` to satisfy workspace
completion policy.

Beyond the original shell contract, the Rust CLI adds:

- `check [SCOPE] [--all] [--strict] [--format text|json]` — validates a scope's
  structural integrity (index/file parity, dangling `[[links]]`, and note
  frontmatter schema). This is the content-level companion to the layout-only
  `doctor`, and is the deterministic core the `review-global-memory` skill is
  intended to call instead of reimplementing the checks in bash.
- `add [SCOPE] --name --type --description [...]` — the single guarded writer
  that creates a note and its `MEMORY.md` index line atomically, so the two
  never drift. Refuses duplicate slugs and validates the type enum.
- `list [SCOPE] [--type <t>] [--format text|json]` — structured listing
  (`path/name/description/type/mtime` per note) with a frontmatter-type filter;
  the default text output is unchanged.
- `search <term> [SCOPE] [--all]` — case-insensitive substring search over note
  frontmatter and bodies, returning `scope/file:line: text`.

All JSON output follows the workspace CLI output contract: `--format json` is
canonical, `--json` is a hidden alias, and records carry a `schema_version`.
