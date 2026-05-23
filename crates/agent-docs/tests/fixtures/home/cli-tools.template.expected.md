# CLI Tools

## Tool Selection

- Prefer `rg` over `grep -R` for recursive search.
- Prefer `fd` over `find` for file discovery.
- Prefer `jq` or `yq` over regex parsing for structured JSON/YAML data.

## Setup Command

```bash
cargo fetch
```

## Build Command

```bash
cargo build --workspace
```

## Test Command

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
```

## Maintenance

- Keep these commands aligned with current project conventions.
- Ensure examples stay executable in local shell and CI.
