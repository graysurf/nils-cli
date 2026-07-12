#!/usr/bin/env bash
set -euo pipefail

# Self-test for scripts/ci/docs-hygiene-audit.sh.
#
# The audit has no fixture-injection flag: it resolves the repo root via
# `git rev-parse --show-toplevel` and scans `docs` / `crates/*/docs`. So each
# case runs the real script black-box inside a throwaway `git init` repo whose
# only populated tree is `docs/`; every other guardrail (transient-doc,
# legacy-keyword, removed-surface scans) is a no-op against the empty fixture,
# leaving the duplicate-hash block as the behavior under test.
#
# Coverage:
#   - distinct payloads pass;
#   - identical payloads across paths are reported as duplicates;
#   - a hash command that exits non-zero is surfaced (not swallowed) -- the
#     branch hardened after the shasum/sha1sum fallback landed;
#   - neither shasum nor sha1sum on PATH is a hard error.

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: must run inside the nils-cli git work tree" >&2
  exit 2
fi

script="$repo_root/scripts/ci/docs-hygiene-audit.sh"
if [[ ! -f "$script" ]]; then
  echo "error: missing audit script: $script" >&2
  exit 2
fi

verify_script="$repo_root/.agents/skills/project-verify-required-checks/scripts/project-verify-required-checks.sh"
if [[ ! -f "$verify_script" ]]; then
  echo "error: missing required checks script: $verify_script" >&2
  exit 2
fi

bash_bin="$(command -v bash)"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/docs-hygiene-audit-test.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

# make_repo <name> -> prints the path to a fresh git repo with an empty docs/.
make_repo() {
  local dir="$tmp_dir/$1"
  mkdir -p "$dir/docs"
  git init -q "$dir"
  printf '%s' "$dir"
}

# run_audit <dir> [path_override] -> sets globals `audit_output` and `status`.
# With path_override the audit runs under that PATH (via an absolute bash) to
# control which hash commands are visible; otherwise it inherits the caller's
# environment.
run_audit() {
  local dir="$1" path_override="${2:-}"
  set +e
  if [[ -n "$path_override" ]]; then
    audit_output="$(cd "$dir" && PATH="$path_override" "$bash_bin" "$script" --strict 2>&1)"
  else
    audit_output="$(cd "$dir" && bash "$script" --strict 2>&1)"
  fi
  status=$?
  set -e
}

fail() {
  echo "FAIL: $1"
  echo "--- audit output ---"
  echo "$audit_output"
  exit 1
}

assert_passes() {
  local label="$1" dir="$2" path_override="${3:-}"
  echo "== $label =="
  run_audit "$dir" "$path_override"
  [[ "$status" -eq 0 ]] || fail "$label: expected exit 0, got $status"
  grep -qF "PASS: docs hygiene audit" <<<"$audit_output" || fail "$label: missing PASS marker"
  echo "ok"
}

# assert_fails <label> <dir> <expected-status> <needle> [path_override]
assert_fails() {
  local label="$1" dir="$2" expect="$3" needle="$4" path_override="${5:-}"
  echo "== $label =="
  run_audit "$dir" "$path_override"
  [[ "$status" -eq "$expect" ]] || fail "$label: expected exit $expect, got $status"
  grep -qF "$needle" <<<"$audit_output" || fail "$label: missing finding: $needle"
  echo "ok"
}

# --- distinct payloads pass --------------------------------------------------
clean_repo="$(make_repo clean)"
printf '# alpha\n\nUnique payload one.\n' >"$clean_repo/docs/alpha.md"
printf '# beta\n\nUnique payload two.\n' >"$clean_repo/docs/beta.md"
assert_passes "distinct payloads pass" "$clean_repo"

# --- identical payloads across paths are flagged -----------------------------
dup_repo="$(make_repo dup)"
mkdir -p "$dup_repo/docs/nested"
printf '# shared\n\nByte-identical payload.\n' >"$dup_repo/docs/one.md"
printf '# shared\n\nByte-identical payload.\n' >"$dup_repo/docs/nested/two.md"
assert_fails "identical payloads detected" "$dup_repo" 1 \
  "duplicate markdown payload hash detected"

