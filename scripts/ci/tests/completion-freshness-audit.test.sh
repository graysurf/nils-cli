#!/usr/bin/env bash
set -euo pipefail

# Self-test for scripts/ci/completion-freshness-audit.sh.
#
# Uses a synthetic mini workspace so stale committed completion content is
# exercised without mutating the real repository's generated assets.

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: must run inside the nils-cli git work tree" >&2
  exit 2
fi

script="$repo_root/scripts/ci/completion-freshness-audit.sh"
if [[ ! -f "$script" ]]; then
  echo "error: missing freshness audit script: $script" >&2
  exit 2
fi

tmp="$(mktemp -d "${TMPDIR:-/tmp}/completion-freshness-test.XXXXXX")"
cleanup() {
  rm -rf "$tmp"
}
trap cleanup EXIT

mkdir -p "$tmp/docs/specs" "$tmp/completions/bash" "$tmp/completions/zsh" "$tmp/target/debug"

cat >"$tmp/docs/specs/completion-coverage-matrix-v1.md" <<'EOF'
| Binary | Obligation | Zsh completion (`completions/zsh`) | Bash completion (`completions/bash`) | Alias requirement | Completion enforcement metadata | Rationale |
| --- | --- | --- | --- | --- | --- | --- |
| `fake-cli` | `required` | `present` (`_fake-cli`) | `present` (`fake-cli`) | not required | `completion_mode=clap-first; completion_mode_toggles=forbidden; alternate_completion_dispatch=forbidden; generated_load_failure=fail-closed` | synthetic test binary |
| `adapter-cli` | `required` | `present` (`_adapter-cli`) | `present` (`adapter-cli`) | not required | `completion_mode=clap-first; completion_mode_toggles=forbidden; alternate_completion_dispatch=forbidden; generated_load_failure=fail-closed` | synthetic adapter test binary |
EOF

cat >"$tmp/target/debug/fake-cli" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "completion" && "${2:-}" == "bash" ]]; then
  printf '%s\n' "bash completion v1"
  exit 0
fi

if [[ "${1:-}" == "completion" && "${2:-}" == "zsh" ]]; then
  printf '%s\n' "zsh completion v1"
  exit 0
fi

echo "unexpected fake-cli invocation: $*" >&2
exit 64
EOF
chmod +x "$tmp/target/debug/fake-cli"

cat >"$tmp/target/debug/adapter-cli" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "completion" && "${2:-}" == "bash" ]]; then
  printf '%s\n' "generated bash payload"
  exit 0
fi

if [[ "${1:-}" == "completion" && "${2:-}" == "zsh" ]]; then
  printf '%s\n' "generated zsh payload"
  exit 0
fi

echo "unexpected adapter-cli invocation: $*" >&2
exit 64
EOF
chmod +x "$tmp/target/debug/adapter-cli"

printf '%s\n' "bash completion v1" >"$tmp/completions/bash/fake-cli"
printf '%s\n' "zsh completion v1" >"$tmp/completions/zsh/_fake-cli"
printf '%s\n' "_nils_cli_completion_common_load_generated_bash" >"$tmp/completions/bash/adapter-cli"
printf '%s\n' "_nils_cli_completion_common_load_generated_zsh" >"$tmp/completions/zsh/_adapter-cli"

assert_fresh_assets_pass() {
  echo "== fresh committed assets pass =="
  local output
  output="$(bash "$script" --root "$tmp" --skip-build)"
  if ! grep -qF "PASS: completion freshness audit (required=2, snapshots_checked=2, runtime_adapters_skipped=2, failures=0)" <<<"$output"; then
    echo "FAIL: expected fresh assets to pass"
    echo "$output"
    exit 1
  fi
  echo "ok"
}

assert_stale_asset_fails() {
  echo "== stale committed asset fails =="
  printf '%s\n' "stale zsh completion" >"$tmp/completions/zsh/_fake-cli"

  local output status
  set +e
  output="$(bash "$script" --root "$tmp" --skip-build 2>&1)"
  status=$?
  set -e

  if [[ "$status" -eq 0 ]]; then
    echo "FAIL: expected stale asset to fail"
    echo "$output"
    exit 1
  fi
  if ! grep -qF "FAIL: fake-cli: stale zsh completion asset: completions/zsh/_fake-cli" <<<"$output"; then
    echo "FAIL: stale-asset failure did not identify the zsh asset"
    echo "$output"
    exit 1
  fi
  if ! grep -qF "INFO: diff preview for fake-cli zsh completion drift" <<<"$output"; then
    echo "FAIL: stale-asset failure did not include a diff preview"
    echo "$output"
    exit 1
  fi
  echo "ok ($status)"
}

assert_fresh_assets_pass
assert_stale_asset_fails

echo
echo "PASS: completion-freshness-audit.test.sh"
