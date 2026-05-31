#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  scripts/ci/workspace-version-lockstep.sh [--strict]

Verifies that every `crates/*/Cargo.toml` `version =` line matches the
workspace root `Cargo.toml` `version =`, and that every internal cross-dep
pin (the `version = "X.Y.Z"` field on a `path = "../<crate>"` dependency)
matches the same workspace version. Catches the "partial bump" anti-pattern
that broke v0.25.7: only `plan-tooling` + `plan-issue` jumped while the
other 31 crates stayed behind, silently breaking downstream consumers that
treat the release tag as a workspace-wide floor.

The lock-step contract is the release convention from `1edf007`
(`chore(release): bump cli versions to 0.25.6`): every release tag matches
every crate's Cargo.toml version. This script makes that contract a CI
invariant rather than a release-time hope.

Options:
  --strict   Treat any drift as a hard failure (exit 1). Without --strict
             the script still prints diagnostics but exits 0 so it can be
             run as a soft probe.
  -h, --help Show this help
USAGE
}

strict=0
while [[ $# -gt 0 ]]; do
  case "${1:-}" in
    --strict)
      strict=1
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

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: must run inside a git work tree" >&2
  exit 2
fi
cd "$repo_root"

workspace_cargo="Cargo.toml"
if [[ ! -f "$workspace_cargo" ]]; then
  echo "error: missing workspace $workspace_cargo" >&2
  exit 2
fi

workspace_version="$(awk '
  /^\[(workspace\.)?package\]/ { in_pkg = 1; next }
  /^\[/ { in_pkg = 0 }
  in_pkg && /^version[[:space:]]*=/ {
    if (match($0, /"[^"]*"/)) {
      print substr($0, RSTART + 1, RLENGTH - 2)
      exit
    }
  }
' "$workspace_cargo")"

if [[ -z "$workspace_version" ]]; then
  # Fall back to first top-level `version = "..."` line if the file has no
  # `[package]` section (rare for the workspace root, but handle gracefully).
  workspace_version="$(awk '
    /^\[/ { section = $0 }
    /^version[[:space:]]*=/ && section == "" {
      if (match($0, /"[^"]*"/)) {
        print substr($0, RSTART + 1, RLENGTH - 2)
        exit
      }
    }
  ' "$workspace_cargo")"
fi

if [[ -z "$workspace_version" ]]; then
  echo "error: could not parse workspace version from $workspace_cargo" >&2
  exit 2
fi

drift_count=0
declare -a drift_lines=()

# Crate version mismatches.
while IFS= read -r -d '' cargo; do
  crate_version="$(awk '
    /^\[package\]/ { in_pkg = 1; next }
    /^\[/ { in_pkg = 0 }
    in_pkg && /^version[[:space:]]*=/ {
      if (match($0, /"[^"]*"/)) {
        print substr($0, RSTART + 1, RLENGTH - 2)
        exit
      }
    }
  ' "$cargo")"
  if [[ -z "$crate_version" ]]; then
    drift_count=$((drift_count + 1))
    drift_lines+=("  [missing-version] $cargo")
    continue
  fi
  if [[ "$crate_version" != "$workspace_version" ]]; then
    drift_count=$((drift_count + 1))
    drift_lines+=("  [crate-version] $cargo: $crate_version != $workspace_version")
  fi
done < <(find crates -maxdepth 2 -name Cargo.toml -print0 2>/dev/null)

# Internal cross-dep pins: any `version = "<vX.Y.Z>"` field on a `path = "../<crate>"`
# dependency line must equal the workspace version.
while IFS= read -r line; do
  # Format: <path>:<lineno>:<full line>
  file="${line%%:*}"
  rest="${line#*:}"
  lineno="${rest%%:*}"
  body="${rest#*:}"
  # Skip if the line has no `version = "..."` substring (defensive — the grep
  # below already filters but we want to be robust to edge cases).
  if ! [[ "$body" =~ version[[:space:]]*=[[:space:]]*\"[^\"]+\" ]]; then
    continue
  fi
  # Skip if the line has no `path = "../"` substring (we only validate internal
  # cross-deps; external registry deps are out of scope for this gate).
  if ! [[ "$body" =~ path[[:space:]]*=[[:space:]]*\"\.\.\/ ]]; then
    continue
  fi
  ver="$(printf '%s\n' "$body" | sed -E 's/.*version[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/')"
  if [[ "$ver" != "$workspace_version" ]]; then
    drift_count=$((drift_count + 1))
    drift_lines+=("  [internal-dep-pin] $file:$lineno: $ver != $workspace_version")
  fi
done < <(grep -nE 'version[[:space:]]*=[[:space:]]*"[0-9]+\.[0-9]+\.[0-9]+"' crates/*/Cargo.toml 2>/dev/null | grep -E 'path[[:space:]]*=[[:space:]]*"\.\./' || true)

if [[ "$drift_count" -gt 0 ]]; then
  echo "FAIL: workspace version lockstep drift (workspace=$workspace_version)"
  for line in "${drift_lines[@]}"; do
    echo "$line"
  done
  echo
  echo "  Remediation:"
  echo "  - The nils-cli workspace bumps every crate in lock-step per the"
  echo "    'chore(release): bump cli versions to vX.Y.Z' convention. Either:"
  echo "    (a) bump every drifted crate to $workspace_version, or"
  echo "    (b) revert the partial bump and open a workspace-wide bump PR."
  echo "  - Downstream consumers (e.g. agent-runtime-kit ci/all.sh Position 2)"
  echo "    treat the release tag as a workspace-wide floor; a partial bump"
  echo "    silently breaks that assumption."
  if [[ "$strict" -eq 1 ]]; then
    exit 1
  fi
  exit 0
fi

echo "PASS: workspace version lockstep audit (workspace=$workspace_version, drift=0)"
