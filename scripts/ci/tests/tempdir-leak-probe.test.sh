#!/usr/bin/env bash
set -euo pipefail

# Self-test for scripts/ci/tempdir-leak-probe.sh.
#
# The probe's whole value is that it fails when a run leaves something behind,
# so that is what this pins. `cargo` is stubbed on PATH: a real workspace run
# would take minutes and could not be made to leak on demand.
#
# Guards:
#   (a) a run that leaves nothing behind passes
#   (b) a run that leaves a directory behind fails, and names its contents —
#       the contents are what identifies the writer
#   (c) a failing test run fails the probe even with no leak
#   (d) a default temp root with Git ancestry is skipped without losing leak
#       visibility
#   (e) --probe-root with any Git ancestry is refused, because tests that
#       assert a path is outside a git repo break under it
#   (f) usage errors exit 2

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: must run inside the nils-cli git work tree" >&2
  exit 2
fi

probe_script="$repo_root/scripts/ci/tempdir-leak-probe.sh"
if [[ ! -f "$probe_script" ]]; then
  echo "error: missing probe script: $probe_script" >&2
  exit 2
fi

has_git_marker_ancestry() {
  local cursor="$1"
  local parent
  while :; do
    if [[ -e "$cursor/.git" || -L "$cursor/.git" ]]; then
      return 0
    fi
    parent="$(dirname "$cursor")"
    if [[ "$parent" == "$cursor" ]]; then
      return 1
    fi
    cursor="$parent"
  done
}

test_root=""
for candidate in /var/tmp /dev/shm "${TMPDIR:-}" /tmp; do
  [[ -n "$candidate" && -d "$candidate" && -w "$candidate" ]] || continue
  candidate="$(cd "$candidate" && pwd -P)"
  if ! has_git_marker_ancestry "$candidate"; then
    test_root="$candidate"
    break
  fi
done
if [[ -z "$test_root" ]]; then
  echo "error: no writable test root outside Git marker ancestry" >&2
  exit 2
fi

work="$(mktemp -d "$test_root/tempdir-leak-probe-test.XXXXXX")"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin" "$work/root"

# Stub `cargo`. $TMPDIR is the private probe directory while it runs, so the
# stub reproduces a leak by writing into it.
write_cargo_stub() {
  local body="$1"
  {
    printf '%s\n' '#!/bin/sh'
    printf '%s\n' "$body"
  } >"$work/bin/cargo"
  chmod +x "$work/bin/cargo"
}

run_probe() {
  PATH="$work/bin:$PATH" bash "$probe_script" --probe-root "$work/root" "$@"
}

expect_status() {
  local expected="$1"
  shift
  local label="$1"
  shift
  local status=0
  run_probe "$@" >"$work/out.txt" 2>&1 || status=$?
  if [[ "$status" -ne "$expected" ]]; then
    echo "FAIL: $label (expected exit $expected, got $status)"
    cat "$work/out.txt"
    exit 1
  fi
  echo "ok: $label"
}

echo "== a clean run passes =="
write_cargo_stub 'exit 0'
expect_status 0 "no leftovers is a pass"

echo "== a leaked directory fails and is named =="
write_cargo_stub 'mkdir -p "$TMPDIR/.tmpLEAKED" && : >"$TMPDIR/.tmpLEAKED/usage.refresh.at"'
expect_status 1 "a leaked directory fails the probe"
if ! grep -q '.tmpLEAKED' "$work/out.txt"; then
  echo "FAIL: probe did not name the leaked entry"
  cat "$work/out.txt"
  exit 1
fi
if ! grep -q 'usage.refresh.at' "$work/out.txt"; then
  echo "FAIL: probe did not report the leaked entry's contents"
  cat "$work/out.txt"
  exit 1
fi
echo "ok: the leaked entry and its contents are reported"

echo "== a leaked file fails too =="
write_cargo_stub ': >"$TMPDIR/.tmpSTRAY.allocation.lock"'
expect_status 1 "a leaked sibling file fails the probe"
if ! grep -q 'allocation.lock' "$work/out.txt"; then
  echo "FAIL: probe did not name the leaked file"
  cat "$work/out.txt"
  exit 1
fi
echo "ok: the leaked file is reported"

