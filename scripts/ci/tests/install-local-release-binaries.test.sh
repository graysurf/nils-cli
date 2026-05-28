#!/usr/bin/env bash
set -euo pipefail

# Self-test for scripts/install-local-release-binaries.sh.
#
# The installer must build the exact binaries it will install. A broad
# workspace build can leave target/release/<bin> stale for bins that Cargo did
# not rebuild, which then copies old local binaries back into ~/.local.

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: must run inside the nils-cli git work tree" >&2
  exit 2
fi

script="$repo_root/scripts/install-local-release-binaries.sh"
if [[ ! -f "$script" ]]; then
  echo "error: missing installer script: $script" >&2
  exit 2
fi

assert_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if ! grep -qF -- "$needle" <<<"$haystack"; then
    echo "FAIL: $label"
    echo "missing: $needle"
    echo "$haystack"
    exit 1
  fi
}

make_fake_tools() {
  local tmp="$1"
  local log="$2"
  local fake_repo="$3"
  local bin_dir="$tmp/bin"
  mkdir -p "$bin_dir"

  cat >"$bin_dir/git" <<'FAKE_GIT'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "rev-parse" && "${2:-}" == "--show-toplevel" ]]; then
  printf '%s\n' "${INSTALLER_TEST_REPO_ROOT:?}"
  exit 0
fi
exec "${INSTALLER_TEST_REAL_GIT:?}" "$@"
FAKE_GIT

  cat >"$bin_dir/cargo" <<'FAKE_CARGO'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "metadata" ]]; then
  cat <<'JSON'
{"packages":[{"targets":[{"kind":["bin"],"crate_types":["bin"],"name":"alpha"},{"kind":["bin"],"crate_types":["bin"],"name":"beta","required-features":["beta-cli"]}]}]}
JSON
  exit 0
fi

if [[ "${1:-}" != "build" ]]; then
  echo "unexpected cargo command: $*" >&2
  exit 1
fi

printf '%s\n' "$*" >>"${INSTALLER_TEST_LOG:?}"
mkdir -p target/release
while [[ $# -gt 0 ]]; do
  case "${1:-}" in
    --bin)
      bin="${2:-}"
      if [[ -z "$bin" ]]; then
        echo "missing --bin value" >&2
        exit 1
      fi
      printf '#!/usr/bin/env bash\n' >"target/release/$bin"
      printf 'echo %s\n' "$bin" >>"target/release/$bin"
      chmod 0755 "target/release/$bin"
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done
FAKE_CARGO

  cat >"$bin_dir/install" <<'FAKE_INSTALL'
#!/usr/bin/env bash
set -euo pipefail
printf 'install %s\n' "$*" >>"${INSTALLER_TEST_LOG:?}"
if [[ "${1:-}" == "-m" ]]; then
  shift 2
fi
src="${1:-}"
dest="${2:-}"
if [[ -z "$src" || -z "$dest" ]]; then
  echo "missing install src/dest" >&2
  exit 1
fi
if [[ "$dest" == */ ]]; then
  mkdir -p "$dest"
  cp "$src" "$dest/$(basename "$src")"
else
  mkdir -p "$(dirname "$dest")"
  cp "$src" "$dest"
fi
FAKE_INSTALL

  chmod 0755 "$bin_dir/git" "$bin_dir/cargo" "$bin_dir/install"

  mkdir -p "$fake_repo/scripts"
  printf '[workspace]\nmembers = []\n' >"$fake_repo/Cargo.toml"
  cp "$repo_root/scripts/workspace-bins.sh" "$fake_repo/scripts/workspace-bins.sh"
}

run_with_fake_tools() {
  local tmp="$1"
  shift
  local log="$tmp/commands.log"
  local fake_repo="$tmp/repo"
  make_fake_tools "$tmp" "$log" "$fake_repo"
  INSTALLER_TEST_LOG="$log" \
    INSTALLER_TEST_REAL_GIT="$(command -v git)" \
    INSTALLER_TEST_REPO_ROOT="$fake_repo" \
    PATH="$tmp/bin:$PATH" \
    bash "$script" "$@"
  cat "$log"
}

assert_explicit_bins_build_exact_selection() {
  echo "== explicit --bin values drive cargo build =="
  local tmp
  tmp="$(mktemp -d)"
  local output
  output="$(run_with_fake_tools "$tmp" --prefix "$tmp/install" --bin git-cli --bin plan-tooling)"

  assert_contains "$FUNCNAME" "$output" "build --release --bin git-cli --bin plan-tooling"
  assert_contains "$FUNCNAME" "$output" "install -m 0755 $tmp/repo/target/release/git-cli $tmp/install/"
  assert_contains "$FUNCNAME" "$output" "install -m 0755 $tmp/repo/target/release/plan-tooling $tmp/install/"
  [[ -x "$tmp/install/git-cli" ]]
  [[ -x "$tmp/install/plan-tooling" ]]
  rm -rf "$tmp"
  echo "ok"
}

assert_default_inventory_builds_release_default_bins() {
  echo "== default inventory drives cargo build =="
  local tmp
  tmp="$(mktemp -d)"
  local output
  output="$(run_with_fake_tools "$tmp" --prefix "$tmp/install")"

  assert_contains "$FUNCNAME" "$output" "build --release --bin alpha"
  if grep -qF -- "--bin beta" <<<"$output"; then
    echo "FAIL: $FUNCNAME"
    echo "required-features bin should be excluded from release-default inventory"
    echo "$output"
    exit 1
  fi
  [[ -x "$tmp/install/alpha" ]]
  rm -rf "$tmp"
  echo "ok"
}

assert_explicit_bins_build_exact_selection
assert_default_inventory_builds_release_default_bins

echo
echo "PASS: install-local-release-binaries.test.sh"
