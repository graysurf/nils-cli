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
  `doctor`, and is the deterministic core the project global-memory review
  skill calls instead of reimplementing the checks in bash.
  Optional `--max-index-bytes` and `--forbid-terms-file` checks enforce bounded
  indexes and caller-owned retired-reference lists without embedding policy in
  the CLI.
- `add [SCOPE] --name --type --description [...]` — the single guarded writer
  that creates a note and its `MEMORY.md` index line atomically, so the two
  never drift. Refuses duplicate slugs and validates the type enum.
- `list [SCOPE] [--type <t>] [--format text|json]` — structured listing
  (`path/name/description/type/mtime` per note) with a frontmatter-type filter;
  the default text output is unchanged.
- `search <term> [SCOPE] [--all]` — case-insensitive substring search over note
  frontmatter and bodies, returning `scope/file:line: text`.
- `recall startup|on-demand|candidates` — exposes bounded startup routing,
  curated term recall, and explicitly untrusted proposal listing as separate
  profiles.
- `candidate add|list|promote` — isolates proposal writers by producer and
  provides an explicit dry-run/apply promotion transaction into curated
  `global/` memory. Promotion requires explicit session provenance, preserves
  supported global-directory symlinks, removes exact native-index filename
  references, and reports incomplete rollback without deleting recovery
  backups.
- `archive list|search` — explicitly queries historical superseded notes that
  are structurally excluded from active recall, search, checks, and completion.
- `archive retire <slug> ... [--apply]` — dry-run-first, rollback-safe movement
  from curated global memory to `archive/superseded/`; it blocks on unresolved
  active references and records reason/current-owner provenance.

Candidate files are opaque untrusted data; canonical frontmatter is required
only at promotion. Scope IDs, producer IDs, source files, and audit inputs are
path-validated and symlink guarded.

Archive commands are storage primitives, not semantic candidate detectors. A
project workflow verifies that runtime behavior actually supersedes the
reminder and obtains approval before invoking `archive retire --apply`.

All JSON output follows the workspace CLI output contract: `--format json` is
canonical, `--json` is a hidden alias, and records carry a `schema_version`.
The new command families retain the same envelope for runtime errors.
