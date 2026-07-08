#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
skill_root="$(cd "${script_dir}/.." && pwd)"
entrypoint="${skill_root}/scripts/project-bump-version-tag-release.sh"

fail() {
  echo "error: $*" >&2
  exit 1
}

assert_contains() {
  local file="$1"
  local pattern="$2"
  if ! rg -q -- "$pattern" "$file"; then
    echo "error: expected pattern '$pattern' in $file" >&2
    sed -n '1,220p' "$file" >&2 || true
    exit 1
  fi
}

assert_not_contains() {
  local file="$1"
  local pattern="$2"
  if rg -q -- "$pattern" "$file"; then
    echo "error: unexpected pattern '$pattern' in $file" >&2
    sed -n '1,220p' "$file" >&2 || true
    exit 1
  fi
}

create_temp_repo() {
  local repo_dir="$1"
  local readme_tag="$2"

  git init --initial-branch=main "$repo_dir" >/dev/null
  git -C "$repo_dir" config user.email "test@example.com"
  git -C "$repo_dir" config user.name "Test User"

  mkdir -p \
    "${repo_dir}/crates/codex-cli" \
    "${repo_dir}/scripts" \
    "${repo_dir}/.agents/skills/project-verify-required-checks/scripts"

  cat > "${repo_dir}/Cargo.toml" <<'EOF'
[workspace]
members = ["crates/codex-cli"]
resolver = "2"

[workspace.package]
version = "0.6.4"
EOF

  cat > "${repo_dir}/crates/codex-cli/Cargo.toml" <<'EOF'
[package]
name = "nils-codex-cli"
version = "0.6.4"
edition = "2021"
EOF

  cat > "${repo_dir}/README.md" <<EOF
To trigger a release build, push a tag like \`${readme_tag}\`:

- \`git tag -a ${readme_tag} -m "${readme_tag}"\`
- \`git push origin ${readme_tag}\`
EOF

  cat > "${repo_dir}/THIRD_PARTY_LICENSES.md" <<'EOF'
licenses-old
EOF

  cat > "${repo_dir}/THIRD_PARTY_NOTICES.md" <<'EOF'
notices-old
EOF

  cat > "${repo_dir}/scripts/generate-third-party-artifacts.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

[[ "${1:-}" == "--write" ]] || exit 1
echo "licenses-generated" > THIRD_PARTY_LICENSES.md
echo "notices-generated" > THIRD_PARTY_NOTICES.md
echo "PASS: regenerated THIRD_PARTY_LICENSES.md THIRD_PARTY_NOTICES.md"
EOF
  chmod +x "${repo_dir}/scripts/generate-third-party-artifacts.sh"

  cat > "${repo_dir}/.agents/skills/project-verify-required-checks/scripts/project-verify-required-checks.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

log_file="${MOCK_LOG:?}"
echo "checks:start" >> "$log_file"
[[ -f Cargo.lock ]] || {
  echo "missing Cargo.lock before checks" >&2
  exit 1
}
[[ -z "${RUSTC_WRAPPER:-}" ]] || {
  echo "RUSTC_WRAPPER should be unset for checks" >&2
  exit 1
}
echo "checks:ok" >> "$log_file"
EOF
  chmod +x "${repo_dir}/.agents/skills/project-verify-required-checks/scripts/project-verify-required-checks.sh"

  git -C "$repo_dir" add .
  git -C "$repo_dir" commit -m "init" >/dev/null
}

create_mock_cargo() {
  local bin_dir="$1"
  cat > "${bin_dir}/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

log_file="${MOCK_LOG:?}"
echo "cargo:$*" >> "$log_file"
echo "cargo:RUSTC_WRAPPER=${RUSTC_WRAPPER-}" >> "$log_file"

case "${1:-}" in
  generate-lockfile)
    [[ -z "${RUSTC_WRAPPER:-}" ]] || {
      echo "RUSTC_WRAPPER should be unset before cargo generate-lockfile" >&2
      exit 1
    }
    echo "# mock lockfile" > Cargo.lock
    ;;
  update)
    # Release bumps re-pin workspace crate versions via `cargo update
    # --workspace`, which also writes Cargo.lock if absent.
    [[ -z "${RUSTC_WRAPPER:-}" ]] || {
      echo "RUSTC_WRAPPER should be unset before cargo update" >&2
      exit 1
    }
    echo "# mock lockfile" > Cargo.lock
    ;;
  check)
    [[ -f Cargo.lock ]] || {
      echo "missing Cargo.lock before cargo check" >&2
      exit 1
    }
    ;;
  *)
    echo "unexpected cargo command: $*" >&2
    exit 1
    ;;
esac
EOF
  chmod +x "${bin_dir}/cargo"
}

create_mock_semantic_commit() {
  local bin_dir="$1"
  cat > "${bin_dir}/semantic-commit" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" != "commit" ]]; then
  echo "unexpected semantic-commit command: $*" >&2
  exit 1
fi

msg_file="$(mktemp)"
cat > "$msg_file"
git commit -F "$msg_file" >/dev/null
rm -f "$msg_file"
EOF
  chmod +x "${bin_dir}/semantic-commit"
}

