# forge-cli docs

Crate-local documentation for the `forge-cli` binary.

## Specs

- [`specs/forge-cli-spec-v1.md`](specs/forge-cli-spec-v1.md) — v1 contract,
  parity matrix, lock-down rules, exit-code map.
- [`specs/forge-cli-ops-v1.yaml`](specs/forge-cli-ops-v1.yaml) —
  machine-readable op catalog.

## Runbooks

- [`runbooks/pr-head-repair-loop.md`](runbooks/pr-head-repair-loop.md) —
  keeping one PR and one review timeline across a CI repair loop: amend,
  force-with-lease, re-bind evidence, re-deliver, and the check-registration
  window after a new head.

Workspace-level docs that govern this crate (the envelope contract, the
crate docs placement policy, and the dispatch plan) live under the
repository root `docs/` tree; consult that tree from the workspace
root.

## Links

- Back to crate README: [`../README.md`](../README.md)
