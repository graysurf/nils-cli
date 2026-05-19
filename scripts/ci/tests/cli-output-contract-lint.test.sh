#!/usr/bin/env bash
set -euo pipefail

# Self-test for scripts/ci/cli-output-contract-lint.sh.
#
# Creates synthetic regression fixtures in a throwaway git worktree and
# asserts the lint script reports each violation class:
#   (a) new `--json` boolean flag without `hide = true`
#   (b) inline `process::exit(1|2)` in a main.rs entrypoint
#   (c) camelCase serde rename outside the documented allowlist
#
# Also runs the lint against the real repository root to confirm the
# baseline still passes.

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: must run inside the nils-cli git work tree" >&2
  exit 2
fi

lint_script="$repo_root/scripts/ci/cli-output-contract-lint.sh"
if [[ ! -x "$lint_script" ]]; then
  echo "error: missing lint script: $lint_script" >&2
  exit 2
fi

assert_lint_clean_in_repo() {
  echo "== baseline: real repo should pass =="
  if ! bash "$lint_script" >/dev/null 2>&1; then
    echo "FAIL: baseline lint failed on real repo"
    bash "$lint_script" || true
    exit 1
  fi
  echo "ok"
}

make_fixture_repo() {
  local fixture_dir="$1"
  (
    cd "$fixture_dir"
    git init --quiet
    git config user.email lint-test@example.com
    git config user.name "Lint Self-Test"
  )
}

write_baseline_fixture() {
  local fixture_dir="$1"
  mkdir -p "$fixture_dir/crates/fake-bin/src"
  mkdir -p "$fixture_dir/scripts/ci"
  cp "$lint_script" "$fixture_dir/scripts/ci/cli-output-contract-lint.sh"
  chmod +x "$fixture_dir/scripts/ci/cli-output-contract-lint.sh"
  cat >"$fixture_dir/crates/fake-bin/src/main.rs" <<'EOF'
use std::process;

fn main() {
    process::exit(0);
}
EOF
  cat >"$fixture_dir/crates/fake-bin/src/cli.rs" <<'EOF'
#[derive(Default)]
pub struct Cli {
    /// Hidden alias for --format json
    #[arg(long, global = true, hide = true, conflicts_with = "format")]
    pub json: bool,
}
EOF
}

assert_fixture_clean() {
  local fixture_dir="$1"
  echo "== fixture: clean baseline should pass =="
  if ! (cd "$fixture_dir" && bash scripts/ci/cli-output-contract-lint.sh >/dev/null 2>&1); then
    echo "FAIL: clean fixture failed unexpectedly"
    (cd "$fixture_dir" && bash scripts/ci/cli-output-contract-lint.sh) || true
    exit 1
  fi
  echo "ok"
}

assert_violation() {
  local label="$1"
  local fixture_dir="$2"
  local expected_substring="$3"
  echo "== regression: $label =="
  set +e
  output="$(cd "$fixture_dir" && bash scripts/ci/cli-output-contract-lint.sh 2>&1)"
  status=$?
  set -e
  if [[ $status -eq 0 ]]; then
    echo "FAIL: expected lint to fail for '$label' but it passed"
    echo "----- output -----"
    echo "$output"
    exit 1
  fi
  if ! grep -qF "$expected_substring" <<<"$output"; then
    echo "FAIL: '$label' did not surface expected substring: $expected_substring"
    echo "----- output -----"
    echo "$output"
    exit 1
  fi
  echo "ok ($status, matched: $expected_substring)"
}

main() {
  assert_lint_clean_in_repo

  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp:-}"' EXIT

  make_fixture_repo "$tmp"
  write_baseline_fixture "$tmp"
  assert_fixture_clean "$tmp"

  # (a) new --json bool without hide = true
  local tmp_a
  tmp_a="$(mktemp -d)"
  make_fixture_repo "$tmp_a"
  write_baseline_fixture "$tmp_a"
  cat >"$tmp_a/crates/fake-bin/src/cli.rs" <<'EOF'
#[derive(Default)]
pub struct Cli {
    /// Plain --json alias (no hide flag)
    #[arg(long, global = true, conflicts_with = "format")]
    pub json: bool,
}
EOF
  assert_violation "check (a): --json without hide = true" "$tmp_a" "(a) --json bool flag without 'hide = true'"
  rm -rf "$tmp_a"

  # (b) process::exit(1) in main.rs
  local tmp_b
  tmp_b="$(mktemp -d)"
  make_fixture_repo "$tmp_b"
  write_baseline_fixture "$tmp_b"
  cat >"$tmp_b/crates/fake-bin/src/main.rs" <<'EOF'
use std::process;

fn main() {
    process::exit(1);
}
EOF
  assert_violation "check (b): process::exit(1) literal" "$tmp_b" "(b) inline process::exit(1|2) literal"
  rm -rf "$tmp_b"

  # (c) camelCase serde rename outside allowlist
  local tmp_c
  tmp_c="$(mktemp -d)"
  make_fixture_repo "$tmp_c"
  write_baseline_fixture "$tmp_c"
  cat >"$tmp_c/crates/fake-bin/src/output.rs" <<'EOF'
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Payload {
    pub field_name: String,
}
EOF
  assert_violation "check (c): camelCase outside allowlist" "$tmp_c" "(c) camelCase serde rename"
  rm -rf "$tmp_c"

  echo
  echo "PASS: cli-output-contract-lint.test.sh (all regression fixtures fired as expected)"
}

main "$@"
