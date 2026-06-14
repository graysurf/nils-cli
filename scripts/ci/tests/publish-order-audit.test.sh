#!/usr/bin/env bash
set -euo pipefail

# Self-test for scripts/ci/publish-order-audit.sh.
#
# Real-workspace cases (driven by `cargo metadata`) cover the pass case and the
# completeness/duplicate/absent classes against the live order file. Synthetic
# `--metadata-file` fixtures cover the branches the real workspace cannot
# exercise today: `publish = false` entries, the dev-dependency exclusion, and
# an optional normal dependency as a real ordering constraint.

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: must run inside the nils-cli git work tree" >&2
  exit 2
fi
cd "$repo_root"

script="$repo_root/scripts/ci/publish-order-audit.sh"
real_order="$repo_root/release/crates-io-publish-order.txt"
for path in "$script" "$real_order"; do
  if [[ ! -f "$path" ]]; then
    echo "error: missing required file: $path" >&2
    exit 2
  fi
done

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/publish-order-audit-test.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

# run_audit <args...> -> sets globals `audit_output` and `status`.
run_audit() {
  set +e
  audit_output="$(bash "$script" "$@" 2>&1)"
  status=$?
  set -e
}

fail() {
  echo "FAIL: $1"
  echo "$audit_output"
  exit 1
}

assert_passes() {
  local label="$1"; shift
  echo "== $label =="
  run_audit "$@"
  [[ "$status" -eq 0 ]] || fail "$label: expected exit 0, got $status"
  grep -qF "publish-order-audit: OK" <<<"$audit_output" || fail "$label: missing OK marker"
  echo "ok"
}

# assert_fails <label> <expected-status> <needle> <args...>
assert_fails() {
  local label="$1" expect="$2" needle="$3"; shift 3
  echo "== $label =="
  run_audit "$@"
  [[ "$status" -eq "$expect" ]] || fail "$label: expected exit $expect, got $status"
  grep -qF "$needle" <<<"$audit_output" || fail "$label: missing finding: $needle"
  echo "ok"
}

# --- Real-workspace cases ----------------------------------------------------

assert_passes "real publish order passes" --order-file "$real_order"

# Missing publishable member: drop a leaf crate from the order.
grep -v '^nils-evidence$' "$real_order" >"$tmp_dir/missing.txt"
assert_fails "missing member fails" 1 \
  "publishable workspace member missing from publish order: nils-evidence" \
  --order-file "$tmp_dir/missing.txt"

# Non-member entry.
{ cat "$real_order"; echo "nils-not-a-real-crate"; } >"$tmp_dir/extra.txt"
assert_fails "non-member entry fails" 1 \
  "publish-order entry is not a workspace member: nils-not-a-real-crate" \
  --order-file "$tmp_dir/extra.txt"

# Duplicate entry.
{ cat "$real_order"; echo "nils-scrub"; } >"$tmp_dir/dup.txt"
assert_fails "duplicate entry fails" 1 \
  "duplicate publish-order entry: nils-scrub" \
  --order-file "$tmp_dir/dup.txt"

# Dependency absent from the order: drop nils-scrub, which nils-plan-archive
# (still listed) depends on. The 'absent' topological branch fires alongside
# the missing-member finding; assert the specific 'absent' wording.
grep -v '^nils-scrub$' "$real_order" >"$tmp_dir/absent.txt"
assert_fails "absent dependency fails" 1 \
  "depends on workspace crate nils-scrub, which is absent from the publish order" \
  --order-file "$tmp_dir/absent.txt"

# Missing prerequisite: a nonexistent order file is a usage error (exit 2).
assert_fails "missing order file is exit 2" 2 \
  "publish-order file not found" \
  --order-file "$tmp_dir/does-not-exist.txt"

# --- Synthetic-metadata cases (--metadata-file) ------------------------------

# Optional normal dependency is a real ordering constraint: crate-a optionally
# depends on crate-b, so listing crate-a before crate-b is an inversion.
cat >"$tmp_dir/meta-optional.json" <<'JSON'
{
  "workspace_members": ["crate-a 0.0.0 (path+file:///a)", "crate-b 0.0.0 (path+file:///b)"],
  "packages": [
    {"id": "crate-a 0.0.0 (path+file:///a)", "name": "crate-a", "publish": null,
     "dependencies": [{"name": "crate-b", "kind": null, "optional": true}]},
    {"id": "crate-b 0.0.0 (path+file:///b)", "name": "crate-b", "publish": null,
     "dependencies": []}
  ]
}
JSON
printf 'crate-a\ncrate-b\n' >"$tmp_dir/order-optional.txt"
assert_fails "optional dependency constrains order" 1 \
  "before its dependency crate-b" \
  --order-file "$tmp_dir/order-optional.txt" --metadata-file "$tmp_dir/meta-optional.json"

# Dev-dependency DOES constrain the order: cargo resolves versioned path
# dev-dependencies while packaging, so the real publisher (publish-crates.sh,
# which constrains on any workspace path dep regardless of kind) requires
# crate-b before crate-a. Listing crate-a (dev-depends crate-b) before crate-b
# is an inversion the audit must catch, or it would green-light an order the
# publisher rejects.
cat >"$tmp_dir/meta-dev.json" <<'JSON'
{
  "workspace_members": ["crate-a 0.0.0 (path+file:///a)", "crate-b 0.0.0 (path+file:///b)"],
  "packages": [
    {"id": "crate-a 0.0.0 (path+file:///a)", "name": "crate-a", "publish": null,
     "dependencies": [{"name": "crate-b", "kind": "dev"}]},
    {"id": "crate-b 0.0.0 (path+file:///b)", "name": "crate-b", "publish": null,
     "dependencies": []}
  ]
}
JSON
printf 'crate-a\ncrate-b\n' >"$tmp_dir/order-dev.txt"
assert_fails "dev dependency constrains order" 1 \
  "before its dependency crate-b" \
  --order-file "$tmp_dir/order-dev.txt" --metadata-file "$tmp_dir/meta-dev.json"

# A publish=false crate must not appear in the order.
cat >"$tmp_dir/meta-pubfalse.json" <<'JSON'
{
  "workspace_members": ["crate-a 0.0.0 (path+file:///a)", "crate-b 0.0.0 (path+file:///b)"],
  "packages": [
    {"id": "crate-a 0.0.0 (path+file:///a)", "name": "crate-a", "publish": null,
     "dependencies": []},
    {"id": "crate-b 0.0.0 (path+file:///b)", "name": "crate-b", "publish": [],
     "dependencies": []}
  ]
}
JSON
printf 'crate-a\ncrate-b\n' >"$tmp_dir/order-pubfalse.txt"
assert_fails "publish=false crate listed fails" 1 \
  "publish-order lists a publish=false crate: crate-b" \
  --order-file "$tmp_dir/order-pubfalse.txt" --metadata-file "$tmp_dir/meta-pubfalse.json"

echo
echo "PASS: publish-order-audit.test.sh"
