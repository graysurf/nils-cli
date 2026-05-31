#!/usr/bin/env bash
# scripts/ci/crate-naming-audit.sh — enforce the workspace crate/CLI naming
# convention documented in docs/specs/crate-cli-naming-convention-v1.md.
#
# Rules (see the spec for rationale):
#   - `[package].name` == "nils-<dir>", unless <dir> already starts with
#     "nils-" (then the package name == <dir>; no double prefix).
#   - Each `[[bin]].name` == <dir> (the crate directory name).
#   - A small, explicit allowlist grandfathers existing crates whose published
#     package/binary names predate this convention. New crates MUST comply or
#     extend the allowlist here AND in the spec, with justification.
#
# Compatibility: must run on macOS (system bash 3.2) and Linux runners. Avoid
# associative arrays, mapfile, and `${var,,}` lowercasing.

set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  bash scripts/ci/crate-naming-audit.sh [--help]

Audits every crate under crates/ against the workspace crate/CLI naming
convention. Exits 0 when all crates comply (or are allowlisted), 1 on any
violation, 2 on usage error.
USAGE
}

case "${1:-}" in
  -h | --help)
    usage
    exit 0
    ;;
  "") ;;
  *)
    echo "crate-naming-audit: unexpected argument: $1" >&2
    usage >&2
    exit 2
    ;;
esac

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

# --- Grandfathered exceptions ------------------------------------------------
# Package names that intentionally diverge from "nils-<dir>".
pkg_exception() {
  case "$1" in
    *) return 1 ;;
  esac
}

# Allowlisted binary names per crate dir (space-separated), for crates whose
# published binaries diverge from the "<bin> == <dir>" rule.
allowed_bins_for() {
  case "$1" in
    plan-issue-cli) echo "plan-issue plan-issue-local" ;;
    nils-markdown) echo "md-render" ;;
    agent-workflow-primitives)
      echo "agent-run browser-session canary-check docs-impact heuristic-inbox model-cross-check review-evidence review-specialists repo-retro skill-usage test-first-evidence"
      ;;
    *) echo "" ;;
  esac
}

# --- Cargo.toml parsing (awk; no toml dependency) ----------------------------
pkg_name() {
  awk -F'"' '
    /^\[package\]/ { inpkg = 1; next }
    /^\[/          { inpkg = 0 }
    inpkg && /^name[ ]*=/ { print $2; exit }
  ' "$1"
}

bin_names() {
  awk -F'"' '
    /^\[\[bin\]\]/ { inbin = 1; next }
    /^\[/          { inbin = 0 }
    inbin && /^name[ ]*=/ { print $2; inbin = 0 }
  ' "$1"
}

violations=0
checked=0

for manifest in crates/*/Cargo.toml; do
  [ -f "$manifest" ] || continue
  dir="$(basename "$(dirname "$manifest")")"
  checked=$((checked + 1))

  # Expected package name.
  case "$dir" in
    nils-*) expected_pkg="$dir" ;;
    *) expected_pkg="nils-$dir" ;;
  esac

  actual_pkg="$(pkg_name "$manifest")"
  if [ "$actual_pkg" != "$expected_pkg" ]; then
    if pkg_exception "$dir"; then
      :
    else
      echo "crate-naming-audit: $dir: package name '$actual_pkg' should be '$expected_pkg'" >&2
      violations=$((violations + 1))
    fi
  fi

  # Binary names: each must equal the dir, or be in this crate's allowlist.
  allowed="$(allowed_bins_for "$dir")"
  while IFS= read -r bin; do
    [ -n "$bin" ] || continue
    [ "$bin" = "$dir" ] && continue
    ok=0
    for a in $allowed; do
      [ "$bin" = "$a" ] && { ok=1; break; }
    done
    if [ "$ok" -ne 1 ]; then
      echo "crate-naming-audit: $dir: binary name '$bin' should equal the crate dir '$dir' (or be allowlisted)" >&2
      violations=$((violations + 1))
    fi
  done <<EOF
$(bin_names "$manifest")
EOF
done

if [ "$violations" -ne 0 ]; then
  echo "crate-naming-audit: FAIL ($violations violation(s) across $checked crate(s))" >&2
  echo "  See docs/specs/crate-cli-naming-convention-v1.md; new crates must comply or extend the allowlist with justification." >&2
  exit 1
fi

echo "crate-naming-audit: OK ($checked crates compliant)"
