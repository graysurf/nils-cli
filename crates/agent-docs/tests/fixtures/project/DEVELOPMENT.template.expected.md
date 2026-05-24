# DEVELOPMENT.md

## Setup

Run setup before editing or building:

```bash
cargo fetch
```

## Build

Run build commands before sharing changes:

```bash
cargo build --workspace
```

## Test

Run project-defined local checks before delivery:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
```

## Notes

- Keep commands deterministic and runnable from the repository root.
- Update this file when your build or test workflow changes.