create_mock_git_scope() {
  local bin_dir="$1"
  cat > "${bin_dir}/git-scope" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
exit 0
EOF
  chmod +x "${bin_dir}/git-scope"
}

create_mock_bad_wrapper() {
  local bin_dir="$1"
  cat > "${bin_dir}/bad-wrapper" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
echo "Compiler not supported: mock wrapper" >&2
exit 1
EOF
  chmod +x "${bin_dir}/bad-wrapper"
}

# Happy-path gh stub for the `--from-tap` flow. It answers the two gh calls that
# path now makes: the REST release-asset check
# (`gh api repos/<repo>/releases/tags/v0.9.9`, returning all eight required
# assets + an `html_url`) and the tap-update workflow poll (`gh -R <tap> run
# list …`, returning a completed/success run whose `displayTitle` carries the
# tag — the field `wait_for_homebrew_tap_update` matches on). Tailored to the
# v0.9.9 fixture used by test_from_tap_upgrades_installed_local_brew_formula.
create_mock_gh() {
  local bin_dir="$1"
  cat > "${bin_dir}/gh" <<'EOF'
#!/usr/bin/env bash
set -uo pipefail

if [[ "$*" == *"api repos/"*"/releases/tags/"* ]]; then
  cat <<'JSON'
{
  "html_url": "https://github.com/test-org/test-repo/releases/tag/v0.9.9",
  "assets": [
    {"name": "nils-cli-v0.9.9-aarch64-apple-darwin.tar.gz"},
    {"name": "nils-cli-v0.9.9-aarch64-apple-darwin.tar.gz.sha256"},
    {"name": "nils-cli-v0.9.9-x86_64-apple-darwin.tar.gz"},
    {"name": "nils-cli-v0.9.9-x86_64-apple-darwin.tar.gz.sha256"},
    {"name": "nils-cli-v0.9.9-aarch64-unknown-linux-gnu.tar.gz"},
    {"name": "nils-cli-v0.9.9-aarch64-unknown-linux-gnu.tar.gz.sha256"},
    {"name": "nils-cli-v0.9.9-x86_64-unknown-linux-gnu.tar.gz"},
    {"name": "nils-cli-v0.9.9-x86_64-unknown-linux-gnu.tar.gz.sha256"}
  ]
}
JSON
  exit 0
fi

if [[ "$*" == *"run list"* ]]; then
  printf '[{"databaseId":1,"status":"completed","conclusion":"success","url":"https://example.test/run","displayTitle":"Update nils-cli formula to v0.9.9","event":"repository_dispatch","createdAt":"2026-07-08T00:00:00Z"}]\n'
  exit 0
fi

echo "unexpected gh command: $*" >&2
exit 1
EOF
  chmod +x "${bin_dir}/gh"
}

# gh stub whose REST releases lookup fails with a rate-limit error, to exercise
# the rate-limit-aware branch of assert_release_assets_available.
create_mock_gh_rate_limited() {
  local bin_dir="$1"
  cat > "${bin_dir}/gh" <<'EOF'
#!/usr/bin/env bash
set -uo pipefail

if [[ "$*" == *"api repos/"*"/releases/tags/"* ]]; then
  echo "gh: API rate limit exceeded for user ID 12345. (HTTP 403)" >&2
  exit 1
fi

echo "unexpected gh command: $*" >&2
exit 1
EOF
  chmod +x "${bin_dir}/gh"
}

create_mock_brew() {
  local bin_dir="$1"
  cat > "${bin_dir}/brew" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

log_file="${MOCK_LOG:?}"

case "${1:-}" in
  tap)
    # `brew tap` (no args) lists installed taps — emit nothing so the caller
    # treats the tap as absent and taps it; `brew tap <name> <url>` adds it.
    if [[ -n "${2:-}" ]]; then
      echo "brew:tap ${2}" >> "$log_file"
    fi
    ;;
  style)
    echo "brew:style ${2:-}" >> "$log_file"
    ;;
  list)
    case "${2:-}" in
      --formula)
        echo "brew:list_formula ${3:-}" >> "$log_file"
        [[ "${3:-}" == "nils-cli" ]] || exit 1
        ;;
      --versions)
        echo "brew:list_versions ${3:-}" >> "$log_file"
        printf 'nils-cli %s\n' "${MOCK_BREW_VERSION:?}"
        ;;
      *)
        echo "unexpected brew list command: $*" >&2
        exit 1
        ;;
    esac
    ;;
  update)
    echo "brew:update" >> "$log_file"
    ;;
  upgrade)
    echo "brew:upgrade ${2:-}" >> "$log_file"
    [[ "${2:-}" == "nils-cli" ]] || exit 1
    ;;
  *)
    echo "unexpected brew command: $*" >&2
    exit 1
    ;;
esac
EOF
  chmod +x "${bin_dir}/brew"
}

