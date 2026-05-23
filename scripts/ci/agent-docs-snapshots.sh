#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  scripts/ci/agent-docs-snapshots.sh           # run add/baseline tests that cover snapshot behavior
  scripts/ci/agent-docs-snapshots.sh --bless   # regenerate add snapshot expected fixtures
USAGE
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fixture_dir="$repo_root/crates/agent-docs/tests/fixtures/add"
manifest_path="$repo_root/crates/agent-docs/Cargo.toml"

run_snapshot_tests() {
  cargo test --manifest-path "$manifest_path" --test integration -- add:: baseline::
}

bless_add_snapshots() {
  if [[ ! -d "$fixture_dir" ]]; then
    echo "error: fixture directory not found: $fixture_dir" >&2
    exit 1
  fi

  mapfile -d '' inputs < <(find "$fixture_dir" -maxdepth 1 -type f -name '*.input.toml' -print0 | sort -z)
  if [[ ${#inputs[@]} -eq 0 ]]; then
    echo "error: no input fixtures found under $fixture_dir" >&2
    exit 1
  fi

  for input in "${inputs[@]}"; do
    input="${input%$'\0'}"
    expected="${input%.input.toml}.expected.toml"

    tmp="$(mktemp -d)"
    agent_home="$tmp/agent-home"
    project_path="$tmp/project"
    mkdir -p "$agent_home" "$project_path"
    cp "$input" "$agent_home/AGENT_DOCS.toml"

    cargo run --manifest-path "$manifest_path" --quiet -- \
      --agent-home "$agent_home" \
      --project-path "$project_path" \
      add \
      --target home \
      --context task-tools \
      --scope home \
      --path core/policies/cli-tools.md \
      --required \
      --notes after >/dev/null

    cp "$agent_home/AGENT_DOCS.toml" "$expected"
    rm -rf "$tmp"
    echo "updated snapshot: $(basename "$expected")"
  done

  run_snapshot_tests
}

mode="${1:-}"
case "$mode" in
  "")
    run_snapshot_tests
    ;;
  --bless)
    if [[ $# -ne 1 ]]; then
      usage
      exit 2
    fi
    bless_add_snapshots
    ;;
  -h|--help)
    usage
    ;;
  *)
    usage
    exit 2
    ;;
esac
