#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  scripts/ci/agent-docs-snapshots.sh   # run agent-docs catalog/output behavior tests

Runs the agent-docs integration tests that cover the data-driven catalog,
preflight JSON contract, audit, and init stub output. These replace the former
add/baseline snapshot fixtures (the surface is now programmatic, not fixture
based).
USAGE
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
manifest_path="$repo_root/crates/agent-docs/Cargo.toml"

run_behavior_tests() {
  cargo test --manifest-path "$manifest_path" --test integration -- \
    catalog_parse:: resolution:: when_predicate:: content_validation:: \
    preflight:: command_surface:: init::
}

case "${1:-}" in
  "")
    run_behavior_tests
    ;;
  -h|--help)
    usage
    ;;
  *)
    usage
    exit 2
    ;;
esac
