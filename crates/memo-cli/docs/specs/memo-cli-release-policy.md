# memo-cli Release Policy

## Publishable-first MVP policy

- `nils-memo-cli` follows a publishable-first policy for MVP.
- The first release target is crates.io, not an internal-only distribution.
- Rationale: memo capture and agent-inbox workflows need an install path that works outside this repository.

## Required crate metadata

- `crates/memo-cli/Cargo.toml` must remain crates.io-ready.
- Required package fields:
  - `name = "nils-memo-cli"`
  - `version = "<semver>"`
  - `description = "<crate summary>"`
  - `repository = "https://github.com/sympoies/nils-cli"`
- `publish = false` is forbidden for MVP and release candidates.

## Release order gate

- Release order is controlled by `release/crates-io-publish-order.txt`.
- `nils-memo-cli` must be listed in dependency-safe position:
  - after shared dependencies it consumes (for MVP: `nils-common`)
  - before any future crates that depend on `nils-memo-cli`

## Dry-run verification gate

- Required publish readiness dry-run command:

```bash
scripts/publish-crates.sh --dry-run --crate nils-memo-cli
```

- Gate passes only when the command exits `0` and runs crate publish checks without metadata or ordering errors.

## Contract-change release notes gate

- Any release that adds command flags or JSON fields must include concise release
  notes in the PR and changelog/release artifact.
- For additive contract changes (for example metadata fields or temporal report
  options), notes must explicitly state:
  - compatibility impact: additive / non-breaking
  - new error codes (if any)
  - operator-facing rollout checks
