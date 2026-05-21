#!/usr/bin/env bash
set -euo pipefail

# Self-test for scripts/dev/shared-helper-adoption-audit.sh.
#
# Guards the seed manifest against path drift:
#   (a) the real repo must not contain missing seeded paths
#   (b) --check-seeds must fail when run against a synthetic workspace whose
#       seeded files are absent

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: must run inside the nils-cli git work tree" >&2
  exit 2
fi

audit_script="$repo_root/scripts/dev/shared-helper-adoption-audit.sh"
if [[ ! -x "$audit_script" ]]; then
  echo "error: missing audit script: $audit_script" >&2
  exit 2
fi

assert_real_repo_seed_paths_exist() {
  echo "== baseline: real repo seed paths should exist =="
  if ! bash "$audit_script" --check-seeds --format tsv >/dev/null; then
    echo "FAIL: real repo contains missing shared-helper audit seed paths"
    bash "$audit_script" --check-seeds --format tsv || true
    exit 1
  fi
  echo "ok"
}

assert_check_seeds_keeps_tsv_stdout() {
  echo "== contract: --check-seeds keeps TSV stdout clean =="
  local output
  output="$(bash "$audit_script" --check-seeds --format tsv 2>/dev/null)"
  local first_line
  first_line="$(printf '%s\n' "$output" | sed -n '1p')"
  if [[ "$first_line" != $'path\tcategory\thelper_target\tstatus\ttask_id\trisk\tdetection_regex\tmatch_count\tmatch_preview\tnote' ]]; then
    echo "FAIL: --check-seeds polluted TSV stdout"
    printf '%s\n' "$output" | sed -n '1,5p'
    exit 1
  fi
  echo "ok"
}

assert_missing_seed_paths_fail() {
  echo "== regression: missing seeded paths should fail =="

  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp:-}"' EXIT

  mkdir -p "$tmp/crates"
  printf '[workspace]\nmembers = []\n' >"$tmp/Cargo.toml"

  set +e
  output="$(bash "$audit_script" --root "$tmp" --check-seeds --format tsv 2>&1)"
  status=$?
  set -e

  if [[ "$status" -eq 0 ]]; then
    echo "FAIL: expected --check-seeds to fail for missing seeded paths"
    echo "$output"
    exit 1
  fi
  if ! grep -qF "missing seeded paths" <<<"$output"; then
    echo "FAIL: missing-path failure did not name the seed-path problem"
    echo "$output"
    exit 1
  fi
  echo "ok ($status)"
}

assert_real_repo_seed_paths_exist
assert_check_seeds_keeps_tsv_stdout
assert_missing_seed_paths_fail

echo
echo "PASS: shared-helper-adoption-audit.test.sh"
