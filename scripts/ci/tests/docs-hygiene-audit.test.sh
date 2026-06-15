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

echo
echo "PASS: docs-hygiene-audit.test.sh"