# --- a failing hash command is surfaced, not swallowed -----------------------
# Prepend a shasum stub that exits non-zero ahead of the real PATH so the audit
# selects it, runs it, and must report the failure instead of passing silently.
fail_bin="$tmp_dir/fail-bin"
mkdir -p "$fail_bin"
cat >"$fail_bin/shasum" <<'STUB'
#!/usr/bin/env bash
exit 1
STUB
chmod +x "$fail_bin/shasum"
assert_fails "hash command failure surfaced" "$clean_repo" 1 \
  "failed to enumerate or hash markdown payloads" "$fail_bin:$PATH"

# --- neither shasum nor sha1sum available is a hard error --------------------
# Curate a PATH containing only the tools the audit needs, omitting both hash
# commands, so command -v finds neither. Keep this list a superset of the
# external commands docs-hygiene-audit.sh invokes on its --strict path: a tool
# the audit needs but that is absent here would fail this case for the wrong
# reason (a missing tool, not the missing hash command).
curated_bin="$tmp_dir/curated-bin"
mkdir -p "$curated_bin"
for tool in git find xargs awk sort uniq rg sed grep; do
  src="$(command -v "$tool" 2>/dev/null || true)"
  [[ -n "$src" ]] && ln -s "$src" "$curated_bin/$tool"
done
assert_fails "missing hash command is fatal" "$clean_repo" 1 \
  "missing required hash command" "$curated_bin"

# --- ripgrep is a hard prerequisite -----------------------------------------
# Keep every other command used by the clean audit available while omitting
# only rg. The audit must reject the missing prerequisite before any scan and
# must never emit its success marker.
missing_rg_bin="$tmp_dir/missing-rg-bin"
mkdir -p "$missing_rg_bin"
for tool in git find xargs awk sort uniq shasum sha1sum; do
  src="$(command -v "$tool" 2>/dev/null || true)"
  [[ -n "$src" ]] && ln -s "$src" "$missing_rg_bin/$tool"
done
assert_fails "missing ripgrep is fatal before scanning" "$clean_repo" 2 \
  "missing required tool on PATH: rg" "$missing_rg_bin"
if grep -qF "PASS: docs hygiene audit" <<<"$audit_output"; then
  fail "missing ripgrep is fatal before scanning: unexpected PASS marker"
fi

# A present rg binary can still fail to execute a scan. Exit 1 is the normal
# no-match result, but exit 2+ must propagate instead of being erased by the
# probes' historical `|| true` handling.
printf '# fixture root\n' >"$clean_repo/README.md"
failing_rg_bin="$tmp_dir/failing-rg-bin"
mkdir -p "$failing_rg_bin"
for tool in git find xargs awk sort uniq shasum sha1sum; do
  src="$(command -v "$tool" 2>/dev/null || true)"
  [[ -n "$src" ]] && ln -s "$src" "$failing_rg_bin/$tool"
done
cat >"$failing_rg_bin/rg" <<'STUB'
#!/bin/sh
exit 2
STUB
chmod +x "$failing_rg_bin/rg"
assert_fails "ripgrep runtime failure is propagated" "$clean_repo" 2 \
  "ripgrep scan failed (exit 2)" "$failing_rg_bin"
if grep -qF "PASS: docs hygiene audit" <<<"$audit_output"; then
  fail "ripgrep runtime failure is propagated: unexpected PASS marker"
fi

# The docs-only aggregate caller should fail at its own prerequisite boundary
# instead of launching an audit that is known to require rg.
verify_missing_rg_bin="$tmp_dir/verify-missing-rg-bin"
mkdir -p "$verify_missing_rg_bin"
for tool in git npx; do
  src="$(command -v "$tool" 2>/dev/null || true)"
  [[ -n "$src" ]] && ln -s "$src" "$verify_missing_rg_bin/$tool"
done

echo "== docs-only verifier preflights ripgrep =="
set +e
verify_output="$(cd "$repo_root" && PATH="$verify_missing_rg_bin" \
  "$bash_bin" "$verify_script" --docs-only 2>&1)"
verify_status=$?
set -e
if [[ "$verify_status" -ne 2 ]] \
  || ! grep -qF "missing required tool on PATH: rg" <<<"$verify_output"; then
  echo "FAIL: docs-only verifier preflights ripgrep"
  echo "--- verifier output ---"
  echo "$verify_output"
  exit 1
fi
if grep -qF "+ bash scripts/ci/" <<<"$verify_output"; then
  echo "FAIL: docs-only verifier started checks before rejecting missing rg"
  echo "$verify_output"
  exit 1
fi
echo "ok"

echo
echo "PASS: docs-hygiene-audit.test.sh"