echo "== --allow tolerates a named reusable entry =="
write_cargo_stub 'mkdir -p "$TMPDIR/git-cli-test-worker.1000"'
expect_status 1 "an unallowed reusable entry still fails"
expect_status 0 "--allow tolerates the matching entry" --allow 'git-cli-test-worker.*'
write_cargo_stub 'mkdir -p "$TMPDIR/git-cli-test-worker.1000" "$TMPDIR/.tmpREAL"'
expect_status 1 "--allow does not hide a different entry" --allow 'git-cli-test-worker.*'
if ! grep -q '.tmpREAL' "$work/out.txt"; then
  echo "FAIL: allowlist suppressed the unrelated entry"
  cat "$work/out.txt"
  exit 1
fi
echo "ok: the allowlist is scoped to its glob"

echo "== a failing test run fails the probe =="
write_cargo_stub 'exit 1'
expect_status 1 "a failing run fails even with no leak"
if ! grep -q 'the test run failed' "$work/out.txt"; then
  echo "FAIL: probe did not report the failing run"
  cat "$work/out.txt"
  exit 1
fi
echo "ok: a failing run is reported distinctly"

echo "== --runs repeats before checking =="
# shellcheck disable=SC2016 # the stub expands CARGO_STUB_LOG at run time
write_cargo_stub 'echo ran >>"$CARGO_STUB_LOG"'
: >"$work/runs.log"
CARGO_STUB_LOG="$work/runs.log" expect_status 0 "three runs are accepted" --runs 3
if [[ "$(wc -l <"$work/runs.log" | tr -d ' ')" != "3" ]]; then
  echo "FAIL: --runs 3 did not invoke cargo three times"
  cat "$work/runs.log"
  exit 1
fi
echo "ok: --runs repeats the run"

echo "== a polluted default root falls back without losing leak visibility =="
mkdir -p "$work/contaminated/.git"
# shellcheck disable=SC2016 # the stub expands both variables at run time
write_cargo_stub 'printf "%s\n" "$TMPDIR" >"$CARGO_STUB_LOG"; : >"$TMPDIR/.tmpDEFAULT_LEAK"'
status=0
CARGO_STUB_LOG="$work/selected-root.log" \
  PATH="$work/bin:$PATH" \
  TMPDIR="$work/contaminated" \
  XDG_RUNTIME_DIR="$work/contaminated" \
  bash "$probe_script" >"$work/out.txt" 2>&1 || status=$?
if [[ "$status" -ne 1 ]]; then
  echo "FAIL: a leak under the safe default fallback should exit 1, got $status"
  cat "$work/out.txt"
  exit 1
fi
selected_root="$(cat "$work/selected-root.log")"
case "$selected_root/" in
  "$work/contaminated"/*)
    echo "FAIL: the default probe used a root with Git ancestry: $selected_root"
    exit 1
    ;;
esac
if ! grep -q '.tmpDEFAULT_LEAK' "$work/out.txt"; then
  echo "FAIL: the fallback probe did not report the leaked entry"
  cat "$work/out.txt"
  exit 1
fi
echo "ok: a safe fallback remains visible to the leak detector"

echo "== --probe-root with Git ancestry is refused =="
write_cargo_stub 'exit 0'
status=0
PATH="$work/bin:$PATH" bash "$probe_script" --probe-root "$repo_root" >/dev/null 2>&1 || status=$?
if [[ "$status" -ne 2 ]]; then
  echo "FAIL: an in-repo probe root should exit 2, got $status"
  exit 1
fi
status=0
PATH="$work/bin:$PATH" bash "$probe_script" --probe-root "$work/contaminated" >/dev/null 2>&1 || status=$?
if [[ "$status" -ne 2 ]]; then
  echo "FAIL: a probe root below an unrelated Git marker should exit 2, got $status"
  exit 1
fi
echo "ok: probe roots with Git ancestry are refused"

echo "== usage =="
status=0
run_probe --runs 0 >/dev/null 2>&1 || status=$?
if [[ "$status" -ne 2 ]]; then
  echo "FAIL: --runs 0 should exit 2, got $status"
  exit 1
fi
status=0
run_probe --runs abc >/dev/null 2>&1 || status=$?
if [[ "$status" -ne 2 ]]; then
  echo "FAIL: a non-numeric --runs should exit 2, got $status"
  exit 1
fi
status=0
run_probe --runs >/dev/null 2>&1 || status=$?
if [[ "$status" -ne 2 ]]; then
  echo "FAIL: --runs without a value should exit 2, got $status"
  exit 1
fi
status=0
PATH="$work/bin:$PATH" bash "$probe_script" --probe-root "$work/missing-dir" >/dev/null 2>&1 || status=$?
if [[ "$status" -ne 2 ]]; then
  echo "FAIL: a missing probe root should exit 2, got $status"
  exit 1
fi
echo "ok"

echo "tempdir-leak-probe.test.sh: all checks passed"