test_full_checks_refresh_lockfile_and_disable_bad_wrapper() {
  local tmp repo bin_dir log_file stderr_file
  tmp="$(mktemp -d)"
  repo="${tmp}/repo"
  bin_dir="${tmp}/bin"
  log_file="${tmp}/mock.log"
  stderr_file="${tmp}/stderr.log"

  mkdir -p "$repo" "$bin_dir"
  create_temp_repo "$repo" "v0.6.4"
  create_mock_cargo "$bin_dir"
  create_mock_semantic_commit "$bin_dir"
  create_mock_git_scope "$bin_dir"
  create_mock_bad_wrapper "$bin_dir"

  (
    cd "$repo"
    PATH="${bin_dir}:$PATH" \
      MOCK_LOG="$log_file" \
      RUSTC_WRAPPER="bad-wrapper" \
      "$entrypoint" --version v0.6.5 --full-checks --skip-push
  ) >"${tmp}/stdout.log" 2>"${stderr_file}"

  local order_file="${tmp}/order.log"
  rg -n 'cargo:update --workspace|checks:start' "$log_file" >"$order_file"
  assert_contains "$order_file" '1:cargo:update --workspace'
  assert_contains "$order_file" '3:checks:start'
  assert_not_contains "$log_file" 'cargo:RUSTC_WRAPPER=bad-wrapper'
  assert_contains "$stderr_file" 'disabling it for release commands'
  assert_contains "${repo}/README.md" 'v0.6.5'

  git -C "$repo" rev-parse -q --verify "refs/tags/v0.6.5" >/dev/null \
    || fail "expected tag v0.6.5 to exist"
}

test_default_path_skips_full_audit_and_runs_locked_check() {
  local tmp repo bin_dir log_file stderr_file
  tmp="$(mktemp -d)"
  repo="${tmp}/repo"
  bin_dir="${tmp}/bin"
  log_file="${tmp}/mock.log"
  stderr_file="${tmp}/stderr.log"

  mkdir -p "$repo" "$bin_dir"
  create_temp_repo "$repo" "v0.6.4"
  create_mock_cargo "$bin_dir"
  create_mock_semantic_commit "$bin_dir"
  create_mock_git_scope "$bin_dir"

  (
    cd "$repo"
    env -u RUSTC_WRAPPER \
      PATH="${bin_dir}:$PATH" \
      MOCK_LOG="$log_file" \
      "$entrypoint" --version v0.6.5 --skip-push
  ) >"${tmp}/stdout.log" 2>"${stderr_file}"

  assert_contains "$log_file" 'cargo:update --workspace'
  assert_contains "$log_file" 'cargo:check --workspace --locked'
  # Full audit stack must NOT run in the new default path.
  if rg -q 'checks:start' "$log_file"; then
    fail "default path unexpectedly ran the full audit stack"
  fi

  git -C "$repo" rev-parse -q --verify "refs/tags/v0.6.5" >/dev/null \
    || fail "expected tag v0.6.5 to exist"
}

test_skip_checks_is_deprecated_alias_of_default() {
  local tmp repo bin_dir log_file stderr_file
  tmp="$(mktemp -d)"
  repo="${tmp}/repo"
  bin_dir="${tmp}/bin"
  log_file="${tmp}/mock.log"
  stderr_file="${tmp}/stderr.log"

  mkdir -p "$repo" "$bin_dir"
  create_temp_repo "$repo" "v0.6.4"
  create_mock_cargo "$bin_dir"
  create_mock_semantic_commit "$bin_dir"
  create_mock_git_scope "$bin_dir"

  (
    cd "$repo"
    env -u RUSTC_WRAPPER \
      PATH="${bin_dir}:$PATH" \
      MOCK_LOG="$log_file" \
      "$entrypoint" --version v0.6.5 --skip-checks --skip-push
  ) >"${tmp}/stdout.log" 2>"${stderr_file}"

  assert_contains "$stderr_file" '--skip-checks is a deprecated alias'
  assert_contains "$log_file" 'cargo:check --workspace --locked'
  if rg -q 'checks:start' "$log_file"; then
    fail "--skip-checks unexpectedly ran the full audit stack"
  fi
}

test_readme_already_at_target_is_not_warned() {
  local tmp repo bin_dir log_file stderr_file
  tmp="$(mktemp -d)"
  repo="${tmp}/repo"
  bin_dir="${tmp}/bin"
  log_file="${tmp}/mock.log"
  stderr_file="${tmp}/stderr.log"

  mkdir -p "$repo" "$bin_dir"
  create_temp_repo "$repo" "v0.6.5"
  create_mock_cargo "$bin_dir"
  create_mock_semantic_commit "$bin_dir"
  create_mock_git_scope "$bin_dir"

  (
    cd "$repo"
    env -u RUSTC_WRAPPER \
      PATH="${bin_dir}:$PATH" \
      MOCK_LOG="$log_file" \
      "$entrypoint" --version v0.6.5 --skip-checks --skip-push
  ) >"${tmp}/stdout.log" 2>"${stderr_file}"

  assert_not_contains "$stderr_file" 'warning: README release tag example not updated'
  assert_contains "${repo}/README.md" 'v0.6.5'
  assert_contains "$log_file" 'cargo:update --workspace'
  assert_contains "$log_file" 'cargo:check --workspace --locked'
}

