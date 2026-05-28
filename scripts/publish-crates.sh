#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  scripts/publish-crates.sh [options]

Options:
  --dry-run            Validate with `cargo publish --dry-run` (default).
  --publish            Publish to registry after dry-run passes for all crates.
  --crate NAME         Add one crate (repeatable).
  --crates "A B,C"     Add multiple crates (space/comma separated).
  --list-file PATH     Read default crate order from a file.
  --registry NAME      Optional cargo registry name (blank = crates.io).
  --skip-existing      In --publish mode, skip crates already published at this version (default; crates.io only).
  --no-skip-existing   In --publish mode, fail if a crate version already exists.
  --publish-wait-timeout-seconds N
                       Max seconds to wait for a published crates.io version to become visible (default: 300).
  --publish-poll-seconds N
                       Poll interval while waiting for a published crates.io version (default: 10).
  --allow-dirty        Allow a dirty working tree when mode is --publish.
  -h, --help           Show help.

Default crate list file:
  release/crates-io-publish-order.txt
USAGE
}

die() {
  echo "error: $*" >&2
  exit 1
}

note() {
  echo "info: $*" >&2
}

contains() {
  local needle="$1"
  shift
  local item
  for item in "$@"; do
    if [[ "$item" == "$needle" ]]; then
      return 0
    fi
  done
  return 1
}

selected_crate_version() {
  local metadata_path="$1"
  local crate="$2"
  python3 - "$metadata_path" "$crate" <<'PY'
from __future__ import annotations

import json
import sys

metadata_path, crate = sys.argv[1], sys.argv[2]
with open(metadata_path, "r", encoding="utf-8") as fp:
    metadata = json.load(fp)
for pkg in metadata["packages"]:
    if pkg.get("name") == crate:
        print(pkg["version"])
        raise SystemExit(0)
raise SystemExit(1)
PY
}

crate_version_exists_on_crates_io() {
  local crate="$1"
  local version="$2"
  python3 - "$crates_io_api_base" "$crate" "$version" <<'PY'
from __future__ import annotations

import sys
import urllib.error
import urllib.parse
import urllib.request

api_base, crate, version = sys.argv[1], sys.argv[2], sys.argv[3]
url = (
    f"{api_base.rstrip('/')}/crates/"
    f"{urllib.parse.quote(crate, safe='')}/"
    f"{urllib.parse.quote(version, safe='')}"
)
try:
    urllib.request.urlopen(url, timeout=15)
except urllib.error.HTTPError as exc:
    if exc.code == 404:
        raise SystemExit(1)
    print(f"error: crates.io lookup failed for {crate} v{version}: HTTP {exc.code}", file=sys.stderr)
    raise SystemExit(2)
except urllib.error.URLError as exc:
    print(f"error: crates.io lookup failed for {crate} v{version}: {exc.reason}", file=sys.stderr)
    raise SystemExit(2)
raise SystemExit(0)
PY
}

wait_for_crate_version_on_crates_io() {
  local crate="$1"
  local version="$2"
  local timeout_seconds="$3"
  local poll_seconds="$4"
  local elapsed=0
  local rc=0

  note "waiting for ${crate} v${version} to be visible on crates.io"
  while (( elapsed <= timeout_seconds )); do
    set +e
    crate_version_exists_on_crates_io "$crate" "$version"
    rc="$?"
    set -e

    case "$rc" in
      0)
        note "confirmed ${crate} v${version} on crates.io"
        return 0
        ;;
      1)
        ;;
      *)
        note "crates.io lookup for ${crate} v${version} failed with exit code ${rc}; retrying"
        ;;
    esac

    if (( elapsed >= timeout_seconds )); then
      break
    fi
    sleep "$poll_seconds"
    elapsed=$((elapsed + poll_seconds))
  done

  return 1
}

append_crates_from_words() {
  local raw="$1"
  local item
  raw="${raw//,/ }"
  for item in $raw; do
    [[ -n "$item" ]] || continue
    selected_crates+=("$item")
  done
}

append_crates_from_file() {
  local path="$1"
  [[ -f "$path" ]] || die "default crate list not found: $path"

  local line trimmed
  while IFS= read -r line || [[ -n "$line" ]]; do
    trimmed="$(printf '%s' "$line" | sed -E 's/[[:space:]]*#.*$//; s/^[[:space:]]+//; s/[[:space:]]+$//')"
    [[ -n "$trimmed" ]] || continue
    selected_crates+=("$trimmed")
  done < "$path"
}

mode="dry-run"
allow_dirty=0
list_file="release/crates-io-publish-order.txt"
registry=""
skip_existing=1
publish_wait_timeout_seconds=300
publish_poll_seconds=10
crates_io_api_base="${PUBLISH_CRATES_API_BASE:-https://crates.io/api/v1}"
declare -a selected_crates=()

