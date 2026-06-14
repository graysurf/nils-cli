#!/usr/bin/env bash
# scripts/ci/publish-order-audit.sh — keep release/crates-io-publish-order.txt
# complete and topologically valid for a `cargo publish` sweep.
#
# Checks:
#   - every publishable workspace member is listed exactly once
#   - no listed entry is a non-member or a `publish = false` crate
#   - no duplicate entries
#   - each crate's normal/build workspace dependencies appear earlier in the
#     list, so `scripts/publish-crates.sh` never reaches a crate before a
#     workspace dependency it needs has been published
#
# This closes the gap that let #846 (`nils-scrub`) and #848 (`nils-evidence`)
# add publishable crates without a publish-order entry — review caught both
# after merge.
#
# Dependency edges come from each crate's manifest (`packages[].dependencies`
# in `cargo metadata`), NOT the feature/platform-resolved dependency graph: a
# `cargo publish` registry entry includes optional and target-gated deps, so
# they constrain the order even when a default build would not resolve them.
#
# Compatibility: heavy lifting runs in python3 (already required by the
# non-docs CI gate); the bash wrapper avoids bash-4-only constructs so it runs
# on macOS system bash 3.2.

set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  bash scripts/ci/publish-order-audit.sh [--strict] [--order-file <path>] [--metadata-file <path>]

Audits the crates.io publish order against `cargo metadata`. Exits 0 when the
order is complete and topologically valid, 1 on any violation, 2 on usage error
or missing prerequisites.

Options:
  --strict                Accepted for parity with the other audits (all
                          findings are already hard failures).
  --order-file <path>     Publish-order file to audit.
                          Default: release/crates-io-publish-order.txt
  --metadata-file <path>  Read workspace metadata from this `cargo metadata`
                          JSON instead of invoking cargo. Intended for the
                          self-test's synthetic fixtures.
  -h, --help              Show this help.
USAGE
}