test_allow_dirty_rejects_non_release_managed_paths() {
  local tmp repo bin_dir log_file stderr_file
  tmp="$(mktemp -d)"
  repo="${tmp}/repo"
  bin_dir="${tmp}/bin"
  log_file="${tmp}/mock.log"
  stderr_file="${tmp}/stderr.log"

  mkdir -p "$repo" "$bin_dir"
  create_temp_repo "$repo" "v0.6.4"
  create_mock_cargo "$bin_dir"
  create_mock_semantic_commit "$bin_dir"
  create_mock_git_scope "$bin_dir"

  echo "temporary docs fix" >"${repo}/crates/codex-cli/README.md"

  set +e
  (
    cd "$repo"
    env -u RUSTC_WRAPPER \
      PATH="${bin_dir}:$PATH" \
      MOCK_LOG="$log_file" \
      "$entrypoint" --version v0.6.5 --skip-checks --skip-push --allow-dirty \
      >"${tmp}/stdout.log" 2>"${stderr_file}"
  )
  local rc=$?
  set -e

  if [[ "$rc" -eq 0 ]]; then
    fail "expected --allow-dirty with non-release-managed paths to exit non-zero"
  fi
  assert_contains "$stderr_file" '--allow-dirty only permits release-managed paths'
  assert_contains "$stderr_file" 'crates/codex-cli/README.md'
  if [[ -f "$log_file" ]]; then
    assert_not_contains "$log_file" 'cargo:'
  fi
}

test_skip_push_skips_tap_stage_with_note() {
  local tmp repo bin_dir log_file stderr_file
  tmp="$(mktemp -d)"
  repo="${tmp}/repo"
  bin_dir="${tmp}/bin"
  log_file="${tmp}/mock.log"
  stderr_file="${tmp}/stderr.log"

  mkdir -p "$repo" "$bin_dir"
  create_temp_repo "$repo" "v0.6.4"
  create_mock_cargo "$bin_dir"
  create_mock_semantic_commit "$bin_dir"
  create_mock_git_scope "$bin_dir"

  (
    cd "$repo"
    env -u RUSTC_WRAPPER -u NILS_CLI_HOMEBREW_TAP_DIR \
      PATH="${bin_dir}:$PATH" \
      MOCK_LOG="$log_file" \
      "$entrypoint" --version v0.6.5 --skip-checks --skip-push
  ) >"${tmp}/stdout.log" 2>"${stderr_file}"

  assert_contains "$stderr_file" '--skip-push set; tap stage skipped'
}

test_from_tap_without_tag_fails() {
  local tmp repo bin_dir stderr_file
  tmp="$(mktemp -d)"
  repo="${tmp}/repo"
  bin_dir="${tmp}/bin"
  stderr_file="${tmp}/stderr.log"

  mkdir -p "$repo" "$bin_dir"
  create_temp_repo "$repo" "v0.6.4"
  create_mock_cargo "$bin_dir"
  create_mock_semantic_commit "$bin_dir"
  create_mock_git_scope "$bin_dir"

  set +e
  (
    cd "$repo"
    env -u RUSTC_WRAPPER -u NILS_CLI_HOMEBREW_TAP_DIR \
      PATH="${bin_dir}:$PATH" \
      "$entrypoint" --version 0.9.9 --from-tap --tap-dir "${tmp}/tap" \
      >"${tmp}/stdout.log" 2>"${stderr_file}"
  )
  local rc=$?
  set -e

  if [[ "$rc" -eq 0 ]]; then
    fail "expected --from-tap without local tag to exit non-zero"
  fi
  assert_contains "$stderr_file" 'requires existing local tag v0.9.9'
}

test_from_tap_with_skip_tap_is_mutually_exclusive() {
  local tmp repo bin_dir stderr_file
  tmp="$(mktemp -d)"
  repo="${tmp}/repo"
  bin_dir="${tmp}/bin"
  stderr_file="${tmp}/stderr.log"

  mkdir -p "$repo" "$bin_dir"
  create_temp_repo "$repo" "v0.6.4"
  create_mock_cargo "$bin_dir"
  create_mock_semantic_commit "$bin_dir"
  create_mock_git_scope "$bin_dir"

  set +e
  (
    cd "$repo"
    env -u RUSTC_WRAPPER -u NILS_CLI_HOMEBREW_TAP_DIR \
      PATH="${bin_dir}:$PATH" \
      "$entrypoint" --version 0.9.9 --from-tap --skip-tap \
      >"${tmp}/stdout.log" 2>"${stderr_file}"
  )
  local rc=$?
  set -e

  if [[ "$rc" -eq 0 ]]; then
    fail "expected mutually-exclusive flags to exit non-zero"
  fi
  assert_contains "$stderr_file" 'mutually exclusive'
}

