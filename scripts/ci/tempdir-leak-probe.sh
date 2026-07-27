#!/usr/bin/env bash
# scripts/ci/tempdir-leak-probe.sh — run a test selection against a private,
# empty TMPDIR and fail if anything survives the run.
#
# This is the runtime counterpart to tempdir-leak-audit.sh. The audit greps for
# handles whose cleanup can *never* run; it cannot see the two classes that
# actually dominated this workspace:
#
#   * cleanup runs, then a detached child re-creates the tree. `write_atomic`
#     calls `create_dir_all` before writing, so a child that writes one marker
#     after teardown resurrects the whole fixture directory.
#   * a lock or marker is written as a *sibling* of the fixture root, so it
#     lands next to the `TempDir` in `$TMPDIR` where teardown never reaches.
#
# Neither is visible by inspection and neither fails a test, so the only honest
# detector is to give the run an empty directory and look at what is left.
#
# Why a private TMPDIR and not a diff of the shared /tmp: the shared directory
# has concurrent writers (editors, agents, the user's own shells), so a
# before/after diff reports their entries as leaks. It also hides the real ones
# — temp directories are dotfiles, and a plain `ls` does not list them.
#
# Do NOT point TMPDIR inside the repository work tree. Many tests assert that a
# path is outside a git repository (`*_outside_git_repo`, `not_a_repo`), and a
# TMPDIR under the checkout fails 131 of them across ~25 crates. The probe root
# therefore defaults to the system temp directory.
#
# Compatibility: must run on macOS (system bash 3.2) and Linux runners. Avoid
# associative arrays, mapfile, and `${var,,}` lowercasing.

set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  bash scripts/ci/tempdir-leak-probe.sh [--runs <n>] [--probe-root <dir>]
                                        [--] [<cargo nextest args>...]

Runs `cargo nextest run` with TMPDIR pointed at a private empty directory and
fails if the run leaves anything behind. Defaults to `--workspace` when no
nextest arguments are given.

  --runs <n>         repeat the run <n> times before checking (default: 1).
                     Several leaks are races that only lose sometimes; repeat
                     to raise the odds of catching one.
  --probe-root <dir> where to create the private TMPDIR (default: the system
                     temp directory). Must not be inside the repository.
  --allow <glob>     tolerate a surviving entry whose name matches <glob>.
                     Repeatable. Only for state a test deliberately reuses
                     across runs under a fixed name — such state is bounded, so
                     it does not grow the way a leak does. Never use it to
                     silence a randomly-named directory.

Exits 0 when nothing is left behind, 1 on a leak or a failing test run, 2 on a
usage error.
USAGE
}

runs=1
probe_root="${TMPDIR:-/tmp}"
allow_globs=()
while [ $# -gt 0 ]; do
  case "$1" in
    -h | --help)
      usage
      exit 0
      ;;
    --allow)
      shift
      if [ $# -eq 0 ]; then
        echo "tempdir-leak-probe: --allow requires a glob" >&2
        exit 2
      fi
      allow_globs+=("$1")
      shift
      ;;
    --runs)
      shift
      if [ $# -eq 0 ]; then
        echo "tempdir-leak-probe: --runs requires a count" >&2
        exit 2
      fi
      runs="$1"
      shift
      ;;
    --probe-root)
      shift
      if [ $# -eq 0 ]; then
        echo "tempdir-leak-probe: --probe-root requires a path" >&2
        exit 2
      fi
      probe_root="$1"
      shift
      ;;
    --)
      shift
      break
      ;;
    -*)
      break
      ;;
    *)
      break
      ;;
  esac
done

case "$runs" in
  '' | *[!0-9]*)
    echo "tempdir-leak-probe: --runs must be a positive integer: $runs" >&2
    exit 2
    ;;
esac
if [ "$runs" -lt 1 ]; then
  echo "tempdir-leak-probe: --runs must be at least 1" >&2
  exit 2
fi
if [ ! -d "$probe_root" ]; then
  echo "tempdir-leak-probe: not a directory: $probe_root" >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
probe_root_abs="$(cd "$probe_root" && pwd)"
case "$probe_root_abs/" in
  "$repo_root"/*)
    echo "tempdir-leak-probe: --probe-root must be outside the repository;" >&2
    echo "  tests that assert a path is outside a git repo fail under it." >&2
    exit 2
    ;;
esac

if [ $# -eq 0 ]; then
  set -- --workspace
fi

probe_dir="$(mktemp -d "$probe_root_abs/tempdir-leak-probe.XXXXXX")"
cleanup() {
  rm -rf "$probe_dir"
}
trap cleanup EXIT

cd "$repo_root"

run_status=0
i=1
while [ "$i" -le "$runs" ]; do
  if [ "$runs" -gt 1 ]; then
    echo "tempdir-leak-probe: run $i/$runs"
  fi
  if ! TMPDIR="$probe_dir" cargo nextest run "$@"; then
    run_status=1
  fi
  i=$((i + 1))
done

is_allowed() {
  candidate="$1"
  for glob in ${allow_globs+"${allow_globs[@]}"}; do
    # shellcheck disable=SC2053 # glob match is the point
    if [[ "$candidate" == $glob ]]; then
      return 0
    fi
  done
  return 1
}

leaked=""
while IFS= read -r entry; do
  [ -n "$entry" ] || continue
  if is_allowed "$entry"; then
    echo "tempdir-leak-probe: allowed reusable entry: $entry"
    continue
  fi
  leaked="${leaked}${entry}
"
done <<EOF
$(ls -A "$probe_dir" 2>/dev/null || true)
EOF
leaked="$(printf '%s' "$leaked")"

if [ -n "$leaked" ]; then
  echo "" >&2
  echo "tempdir-leak-probe: the test run left these behind in its TMPDIR:" >&2
  echo "" >&2
  printf '%s\n' "$leaked" |
    while IFS= read -r entry; do
      if [ -d "$probe_dir/$entry" ]; then
        contents="$(ls -A "$probe_dir/$entry" 2>/dev/null | tr '\n' ' ')"
        printf '  %s/  contains: %s\n' "$entry" "${contents:-<empty>}"
      else
        printf '  %s\n' "$entry"
      fi
    done >&2
  echo "" >&2
  echo "The contents above identify the writer. Find the test that produces" >&2
  echo "them, then make it wait for the background work to finish before its" >&2
  echo "fixture drops, or give the writer a parent it owns inside the fixture." >&2
  echo "See docs/specs/test-temp-directory-policy.md." >&2
  exit 1
fi

if [ "$run_status" -ne 0 ]; then
  echo "tempdir-leak-probe: no leak, but the test run failed" >&2
  exit 1
fi

echo "tempdir-leak-probe: ok — nothing left behind"