order_file=""
metadata_file=""
while [[ $# -gt 0 ]]; do
  case "${1:-}" in
    --strict)
      shift
      ;;
    --order-file)
      if [[ $# -lt 2 ]]; then
        echo "error: --order-file requires a value" >&2
        exit 2
      fi
      order_file="${2:-}"
      shift 2
      ;;
    --order-file=*)
      order_file="${1#--order-file=}"
      shift
      ;;
    --metadata-file)
      if [[ $# -lt 2 ]]; then
        echo "error: --metadata-file requires a value" >&2
        exit 2
      fi
      metadata_file="${2:-}"
      shift 2
      ;;
    --metadata-file=*)
      metadata_file="${1#--metadata-file=}"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: ${1:-}" >&2
      usage >&2
      exit 2
      ;;
  esac
done

required_cmds=(git python3)
if [[ -z "$metadata_file" ]]; then
  required_cmds+=(cargo)
fi
for cmd in "${required_cmds[@]}"; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "error: missing required tool on PATH: $cmd" >&2
    exit 2
  fi
done

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: must run inside a git work tree" >&2
  exit 2
fi
cd "$repo_root"

if [[ -z "$order_file" ]]; then
  order_file="release/crates-io-publish-order.txt"
fi
if [[ ! -f "$order_file" ]]; then
  echo "error: publish-order file not found: $order_file" >&2
  exit 2
fi

cleanup_metadata=""
if [[ -n "$metadata_file" ]]; then
  if [[ ! -f "$metadata_file" ]]; then
    echo "error: metadata file not found: $metadata_file" >&2
    exit 2
  fi
  metadata_path="$metadata_file"
else
  metadata_path="$(mktemp "${TMPDIR:-/tmp}/publish-order-audit.XXXXXX")"
  cleanup_metadata="$metadata_path"
  trap 'rm -f "$cleanup_metadata"' EXIT
  if ! cargo metadata --format-version 1 --no-deps >"$metadata_path"; then
    echo "error: cargo metadata failed" >&2
    exit 2
  fi
fi

python3 - "$metadata_path" "$order_file" <<'PY'
import json
import sys

metadata_path, order_path = sys.argv[1], sys.argv[2]

with open(metadata_path, encoding="utf-8") as fp:
    md = json.load(fp)

ws_ids = set(md["workspace_members"])
ws_pkgs = [p for p in md["packages"] if p["id"] in ws_ids]
names = [p["name"] for p in ws_pkgs]
members = set(names)

errors = []

# Distinct package names are assumed everywhere below (the graph is name-keyed).
if len(members) != len(names):
    seen = set()
    for n in names:
        if n in seen:
            errors.append(f"duplicate workspace package name: {n}")
        seen.add(n)

# `publish = false` renders as an empty allow-list; such crates must NOT be in
# the publish order. Anything else (null or a registry list) is publishable.
publish_false = {p["name"] for p in ws_pkgs if p.get("publish") == []}
publishable = members - publish_false

# Publish-relevant dependency edges from the manifest: a workspace-internal
# *path* dependency constrains the order, regardless of kind (normal, build, AND
# dev), including optional and target-gated ones. cargo resolves versioned path
# dev-dependencies while packaging a crate, so a path dev-dependency on a
# workspace member must be published first.
#
# This mirrors scripts/publish-crates.sh exactly, which keys on `dep.get("path")`
# (lines 287-295). The `path` check is essential, not just the name: a
# dependency that resolves to an already-published *registry* version of a
# sibling crate (e.g. a dev-dependency `nils-foo = "1.0"`) is reported by
# `cargo metadata` by name but with a `source: registry...` and NO `path`. Such
# a dependency is NOT a publish-order edge — treating it as one (by name alone)
# would reject orders the publisher accepts. (A genuine path dev-only cycle is a
# real publish blocker the topological check below should surface, not hide.)
deps_of = {}
for p in ws_pkgs:
    edges = set()
    for d in p.get("dependencies", []):
        if not d.get("path"):
            continue
        dep_name = d["name"]
        if dep_name in members and dep_name != p["name"]:
            edges.add(dep_name)
    deps_of[p["name"]] = edges

with open(order_path, encoding="utf-8") as fp:
    order = [line.strip() for line in fp if line.strip()]

# Duplicates.
seen = set()
for name in order:
    if name in seen:
        errors.append(f"duplicate publish-order entry: {name}")
    seen.add(name)

# Entries that are not workspace members, or are publish=false.
for name in order:
    if name not in members:
        errors.append(f"publish-order entry is not a workspace member: {name}")
    elif name in publish_false:
        errors.append(f"publish-order lists a publish=false crate: {name}")

# Publishable members missing from the order.
for name in sorted(publishable - set(order)):
    errors.append(f"publishable workspace member missing from publish order: {name}")

# Topological validity: every normal/build workspace dependency must precede
# its dependent in the order.
pos = {name: idx for idx, name in enumerate(order)}
for name in order:
    if name not in members:
        continue
    for dep in sorted(deps_of.get(name, ())):
        if dep not in pos:
            errors.append(
                f"{name} depends on workspace crate {dep}, which is absent from the publish order"
            )
        elif pos[dep] > pos[name]:
            errors.append(
                f"publish order places {name} (#{pos[name]}) before its dependency {dep} (#{pos[dep]})"
            )

if errors:
    for err in errors:
        print(f"FAIL: {err}", file=sys.stderr)
    print(
        f"publish-order-audit: FAIL "
        f"(members={len(members)}, publishable={len(publishable)}, "
        f"order_entries={len(order)}, errors={len(errors)})",
        file=sys.stderr,
    )
    sys.exit(1)

print(
    f"publish-order-audit: OK "
    f"(publishable={len(publishable)} crates, order_entries={len(order)}, "
    f"topological order valid)"
)
PY