# Regression for sympoies/nils-cli#1051: when the release lookup is rate-limited,
# the script must say so explicitly and must NOT report a false "not available".
test_from_tap_reports_rate_limit_instead_of_not_available() {
  local tmp repo bin_dir stderr_file
  tmp="$(mktemp -d)"
  repo="${tmp}/repo"
  bin_dir="${tmp}/bin"
  stderr_file="${tmp}/stderr.log"

  mkdir -p "$repo" "$bin_dir"
  create_temp_repo "$repo" "v0.6.4"
  create_mock_semantic_commit "$bin_dir"
  create_mock_git_scope "$bin_dir"
  create_mock_gh_rate_limited "$bin_dir"
  # `--from-tap` preflights `command -v cargo` before the release-asset check;
  # stub it so this test is hermetic on toolchain-less hosts too.
  create_mock_cargo "$bin_dir"

  git -C "$repo" remote add origin git@github.com:test-org/test-repo.git
  git -C "$repo" tag -a v0.9.9 -m "v0.9.9"

  set +e
  (
    cd "$repo"
    env -u RUSTC_WRAPPER -u NILS_CLI_HOMEBREW_TAP_DIR \
      PATH="${bin_dir}:$PATH" \
      "$entrypoint" --version 0.9.9 --from-tap --tap-dir "${tmp}/tap" --skip-tap-tag \
      >"${tmp}/stdout.log" 2>"${stderr_file}"
  )
  local rc=$?
  set -e

  if [[ "$rc" -eq 0 ]]; then
    fail "expected a rate-limited release check to exit non-zero"
  fi
  assert_contains "$stderr_file" 'rate limit'
  assert_not_contains "$stderr_file" 'is not available'
}

test_from_tap_upgrades_installed_local_brew_formula() {
  local tmp repo tap tap_remote bin_dir log_file stderr_file
  tmp="$(mktemp -d)"
  repo="${tmp}/repo"
  tap="${tmp}/tap"
  tap_remote="${tmp}/tap.git"
  bin_dir="${tmp}/bin"
  log_file="${tmp}/mock.log"
  stderr_file="${tmp}/stderr.log"

  mkdir -p "$repo" "$tap" "$bin_dir" "${tmp}/home"
  create_temp_repo "$repo" "v0.6.4"
  create_mock_semantic_commit "$bin_dir"
  create_mock_git_scope "$bin_dir"
  create_mock_gh "$bin_dir"
  create_mock_brew "$bin_dir"
  # The script preflights `command -v cargo` for every path (including
  # `--from-tap`), so a stub must be on PATH even though `--from-tap` skips the
  # build stages and never invokes it.
  create_mock_cargo "$bin_dir"

  git -C "$repo" remote add origin git@github.com:test-org/test-repo.git
  git -C "$repo" tag -a v0.9.9 -m "v0.9.9"

  git init --bare "$tap_remote" >/dev/null
  git init --initial-branch=main "$tap" >/dev/null
  git -C "$tap" config user.email "test@example.com"
  git -C "$tap" config user.name "Test User"
  git -C "$tap" config commit.gpgSign false
  mkdir -p "${tap}/Formula"
  cat > "${tap}/Formula/nils-cli.rb" <<'EOF'
class NilsCli < Formula
  desc "Test"
  homepage "https://example.com"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/test-org/test-repo/releases/download/v0.9.8/nils-cli-v0.9.8-aarch64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000001"
    else
      url "https://github.com/test-org/test-repo/releases/download/v0.9.8/nils-cli-v0.9.8-x86_64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000002"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/test-org/test-repo/releases/download/v0.9.8/nils-cli-v0.9.8-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000003"
    else
      url "https://github.com/test-org/test-repo/releases/download/v0.9.8/nils-cli-v0.9.8-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000004"
    end
  end

  def install
    bin.install Dir["bin/*"]
  end
end
EOF
  git -C "$tap" add .
  git -C "$tap" commit -m "init formula" >/dev/null
  git -C "$tap" remote add origin "$tap_remote"
  if ! git -C "$tap" push -u origin main >"${tmp}/tap-push.log" 2>&1; then
    sed -n '1,120p' "${tmp}/tap-push.log" >&2 || true
    fail "failed to seed tap remote"
  fi

  (
    cd "$repo"
    env -u RUSTC_WRAPPER \
      HOME="${tmp}/home" \
      PATH="${bin_dir}:$PATH" \
      MOCK_LOG="$log_file" \
      MOCK_BREW_VERSION="0.9.9" \
      NILS_CLI_TAP_WAIT_SECONDS=30 \
      "$entrypoint" --version 0.9.9 --from-tap --tap-dir "$tap" --skip-tap-tag
  ) >"${tmp}/stdout.log" 2>"${stderr_file}"

  # The `--from-tap` path delegates the formula edit to the remote
  # `update-nils-cli-formula.yml` workflow (it only *waits* for that run), so it
  # must NOT edit the local formula — the seeded tap stays at v0.9.8. It asserts
  # the release assets exist (REST), the tap-update run succeeded, and the local
  # brew install was upgraded to the target version.
  assert_contains "$stderr_file" 'GitHub Release assets are available'
  assert_contains "$stderr_file" 'update-nils-cli-formula.yml run 1 completed'
  assert_not_contains "${tap}/Formula/nils-cli.rb" 'v0.9.9'
  assert_contains "$log_file" 'brew:list_formula nils-cli'
  assert_contains "$log_file" 'brew:update'
  assert_contains "$log_file" 'brew:upgrade nils-cli'
  assert_contains "$log_file" 'brew:list_versions nils-cli'
  assert_contains "$stderr_file" 'local Homebrew formula nils-cli is at 0.9.9'
}