while [[ $# -gt 0 ]]; do
  case "${1:-}" in
    --dry-run)
      mode="dry-run"
      shift
      ;;
    --publish)
      mode="publish"
      shift
      ;;
    --crate)
      [[ $# -ge 2 ]] || die "--crate requires a value"
      selected_crates+=("${2:-}")
      shift 2
      ;;
    --crates)
      [[ $# -ge 2 ]] || die "--crates requires a value"
      append_crates_from_words "${2:-}"
      shift 2
      ;;
    --list-file)
      [[ $# -ge 2 ]] || die "--list-file requires a value"
      list_file="${2:-}"
      shift 2
      ;;
    --registry)
      [[ $# -ge 2 ]] || die "--registry requires a value"
      registry="${2:-}"
      shift 2
      ;;
    --skip-existing)
      skip_existing=1
      shift
      ;;
    --no-skip-existing)
      skip_existing=0
      shift
      ;;
    --publish-wait-timeout-seconds)
      [[ $# -ge 2 ]] || die "--publish-wait-timeout-seconds requires a value"
      publish_wait_timeout_seconds="${2:-}"
      shift 2
      ;;
    --publish-poll-seconds)
      [[ $# -ge 2 ]] || die "--publish-poll-seconds requires a value"
      publish_poll_seconds="${2:-}"
      shift 2
      ;;
    --allow-dirty)
      allow_dirty=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "unknown argument: ${1:-}"
      ;;
  esac
done

[[ "$publish_wait_timeout_seconds" =~ ^[0-9]+$ ]] || die "--publish-wait-timeout-seconds must be an integer >= 0"
[[ "$publish_poll_seconds" =~ ^[0-9]+$ ]] || die "--publish-poll-seconds must be an integer >= 0"
(( publish_poll_seconds > 0 )) || die "--publish-poll-seconds must be > 0"

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
[[ -n "$repo_root" ]] || die "must run inside a git work tree"
cd "$repo_root"

if [[ ${#selected_crates[@]} -eq 0 ]]; then
  append_crates_from_file "$list_file"
fi

declare -a deduped_crates=()
for crate in "${selected_crates[@]}"; do
  [[ "$crate" =~ ^[A-Za-z0-9_-]+$ ]] || die "invalid crate name: '$crate'"
  if ! contains "$crate" "${deduped_crates[@]}"; then
    deduped_crates+=("$crate")
  fi
done
selected_crates=("${deduped_crates[@]}")

[[ ${#selected_crates[@]} -gt 0 ]] || die "no crates selected"

metadata_file="$(mktemp)"
trap 'rm -f "$metadata_file"' EXIT
cargo metadata --format-version 1 --no-deps > "$metadata_file"

python3 - "$metadata_file" "${selected_crates[@]}" <<'PY'
from __future__ import annotations

import json
import sys

metadata_path = sys.argv[1]
selected = sys.argv[2:]
with open(metadata_path, "r", encoding="utf-8") as fp:
    metadata = json.load(fp)
packages = {pkg["name"]: pkg for pkg in metadata["packages"]}

errors: list[str] = []

for name in selected:
    pkg = packages.get(name)
    if pkg is None:
        errors.append(f"selected crate '{name}' is not in this workspace")
        continue
    if pkg.get("publish") == []:
        errors.append(f"selected crate '{name}' has publish=false")

order = {name: idx for idx, name in enumerate(selected)}
for name in selected:
    pkg = packages.get(name)
    if pkg is None:
        continue
    for dep in pkg.get("dependencies", []):
        dep_name = dep.get("name")
        if dep.get("path") and packages.get(dep_name, {}).get("publish") == []:
            errors.append(
                f"selected crate '{name}' depends on workspace crate '{dep_name}' "
                "which has publish=false"
            )
        if dep.get("path") and dep_name in order and order[dep_name] > order[name]:
            errors.append(
                f"publish order invalid: '{name}' depends on '{dep_name}', "
                "so dependency must appear earlier in the crate list"
            )

if errors:
    for err in errors:
        print(f"error: {err}", file=sys.stderr)
    raise SystemExit(1)
PY

if [[ "$mode" == "publish" && "$allow_dirty" -eq 0 ]]; then
  if [[ -n "$(git status --porcelain)" ]]; then
    die "working tree is not clean; commit/stash changes or use --allow-dirty"
  fi
fi

declare -a cargo_args=(--locked)
if [[ -n "$registry" ]]; then
  cargo_args+=(--registry "$registry")
fi
if [[ "$allow_dirty" -eq 1 ]]; then
  cargo_args+=(--allow-dirty)
fi

note "mode: $mode"
note "crates: ${selected_crates[*]}"
if [[ -n "$registry" ]]; then
  note "registry: $registry"
else
  note "registry: crates.io (default)"
fi

if [[ "$mode" == "publish" ]]; then
  for crate in "${selected_crates[@]}"; do
    version="$(selected_crate_version "$metadata_file" "$crate")" \
      || die "failed to resolve version for crate '$crate'"
    if [[ "$skip_existing" -eq 1 && -z "$registry" ]]; then
      set +e
      crate_version_exists_on_crates_io "$crate" "$version"
      exists_rc="$?"
      set -e
      case "$exists_rc" in
        0)
          note "[publish] skip ${crate} v${version} (already published on crates.io)"
          continue
          ;;
        1)
          ;;
        *)
          die "failed to check whether ${crate} v${version} exists on crates.io"
          ;;
      esac
    fi
    note "[dry-run] cargo publish -p ${crate} --dry-run ${cargo_args[*]}"
    cargo publish -p "$crate" --dry-run "${cargo_args[@]}"
    note "[publish] cargo publish -p ${crate} ${cargo_args[*]}"
    cargo publish -p "$crate" "${cargo_args[@]}"
    if [[ -z "$registry" ]]; then
      wait_for_crate_version_on_crates_io \
        "$crate" \
        "$version" \
        "$publish_wait_timeout_seconds" \
        "$publish_poll_seconds" \
        || die "timed out waiting for ${crate} v${version} to become visible on crates.io"
    fi
  done
  note "publish finished for: ${selected_crates[*]}"
else
  for crate in "${selected_crates[@]}"; do
    note "[dry-run] cargo publish -p ${crate} --dry-run ${cargo_args[*]}"
    cargo publish -p "$crate" --dry-run "${cargo_args[@]}"
  done
  note "dry-run finished for: ${selected_crates[*]}"
fi
