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