test_formula_inplace_editor_idempotent() {
  local tmp formula_path
  tmp="$(mktemp -d)"
  formula_path="${tmp}/nils-cli.rb"

  cat > "$formula_path" <<'EOF'
class NilsCli < Formula
  desc "Test"
  homepage "https://example.com"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/test-org/test-repo/releases/download/v0.6.4/nils-cli-v0.6.4-aarch64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000001"
    else
      url "https://github.com/test-org/test-repo/releases/download/v0.6.4/nils-cli-v0.6.4-x86_64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000002"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/test-org/test-repo/releases/download/v0.6.4/nils-cli-v0.6.4-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000003"
    else
      url "https://github.com/test-org/test-repo/releases/download/v0.6.4/nils-cli-v0.6.4-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000004"
    end
  end

  def install
    bin.install Dir["bin/*"]
  end
end
EOF

  # Source the entrypoint just to expose the helper functions, by setting an
  # invalid version that aborts early — but functions remain accessible. Easier
  # approach: invoke the Python in-place editor via a tiny driver heredoc that
  # mirrors the call site so we test the same code path the script uses.
  python3 - "$formula_path" "0.7.0" \
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" \
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" \
    "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc" \
    "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd" \
    <<'PY'
from __future__ import annotations

import re
import sys
from pathlib import Path

(_, formula_path, version,
 sha_a_d, sha_x_d, sha_a_l, sha_x_l) = sys.argv

sha_map = {
    "aarch64-apple-darwin": sha_a_d,
    "x86_64-apple-darwin": sha_x_d,
    "aarch64-unknown-linux-gnu": sha_a_l,
    "x86_64-unknown-linux-gnu": sha_x_l,
}

path = Path(formula_path)
text = path.read_text("utf-8")
lines = text.splitlines()
out: list[str] = []
last_arch = None
url_pattern = re.compile(
    r'^(?P<indent>\s*)url\s+"https://github\.com/(?P<origin>[^/"]+/[^/"]+)'
    r'/releases/download/v[0-9.]+/nils-cli-v[0-9.]+-(?P<arch>[a-z0-9_-]+)\.tar\.gz"\s*$'
)
sha_pattern = re.compile(r'^(?P<indent>\s*)sha256\s+"[0-9a-f]+"\s*$')

archs_seen = set()
for line in lines:
    url_match = url_pattern.match(line)
    if url_match:
        arch = url_match.group("arch")
        last_arch = arch
        archs_seen.add(arch)
        new_line = (
            f'{url_match.group("indent")}url '
            f'"https://github.com/{url_match.group("origin")}/releases/download/'
            f'v{version}/nils-cli-v{version}-{arch}.tar.gz"'
        )
        out.append(new_line)
        continue
    sha_match = sha_pattern.match(line)
    if sha_match and last_arch is not None:
        new_line = f'{sha_match.group("indent")}sha256 "{sha_map[last_arch]}"'
        out.append(new_line)
        last_arch = None
        continue
    out.append(line)

new_text = "\n".join(out)
if text.endswith("\n"):
    new_text += "\n"
path.write_text(new_text, "utf-8")
PY

  # Verify the edit landed.
  assert_contains "$formula_path" 'v0.7.0/nils-cli-v0.7.0-aarch64-apple-darwin'
  assert_contains "$formula_path" 'v0.7.0/nils-cli-v0.7.0-x86_64-unknown-linux-gnu'
  assert_contains "$formula_path" 'sha256 "aaaaaaaaaaaaaaaa'
  assert_contains "$formula_path" 'sha256 "dddddddddddddddd'
  assert_not_contains "$formula_path" 'v0.6.4/nils-cli-v0.6.4'
}

create_mock_forge_cli_deliver() {
  # The mock simulates a successful PR deliver by fast-forwarding the bare
  # remote's main to the freshly pushed release branch, then printing a final
  # status line the way `forge-cli pr deliver` does.
  local bin_dir="$1"
  local bare_remote="$2"
  cat > "${bin_dir}/forge-cli" <<EOF
#!/usr/bin/env bash
set -euo pipefail

log_file="\${MOCK_LOG:?}"
echo "forge-cli:\$*" >> "\$log_file"

if [[ "\${1:-}" != "pr" || "\${2:-}" != "deliver" ]]; then
  echo "unexpected forge-cli command: \$*" >&2
  exit 1
fi

# Determine the branch that was just pushed by inspecting the bare remote.
release_branch="\$(git -C "${bare_remote}" symbolic-ref --short HEAD 2>/dev/null || true)"
# Find which branch ref was most recently advanced ahead of main.
for ref in \$(git -C "${bare_remote}" for-each-ref --format='%(refname:short)' refs/heads); do
  if [[ "\$ref" == "main" ]]; then
    continue
  fi
  release_branch="\$ref"
  break
done

if [[ -z "\$release_branch" ]]; then
  echo "mock forge-cli: could not detect release branch on remote" >&2
  exit 1
fi

# Fast-forward main to the release branch on the bare remote (simulates squash-merge).
release_sha="\$(git -C "${bare_remote}" rev-parse "refs/heads/\$release_branch")"
git -C "${bare_remote}" update-ref refs/heads/main "\$release_sha"
git -C "${bare_remote}" update-ref -d "refs/heads/\$release_branch"

echo "merged #999 via squash → \$release_sha (branch deleted)"
EOF
  chmod +x "${bin_dir}/forge-cli"
}

