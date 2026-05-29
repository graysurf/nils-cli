#!/usr/bin/env bash
set -euo pipefail

# Read-only candidate scanner for stale-code cleanup. It surfaces the cleanup
# surface that the workspace gates (clippy -D warnings, test-stale-audit) do
# NOT already catch:
#   1. unused direct dependencies (via cargo-machete, when available)
#   2. dead-code escape hatches (#[allow(dead_code)] / #[allow(unused*)])
#   3. serde structs with deny_unknown_fields (the field-removal trap)
#
# It never edits files, runs tests, or mutates git state. Every candidate it
# prints is only a CANDIDATE; the compiler is the authority (see SKILL.md).

usage() {
  cat <<'USAGE'
usage: nils-cli-prune-stale-code.sh [--root DIR] [--machete | --no-machete]

Surfaces stale-code cleanup candidates as a Markdown report (read-only).

Options:
  --root DIR     Repository (or subtree) root to scan. Default: the enclosing
                 git work tree. A `crates/` subdir is scanned when present.
  --machete      Force the unused-dependency scan (errors if cargo-machete or
                 a root Cargo.toml is missing).
  --no-machete   Skip the unused-dependency scan.
  -h, --help     Show this help.

Exit codes:
  0  report produced
  2  usage error, missing root, or --machete forced without prerequisites
USAGE
}

root=""
machete="auto"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --root)
      [[ $# -ge 2 ]] || { echo "error: --root requires a path" >&2; exit 2; }
      root="$2"
      shift 2
      ;;
    --machete)
      machete="on"
      shift
      ;;
    --no-machete)
      machete="off"
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "error: unexpected argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [[ -z "$root" ]]; then
  root="$(git -C "$script_dir" rev-parse --show-toplevel 2>/dev/null || true)"
  if [[ -z "$root" ]]; then
    echo "error: not inside a git work tree; pass --root DIR" >&2
    exit 2
  fi
fi
if [[ ! -d "$root" ]]; then
  echo "error: root is not a directory: $root" >&2
  exit 2
fi

scan_dir="$root"
[[ -d "$root/crates" ]] && scan_dir="$root/crates"

# grep -r returns 1 when there are no matches; guard every call so `set -e`
# does not treat "no candidates" as a failure.
rg_rs() { grep -rEn "$1" --include='*.rs' "$scan_dir" 2>/dev/null || true; }

echo "# Stale-Code Cleanup Candidates"
echo
echo "Scan root: \`$root\` (scanned: \`${scan_dir#"$root"/}\`)"
echo
echo "> Every entry below is a CANDIDATE only. Confirm with the compiler before"
echo "> deleting: remove the suppression (or dependency) and rebuild under"
echo "> \`cargo clippy --all-targets --all-features -- -D warnings\`. See SKILL.md."
echo

# ---------------------------------------------------------------------------
# 1. Unused direct dependencies
# ---------------------------------------------------------------------------
echo "## 1. Unused direct dependencies"
echo
run_machete=0
case "$machete" in
  off)
    echo "_Skipped (\`--no-machete\`)._"
    ;;
  on)
    if ! command -v cargo-machete >/dev/null 2>&1; then
      echo "error: --machete forced but cargo-machete is not installed" >&2
      echo "       install with: cargo install --locked cargo-machete" >&2
      exit 2
    fi
    if [[ ! -f "$root/Cargo.toml" ]]; then
      echo "error: --machete forced but no Cargo.toml at root: $root" >&2
      exit 2
    fi
    run_machete=1
    ;;
  auto)
    if command -v cargo-machete >/dev/null 2>&1 && [[ -f "$root/Cargo.toml" ]]; then
      run_machete=1
    else
      echo "_cargo-machete not available (or no root Cargo.toml); skipping._"
      echo
      echo "Install the optional scanner with: \`cargo install --locked cargo-machete\`."
    fi
    ;;
esac

if [[ "$run_machete" -eq 1 ]]; then
  echo '```'
  ( cd "$root" && cargo-machete --skip-target-dir 2>&1 ) || true
  echo '```'
  echo
  echo "> WARNING: machete reports a dependency as unused when its crate name"
  echo "> never appears in source. This MISSES deps imported under a different"
  echo "> lib name (e.g. package \`nils-agent-out\` imported as \`agent_out\`)."
  echo "> Never remove a dependency on machete's word alone — the compiler must"
  echo "> confirm it after removal."
fi
echo

# ---------------------------------------------------------------------------
# 2. Dead-code suppressions (escape hatches)
# ---------------------------------------------------------------------------
echo "## 2. Dead-code suppressions"
echo
all_allows="$(rg_rs 'allow\((dead_code|unused[a-z_]*)\)')"

src_allows="$(printf '%s\n' "$all_allows" | grep -v '/tests/' | sed '/^$/d' || true)"
test_allows="$(printf '%s\n' "$all_allows" | grep '/tests/' | sed '/^$/d' || true)"

src_count="$(printf '%s\n' "$src_allows" | sed '/^$/d' | wc -l | tr -d ' ')"
test_count="$(printf '%s\n' "$test_allows" | sed '/^$/d' | wc -l | tr -d ' ')"

echo "### 2a. In production source (highest signal): ${src_count}"
echo
if [[ -n "$src_allows" ]]; then
  echo '```'
  printf '%s\n' "$src_allows" | sed "s#${root}/##"
  echo '```'
else
  echo "_None._"
fi
echo
echo "### 2b. In tests (usually legitimate helper-fanout — verify, do not bulk-delete): ${test_count}"
echo
if [[ -n "$test_allows" ]]; then
  echo '```'
  printf '%s\n' "$test_allows" | sed "s#${root}/##"
  echo '```'
else
  echo "_None._"
fi
echo

# ---------------------------------------------------------------------------
# 3. serde deny_unknown_fields (field-removal trap)
# ---------------------------------------------------------------------------
echo "## 3. serde \`deny_unknown_fields\` structs (field-removal trap)"
echo
deny_files="$(grep -rln 'deny_unknown_fields' --include='*.rs' "$scan_dir" 2>/dev/null | sed "s#${root}/##" | sort || true)"
if [[ -n "$deny_files" ]]; then
  echo "Removing a parsed-but-unread field from a struct in one of these files"
  echo "CHANGES PARSE BEHAVIOR (previously-accepted input would now be rejected)."
  echo "Treat such fields as \`keep\`, not dead code:"
  echo
  echo '```'
  printf '%s\n' "$deny_files"
  echo '```'
else
  echo "_No \`deny_unknown_fields\` structs found; parsed-but-unread fields are"
  echo "functionally inert and safe to drop once the compiler confirms._"
fi
