#!/usr/bin/env bash
# scripts/ci/tempdir-leak-audit.sh — fail the build on temp-directory patterns
# whose cleanup can never run.
#
# Three patterns are rejected:
#
#   1. A `TempDir` owned by a `static` (`OnceLock`, `LazyLock`, `Lazy`, ...).
#      Rust never drops statics, so the directory is never removed. Under
#      `cargo nextest`, which runs one process per test, this leaks once per
#      test rather than once per test binary.
#   2. `.keep()` / `.into_path()`, which disarm cleanup by design and hand back
#      a bare path.
#   3. `mem::forget`, which skips the destructor outright.
#
# This audit exists because `tempfile::TempDir`'s own `Drop` discards the
# `remove_dir_all` result: every failure above is invisible in test output and
# CI logs, so the workspace accumulated hundreds of gigabytes under /tmp before
# anyone noticed. Prefer `nils_test_support::tempdir::ScopedTempDir`, which
# reports a cleanup failure instead of hiding it.
#
# Escape hatch: put `tempdir-leak-audit: allow` in a comment on the offending
# line or the line directly above it, together with the reason.
#
# Not a rule here, and #1411 asked for the evaluation rather than the guess: a
# read-only *directory* whose restore is a plain statement is the class-2 hazard,
# and a chmod-literal grep cannot gate it. What decides is the chmod *target*,
# which a line-oriented match cannot see. Measured on this workspace:
#
#   * `0o[45]xx` matched 4 sites, **none** of them the hazard — every one was a
#     file mode or a permanent hardening, while a real `0o000` directory fixture
#     was accepted;
#   * widening to any owner digit but 7 — which is the true predicate, since
#     emptying a directory needs write *and* execute — matched 154 sites,
#     overwhelmingly legitimate file modes in production code;
#   * scoping that to `crates/*/tests/` still matched 78, again mostly files.
#
# So the class stays a review rule and a policy statement rather than a gate that
# would train authors to annotate safe code. `nils_test_support::tempdir::RestoredMode`
# is the discoverable fix. See docs/specs/test-temp-directory-policy.md.
#
# Scope limit: this only catches leaks where cleanup *never runs*. A leak where
# cleanup runs and then a detached child process recreates the directory is not
# detectable by inspection; that one is a test-authoring rule. See
# docs/specs/test-temp-directory-policy.md for all four leak classes.
#
# Compatibility: must run on macOS (system bash 3.2) and Linux runners. Avoid
# associative arrays, mapfile, and `${var,,}` lowercasing.

set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  bash scripts/ci/tempdir-leak-audit.sh [--root <dir>] [--help]

Scans <dir>/crates/ for temp-directory handles whose cleanup can never run.
--root defaults to the repository root and exists so the self-test can point the
audit at a fixture tree. Exits 0 when clean, 1 on any violation, 2 on usage
error.
USAGE
}

root=""
while [ $# -gt 0 ]; do
  case "$1" in
    -h | --help)
      usage
      exit 0
      ;;
    --root)
      shift
      if [ $# -eq 0 ]; then
        echo "tempdir-leak-audit: --root requires a path" >&2
        exit 2
      fi
      root="$1"
      shift
      ;;
    *)
      echo "tempdir-leak-audit: unexpected argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [ -z "$root" ]; then
  root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fi
if [ ! -d "$root" ]; then
  echo "tempdir-leak-audit: not a directory: $root" >&2
  exit 2
fi
cd "$root"

if ! command -v rg >/dev/null 2>&1; then
  echo "tempdir-leak-audit: ripgrep (rg) is required" >&2
  exit 2
fi

allow_marker='tempdir-leak-audit: allow'
violations="$(mktemp "${TMPDIR:-/tmp}/tempdir-leak-audit.XXXXXX")"
trap 'rm -f "$violations"' EXIT

scan() {
  description="$1"
  pattern="$2"

  # `rg` exits 1 when a pattern has no matches, which is the expected result
  # here; keep that out of `set -e`/`pipefail`.
  matches="$(rg --line-number --no-heading --color never --type rust -e "$pattern" crates/ 2>/dev/null || true)"
  [ -n "$matches" ] || return 0

  printf '%s\n' "$matches" |
    while IFS= read -r hit; do
      file="${hit%%:*}"
      rest="${hit#*:}"
      lineno="${rest%%:*}"
      text="${rest#*:}"

      case "$text" in
        *"$allow_marker"*) continue ;;
      esac
      if [ "$lineno" -gt 1 ]; then
        previous="$(sed -n "$((lineno - 1))p" "$file")"
        case "$previous" in
          *"$allow_marker"*) continue ;;
        esac
      fi

      trimmed="$(printf '%s' "$text" | sed 's/^[[:space:]]*//')"
      printf '%s:%s: %s\n      %s\n' \
        "$file" "$lineno" "$description" "$trimmed" >>"$violations"
    done
}

scan "a TempDir owned by a static is never dropped" \
  'static[[:space:]]+[A-Za-z0-9_]+[[:space:]]*:[^=]*TempDir'
scan "keep() disarms temp-directory cleanup" \
  '\.keep\(\)'
scan "into_path() disarms temp-directory cleanup" \
  '\.into_path\(\)'
scan "mem::forget skips temp-directory cleanup" \
  'mem::forget'

if [ -s "$violations" ]; then
  echo "tempdir-leak-audit: temp-directory cleanup can never run at these sites:" >&2
  echo "" >&2
  cat "$violations" >&2
  echo "" >&2
  echo "Hold the handle in a value that drops, or use" >&2
  echo "nils_test_support::tempdir::ScopedTempDir. If the leak is deliberate," >&2
  echo "annotate the line with '${allow_marker} — <reason>'." >&2
  exit 1
fi

echo "tempdir-leak-audit: ok"