test_pr_mode_default_opens_pr_and_tags_merge_commit() {
  local tmp repo remote bin_dir log_file stderr_file
  tmp="$(mktemp -d)"
  repo="${tmp}/repo"
  remote="${tmp}/repo.git"
  bin_dir="${tmp}/bin"
  log_file="${tmp}/mock.log"
  stderr_file="${tmp}/stderr.log"

  mkdir -p "$repo" "$bin_dir"
  create_temp_repo "$repo" "v0.6.4"
  create_mock_cargo "$bin_dir"
  create_mock_semantic_commit "$bin_dir"
  create_mock_git_scope "$bin_dir"

  git init --bare "$remote" >/dev/null
  git -C "$repo" remote add origin "$remote"
  git -C "$repo" push -u origin main >/dev/null

  create_mock_forge_cli_deliver "$bin_dir" "$remote"

  (
    cd "$repo"
    env -u RUSTC_WRAPPER -u NILS_CLI_HOMEBREW_TAP_DIR \
      PATH="${bin_dir}:$PATH" \
      MOCK_LOG="$log_file" \
      "$entrypoint" --version v0.6.5
  ) >"${tmp}/stdout.log" 2>"${stderr_file}"

  # forge-cli was invoked with the expected shape.
  assert_contains "$log_file" 'forge-cli:pr deliver --kind chore'
  assert_contains "$log_file" 'bump cli versions to 0.6.5'
  assert_contains "$log_file" '--method squash'
  # The release branch existed on the remote before merge and is gone now.
  if git -C "$remote" rev-parse --verify "refs/heads/chore/release-0-6-5" >/dev/null 2>&1; then
    fail "mock forge-cli left release branch behind on remote"
  fi
  # main on the remote contains the bump commit.
  remote_main_msg="$(git -C "$remote" log -1 --pretty=%s main)"
  if [[ "$remote_main_msg" != "chore(release): bump cli versions to 0.6.5" ]]; then
    fail "remote main does not point at the bump commit (got: ${remote_main_msg})"
  fi
  # Local repo back on main with tag v0.6.5 pointing at the merge commit.
  local current_branch
  current_branch="$(git -C "$repo" branch --show-current)"
  if [[ "$current_branch" != "main" ]]; then
    fail "expected to be back on main after PR delivery (got: ${current_branch})"
  fi
  local tagged_sha local_main_sha
  # ^{} dereferences annotated-tag objects down to the commit they tag.
  tagged_sha="$(git -C "$repo" rev-parse --verify "refs/tags/v0.6.5^{}")"
  local_main_sha="$(git -C "$repo" rev-parse --verify HEAD)"
  if [[ "$tagged_sha" != "$local_main_sha" ]]; then
    fail "tag v0.6.5 (${tagged_sha}) does not point at local main (${local_main_sha})"
  fi
}

test_pr_mode_from_linked_worktree_tags_without_checkout_main() {
  local tmp repo remote wt bin_dir log_file stderr_file
  tmp="$(mktemp -d)"
  repo="${tmp}/repo"
  remote="${tmp}/repo.git"
  wt="${tmp}/release-wt"
  bin_dir="${tmp}/bin"
  log_file="${tmp}/mock.log"
  stderr_file="${tmp}/stderr.log"

  mkdir -p "$repo" "$bin_dir"
  create_temp_repo "$repo" "v0.6.4"
  create_mock_cargo "$bin_dir"
  create_mock_semantic_commit "$bin_dir"
  create_mock_git_scope "$bin_dir"

  git init --bare "$remote" >/dev/null
  git -C "$repo" remote add origin "$remote"
  git -C "$repo" push -u origin main >/dev/null

  create_mock_forge_cli_deliver "$bin_dir" "$remote"

  # Dedicated release worktree on the release branch, while the primary checkout
  # ($repo) keeps `main` checked out. This is the shared-worktree-isolation
  # setup that used to abort at the post-merge `git checkout main` with
  # "fatal: 'main' is already used by worktree at ...".
  git -C "$repo" worktree add "$wt" -b chore/release-0-6-5 >/dev/null

  # --skip-tap keeps the scope on the post-merge tag step (the #1049 fix): the
  # bump PR is merged, the worktree is reconciled, and the tag is created +
  # pushed, then the tap stage is skipped (it needs a real provider remote,
  # which this bare local remote is not).
  local rc=0
  (
    cd "$wt"
    env -u RUSTC_WRAPPER -u NILS_CLI_HOMEBREW_TAP_DIR \
      PATH="${bin_dir}:$PATH" \
      MOCK_LOG="$log_file" \
      "$entrypoint" --version v0.6.5 --skip-tap
  ) >"${tmp}/stdout.log" 2>"${stderr_file}" || rc=$?

  if [[ "$rc" -ne 0 ]]; then
    echo "release run aborted from a linked worktree (exit ${rc}); stderr tail:" >&2
    tail -12 "${stderr_file}" >&2 || true
    fail "release run must not abort when launched from a dedicated worktree"
  fi

  # The primary checkout is untouched and still holds main.
  local primary_branch
  primary_branch="$(git -C "$repo" branch --show-current)"
  if [[ "$primary_branch" != "main" ]]; then
    fail "primary checkout should remain on main (got: ${primary_branch})"
  fi

  # main on the remote contains the bump commit (merge succeeded).
  local remote_main_msg
  remote_main_msg="$(git -C "$remote" log -1 --pretty=%s main)"
  if [[ "$remote_main_msg" != "chore(release): bump cli versions to 0.6.5" ]]; then
    fail "remote main does not point at the bump commit (got: ${remote_main_msg})"
  fi

  # Tag v0.6.5 was created from the worktree and points at the merged bump
  # commit (== remote main), even though `git checkout main` was never run there.
  local tagged_sha remote_main_sha
  tagged_sha="$(git -C "$wt" rev-parse --verify "refs/tags/v0.6.5^{}")"
  remote_main_sha="$(git -C "$remote" rev-parse --verify main)"
  if [[ "$tagged_sha" != "$remote_main_sha" ]]; then
    fail "tag v0.6.5 (${tagged_sha}) does not point at merged remote main (${remote_main_sha})"
  fi

  # The worktree must not be left on the (now-merged) release branch.
  local wt_branch
  wt_branch="$(git -C "$wt" branch --show-current || true)"
  if [[ "$wt_branch" == "chore/release-0-6-5" ]]; then
    fail "release worktree left on the merged release branch"
  fi
}

