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
| `dynamic-cli` | `required` | `present` (`_dynamic-cli`) | `present` (`dynamic-cli`) | not required | `completion_mode=clap-first; completion_mode_toggles=forbidden; alternate_completion_dispatch=forbidden; generated_load_failure=fail-closed; completion_engine=dynamic` | synthetic dynamic-engine test binary |
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

# A dynamic-engine CLI emits a `CompleteEnv` registration whose stub embeds the
# resolved binary path (`current_exe()`), so the runtime output is intentionally
# NOT byte-identical to the committed registration asset. The freshness audit
# must classify it as a dynamic engine and skip the diff instead of flagging it
# stale.
cat >"$tmp/target/debug/dynamic-cli" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "completion" && "${2:-}" == "bash" ]]; then
  printf '%s\n' "complete -F _clap_dynamic_completer_dynamic-cli -o nosort -o bashdefault -o default dynamic-cli"
  printf '%s\n' "# COMPLETE registration path: /runtime/only/target/debug/dynamic-cli"
  exit 0
fi

if [[ "${1:-}" == "completion" && "${2:-}" == "zsh" ]]; then
  printf '%s\n' "#compdef dynamic-cli"
  printf '%s\n' "_clap_dynamic_completer_dynamic-cli \"\$@\""
  printf '%s\n' "# COMPLETE registration path: /runtime/only/target/debug/dynamic-cli"
  exit 0
fi

echo "unexpected dynamic-cli invocation: $*" >&2
exit 64
EOF
chmod +x "$tmp/target/debug/dynamic-cli"

printf '%s\n' "bash completion v1" >"$tmp/completions/bash/fake-cli"
printf '%s\n' "zsh completion v1" >"$tmp/completions/zsh/_fake-cli"
printf '%s\n' "_nils_cli_completion_common_load_generated_bash" >"$tmp/completions/bash/adapter-cli"
printf '%s\n' "_nils_cli_completion_common_load_generated_zsh" >"$tmp/completions/zsh/_adapter-cli"
# Committed dynamic registration asset, deliberately embedding a DIFFERENT
# (install-time) path than the runtime binary emits above. A static-mode diff
# would flag this stale; a dynamic-mode audit must skip it.
printf '%s\n' "complete -F _clap_dynamic_completer_dynamic-cli -o nosort -o bashdefault -o default dynamic-cli" >"$tmp/completions/bash/dynamic-cli"
printf '%s\n' "# COMPLETE registration path: /opt/homebrew/bin/dynamic-cli" >>"$tmp/completions/bash/dynamic-cli"
printf '%s\n' "#compdef dynamic-cli" >"$tmp/completions/zsh/_dynamic-cli"
printf '%s\n' "_clap_dynamic_completer_dynamic-cli \"\$@\"" >>"$tmp/completions/zsh/_dynamic-cli"
printf '%s\n' "# COMPLETE registration path: /opt/homebrew/bin/dynamic-cli" >>"$tmp/completions/zsh/_dynamic-cli"

assert_fresh_assets_pass() {
  echo "== fresh committed assets pass =="
  local output
  output="$(bash "$script" --root "$tmp" --skip-build)"
  if ! grep -qF "PASS: completion freshness audit (required=3, snapshots_checked=2, runtime_adapters_skipped=2, dynamic_engine_skipped=2, failures=0)" <<<"$output"; then
    echo "FAIL: expected fresh assets to pass"
    echo "$output"
    exit 1
  fi
  echo "ok"
}

assert_dynamic_cli_skipped() {
  echo "== dynamic-engine CLI is skipped, and static WOULD flag the same asset stale =="
  local output
  output="$(bash "$script" --root "$tmp" --skip-build 2>&1)"
  # The committed dynamic asset differs from the runtime output; a static diff
  # would report it stale. Dynamic mode must not.
  if grep -qF "stale bash completion asset: completions/bash/dynamic-cli" <<<"$output" \
    || grep -qF "stale zsh completion asset: completions/zsh/_dynamic-cli" <<<"$output"; then
    echo "FAIL: dynamic-cli asset was flagged stale (should be skipped)"
    echo "$output"
    exit 1
  fi

  # Negative control: strip the `completion_engine=dynamic` key so the SAME
  # differing asset is treated as static. It must now be flagged stale. This
  # proves the skip is caused by the engine key, not by something incidental,
  # so this assertion cannot pass vacuously alongside assert_fresh_assets_pass.
  local matrix="$tmp/docs/specs/completion-coverage-matrix-v1.md"
  cp "$matrix" "$matrix.orig"
  sed -i.bak 's/; completion_engine=dynamic`/`/' "$matrix"
  rm -f "$matrix.bak"

  local ctrl status
  set +e
  ctrl="$(bash "$script" --root "$tmp" --skip-build 2>&1)"
  status=$?
  set -e

  cp "$matrix.orig" "$matrix"
  rm -f "$matrix.orig"

  if [[ "$status" -eq 0 ]]; then
    echo "FAIL: with the engine key removed, the differing dynamic-cli asset should be flagged stale"
    echo "$ctrl"
    exit 1
  fi
  if ! { grep -qF "stale bash completion asset: completions/bash/dynamic-cli" <<<"$ctrl" \
      || grep -qF "stale zsh completion asset: completions/zsh/_dynamic-cli" <<<"$ctrl"; }; then
    echo "FAIL: negative control did not flag the now-static dynamic-cli asset stale"
    echo "$ctrl"
    exit 1
  fi
  echo "ok"
}

assert_dynamic_cli_missing_asset_fails() {
  echo "== dynamic-engine CLI still requires a committed asset =="
  local saved output status
  saved="$(cat "$tmp/completions/zsh/_dynamic-cli")"
  rm -f "$tmp/completions/zsh/_dynamic-cli"

  set +e
  output="$(bash "$script" --root "$tmp" --skip-build 2>&1)"
  status=$?
  set -e

  # restore for later assertions
  printf '%s\n' "$saved" >"$tmp/completions/zsh/_dynamic-cli"

  if [[ "$status" -eq 0 ]]; then
    echo "FAIL: expected missing dynamic asset to fail"
    echo "$output"
    exit 1
  fi
  if ! grep -qF "missing committed zsh completion asset: completions/zsh/_dynamic-cli" <<<"$output"; then
    echo "FAIL: missing dynamic asset did not surface the expected failure"
    echo "$output"
    exit 1
  fi
  echo "ok ($status)"
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
assert_dynamic_cli_skipped
assert_dynamic_cli_missing_asset_fails
assert_stale_asset_fails

echo
echo "PASS: completion-freshness-audit.test.sh"
