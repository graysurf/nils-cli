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

- `check [SCOPE] [--all] [--json] [--strict]` — validates a scope's structural
  integrity (index/file parity, dangling `[[links]]`, and note frontmatter
  schema). This is the content-level companion to the layout-only `doctor`, and
  is the deterministic core the `review-global-memory` skill is intended to call
  instead of reimplementing the checks in bash.