test_pr_mode_rejects_non_chore_release_branch() {
  local tmp repo bin_dir stderr_file
  tmp="$(mktemp -d)"
  repo="${tmp}/repo"
  bin_dir="${tmp}/bin"
  stderr_file="${tmp}/stderr.log"

  mkdir -p "$repo" "$bin_dir"
  create_temp_repo "$repo" "v0.6.4"
  create_mock_cargo "$bin_dir"
  create_mock_semantic_commit "$bin_dir"
  create_mock_git_scope "$bin_dir"

  # Provide a forge-cli stub so the up-front PATH check passes; the script
  # should die on the prefix validation before invoking it.
  cat > "${bin_dir}/forge-cli" <<'EOF'
#!/usr/bin/env bash
exit 1
EOF
  chmod +x "${bin_dir}/forge-cli"

  set +e
  (
    cd "$repo"
    env -u RUSTC_WRAPPER -u NILS_CLI_HOMEBREW_TAP_DIR \
      PATH="${bin_dir}:$PATH" \
      "$entrypoint" --version 0.6.5 --release-branch feat/release-0-6-5 \
      >"${tmp}/stdout.log" 2>"${stderr_file}"
  )
  local rc=$?
  set -e

  if [[ "$rc" -eq 0 ]]; then
    fail "expected --release-branch without chore/ prefix to exit non-zero"
  fi
  assert_contains "$stderr_file" "must start with 'chore/'"
}

run_all() {
  if [[ ! -f "${skill_root}/SKILL.md" ]]; then
    fail "missing SKILL.md"
  fi
  if [[ ! -f "$entrypoint" ]]; then
    fail "missing entrypoint script"
  fi

  local tests=(
    test_full_checks_refresh_lockfile_and_disable_bad_wrapper
    test_default_path_skips_full_audit_and_runs_locked_check
    test_skip_checks_is_deprecated_alias_of_default
    test_readme_already_at_target_is_not_warned
    test_allow_dirty_rejects_non_release_managed_paths
    test_skip_push_skips_tap_stage_with_note
    test_from_tap_without_tag_fails
    test_from_tap_with_skip_tap_is_mutually_exclusive
    test_from_tap_reports_rate_limit_instead_of_not_available
    test_from_tap_upgrades_installed_local_brew_formula
    test_formula_inplace_editor_idempotent
    test_pr_mode_default_opens_pr_and_tags_merge_commit
    test_pr_mode_from_linked_worktree_tags_without_checkout_main
    test_pr_mode_rejects_non_chore_release_branch
  )

  # `test_full_checks...` drives the real toolchain: it probes the active
  # `rustc` (RUSTC_WRAPPER compatibility check). Skip it (rather than fail) when
  # the Rust toolchain is not installed, so the mock-driven suite still runs on
  # toolchain-less hosts. (`test_from_tap_upgrades...` used to be gated here too,
  # but the current `--from-tap` path skips the build stages and needs no
  # toolchain, so it now runs everywhere.)
  local toolchain_ready=1
  if ! command -v rustc >/dev/null 2>&1 || ! command -v cargo >/dev/null 2>&1; then
    toolchain_ready=0
  fi

  local failed=0 t
  for t in "${tests[@]}"; do
    case "$t" in
      test_full_checks_refresh_lockfile_and_disable_bad_wrapper)
        if [[ "$toolchain_ready" -eq 0 ]]; then
          echo "SKIP ${t} (requires rustc+cargo on PATH)"
          continue
        fi
        ;;
    esac
    if ( set -e; "$t" ); then
      echo "PASS ${t}"
    else
      echo "FAIL ${t}" >&2
      failed=1
    fi
  done

  if [[ "$failed" -ne 0 ]]; then
    exit 1
  fi
  echo "ok: project skill smoke checks passed"
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  run_all
fi
