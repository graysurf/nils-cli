#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
skill_root="$(cd "${script_dir}/.." && pwd)"
entrypoint="${skill_root}/scripts/project-deliver-dependabot-bump-pr.sh"
real_git="$(command -v git)"

fail() {
  echo "error: $*" >&2
  exit 1
}

assert_contains() {
  local file="$1"
  local pattern="$2"
  if ! rg -F -q -- "$pattern" "$file"; then
    echo "error: expected pattern '$pattern' in $file" >&2
    [[ -f "$file" ]] && sed -n '1,220p' "$file" >&2
    exit 1
  fi
}

assert_not_contains() {
  local file="$1"
  local pattern="$2"
  if rg -F -q -- "$pattern" "$file"; then
    echo "error: unexpected pattern '$pattern' in $file" >&2
    sed -n '1,220p' "$file" >&2
    exit 1
  fi
}

artifact_root="${CLAUDE_KIT_STATE_HOME:-${XDG_STATE_HOME:-$HOME/.local/state}/agent-runtime-kit}/out/project-deliver-dependabot-bump-pr-tests"
mkdir -p "$artifact_root"
tmp="$(mktemp -d "${artifact_root}/dependabot-bump.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

write_mock_git() {
  local bin_dir="$1"
  cat >"${bin_dir}/git" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

log_file="${MOCK_LOG:?}"
real_git="${REAL_GIT:?}"

case "${1:-}" in
  push)
    echo "git:push:$*" >> "$log_file"
    count_file="${MOCK_PUSH_COUNT_FILE:?}"
    count=0
    [[ -f "$count_file" ]] && count="$(cat "$count_file")"
    count=$((count + 1))
    printf '%s' "$count" > "$count_file"
    if [[ "${MOCK_PUSH_REJECT_ONCE:-0}" == "1" && "$count" -eq 1 ]]; then
      echo "! [rejected] HEAD -> dependabot/cargo/foo-1.2.3 (stale info)" >&2
      exit 1
    fi
    if [[ "${MOCK_PUSH_SUCCESS:-0}" == "1" || "${MOCK_PUSH_REJECT_ONCE:-0}" == "1" ]]; then
      exit 0
    fi
    exec "$real_git" "$@"
    ;;
  fetch)
    echo "git:fetch:$*" >> "$log_file"
    if [[ "${MOCK_PUSH_REJECT_ONCE:-0}" == "1" ]]; then
      exit 0
    fi
    exec "$real_git" "$@"
    ;;
  rev-parse)
    if [[ "${MOCK_PUSH_REJECT_ONCE:-0}" == "1" && "${2:-}" == refs/remotes/origin/* ]]; then
      "$real_git" rev-parse HEAD
      exit 0
    fi
    exec "$real_git" "$@"
    ;;
  merge-base)
    if [[ "${MOCK_PUSH_REJECT_ONCE:-0}" == "1" && "${2:-}" == "--is-ancestor" ]]; then
      exit 1
    fi
    exec "$real_git" "$@"
    ;;
  rebase)
    echo "git:rebase:$*" >> "$log_file"
    if [[ "${MOCK_PUSH_REJECT_ONCE:-0}" == "1" ]]; then
      exit 0
    fi
    exec "$real_git" "$@"
    ;;
  *)
    exec "$real_git" "$@"
    ;;
esac
EOF
  chmod +x "${bin_dir}/git"
}

write_mock_cargo() {
  local bin_dir="$1"
  cat >"${bin_dir}/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
exit 0
EOF
  chmod +x "${bin_dir}/cargo"
}

write_mock_semantic_commit() {
  local bin_dir="$1"
  cat >"${bin_dir}/semantic-commit" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

[[ "${1:-}" == "commit" ]] || {
  echo "unexpected semantic-commit command: $*" >&2
  exit 1
}

msg_file="${MOCK_MESSAGE_FILE:?}"
cat > "$msg_file"
python3 - "$msg_file" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
for idx, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
    if len(line) > 100:
        print(f"commit body line {idx} exceeds 100 characters: {len(line)}", file=sys.stderr)
        sys.exit(1)
PY

git commit --allow-empty -F "$msg_file" >/dev/null
EOF
  chmod +x "${bin_dir}/semantic-commit"
}

write_mock_gh() {
  local bin_dir="$1"
  cat >"${bin_dir}/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

log_file="${MOCK_LOG:?}"
real_git="${REAL_GIT:?}"

pr_json() {
  local pr="$1"
  local dep="foo"
  if [[ "$pr" == "102" ]]; then
    dep="bar"
  fi
  cat <<JSON
{"number":${pr},"state":"OPEN","title":"chore(deps): bump ${dep} from 1.2.2 to 1.2.3","headRefName":"dependabot/cargo/${dep}-1.2.3","headRefOid":"${dep}-head-1","isCrossRepository":false,"author":{"login":"app/dependabot"}}
JSON
}

case "${1:-} ${2:-}" in
  "auth status")
    exit 0
    ;;
  "pr list")
    echo "gh:pr-list:$*" >> "$log_file"
    cat <<'JSON'
[{"number":101,"title":"chore(deps): bump foo from 1.2.2 to 1.2.3","headRefName":"dependabot/cargo/foo-1.2.3","author":{"login":"app/dependabot"},"isCrossRepository":false},{"number":102,"title":"chore(deps): bump bar from 1.2.2 to 1.2.3","headRefName":"dependabot/cargo/bar-1.2.3","author":{"login":"app/dependabot"},"isCrossRepository":false}]
JSON
    exit 0
    ;;
  "pr view")
    pr_json "${3:?}"
    exit 0
    ;;
  "pr checkout")
    pr="${3:?}"
    echo "gh:pr-checkout:${pr}:$*" >> "$log_file"
    if [[ "$pr" == "102" ]]; then
      "$real_git" checkout -B dependabot/cargo/bar-1.2.3 main >/dev/null
    else
      "$real_git" checkout -B dependabot/cargo/foo-1.2.3 main >/dev/null
    fi
    exit 0
    ;;
  "pr checks")
    echo "gh:pr-checks:$*" >> "$log_file"
    if [[ "${MOCK_CHECKS_NO_CHECKS_ONCE:-0}" == "1" ]]; then
      count_file="${MOCK_CHECKS_COUNT_FILE:?}"
      count=0
      [[ -f "$count_file" ]] && count="$(cat "$count_file")"
      count=$((count + 1))
      printf '%s' "$count" > "$count_file"
      if [[ "$count" -eq 1 ]]; then
        echo "no checks reported on the 'dependabot/cargo/foo-1.2.3' branch" >&2
        exit 1
      fi
    fi
    exit 0
    ;;
  "run list")
    echo "gh:run-list:$*" >> "$log_file"
    cat <<'JSON'
[{"databaseId":4242,"status":"in_progress","conclusion":"","headSha":"foo-head-1","workflowName":"CI","event":"pull_request","url":"https://example.test/run/4242"}]
JSON
    exit 0
    ;;
  "run watch")
    echo "gh:run-watch:$*" >> "$log_file"
    exit 0
    ;;
  "pr merge")
    echo "gh:pr-merge:$*" >> "$log_file"
    exit 0
    ;;
  "pr comment")
    echo "gh:pr-comment:$*" >> "$log_file"
    exit 0
    ;;
esac

echo "unexpected gh command: $*" >&2
exit 1
EOF
  chmod +x "${bin_dir}/gh"
}

create_temp_repo() {
  local repo_dir="$1"

  "$real_git" init --initial-branch=main "$repo_dir" >/dev/null
  "$real_git" -C "$repo_dir" config user.email "test@example.com"
  "$real_git" -C "$repo_dir" config user.name "Test User"

  mkdir -p "${repo_dir}/scripts"
  printf 'licenses-old\n' > "${repo_dir}/THIRD_PARTY_LICENSES.md"
  printf 'notices-old\n' > "${repo_dir}/THIRD_PARTY_NOTICES.md"

  cat >"${repo_dir}/scripts/generate-third-party-artifacts.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

case "${1:-}" in
  --check)
    exit 1
    ;;
  --write)
    dep="${MOCK_DEP_NAME:-foo}"
    printf 'licenses-%s\n' "$dep" > THIRD_PARTY_LICENSES.md
    printf 'notices-%s\n' "$dep" > THIRD_PARTY_NOTICES.md
    ;;
  *)
    echo "unexpected generator command: $*" >&2
    exit 2
    ;;
esac
EOF
  chmod +x "${repo_dir}/scripts/generate-third-party-artifacts.sh"

  "$real_git" -C "$repo_dir" add .
  "$real_git" -C "$repo_dir" commit -m "init" >/dev/null
}

create_case() {
  local name="$1"
  local case_dir="${tmp}/${name}"
  local repo_dir="${case_dir}/repo"
  local bin_dir="${case_dir}/bin"
  mkdir -p "$repo_dir" "$bin_dir"
  create_temp_repo "$repo_dir"
  write_mock_git "$bin_dir"
  write_mock_cargo "$bin_dir"
  write_mock_semantic_commit "$bin_dir"
  write_mock_gh "$bin_dir"

  export PATH="${bin_dir}:$PATH"
  export REAL_GIT="$real_git"
  export MOCK_LOG="${case_dir}/mock.log"
  export MOCK_MESSAGE_FILE="${case_dir}/message.txt"
  export MOCK_PUSH_COUNT_FILE="${case_dir}/push-count.txt"
  export MOCK_CHECKS_COUNT_FILE="${case_dir}/checks-count.txt"
  : > "$MOCK_LOG"

  CASE_REPO_DIR="$repo_dir"
}

run_in_case() {
  local repo_dir="$1"
  shift
  (cd "$repo_dir" && "$entrypoint" "$@")
}

create_case commit-message
repo_dir="$CASE_REPO_DIR"
run_in_case "$repo_dir" --pr 101 --no-sync-main --skip-push
assert_contains "$MOCK_MESSAGE_FILE" "fix(ci): refresh third-party artifacts for foo bump"
assert_contains "$MOCK_MESSAGE_FILE" "Regenerate third-party artifacts after the foo bump."
assert_not_contains "$MOCK_MESSAGE_FILE" "scripts/generate-third-party-artifacts.sh --write after"

create_case all-open
repo_dir="$CASE_REPO_DIR"
run_in_case "$repo_dir" --all-open --no-sync-main --skip-push
assert_contains "$MOCK_LOG" "gh:pr-list"
assert_contains "$MOCK_LOG" "gh:pr-checkout:101"
assert_contains "$MOCK_LOG" "gh:pr-checkout:102"

create_case ci-fallback
repo_dir="$CASE_REPO_DIR"
export MOCK_PUSH_SUCCESS=1
export MOCK_CHECKS_NO_CHECKS_ONCE=1
export DEPENDABOT_BUMP_PR_CI_RUN_POLL_SECONDS=1
run_in_case "$repo_dir" --pr 101 --no-sync-main --skip-merge
assert_contains "$MOCK_LOG" "gh:pr-checks"
assert_contains "$MOCK_LOG" "gh:run-list"
assert_contains "$MOCK_LOG" "gh:run-watch"
unset MOCK_PUSH_SUCCESS MOCK_CHECKS_NO_CHECKS_ONCE DEPENDABOT_BUMP_PR_CI_RUN_POLL_SECONDS

create_case push-rebase
repo_dir="$CASE_REPO_DIR"
export MOCK_PUSH_REJECT_ONCE=1
run_in_case "$repo_dir" --pr 101 --no-sync-main --no-ci-wait --skip-merge
assert_contains "$MOCK_LOG" "git:push:push origin HEAD"
assert_contains "$MOCK_LOG" "git:fetch:fetch origin refs/heads/dependabot/cargo/foo-1.2.3:refs/remotes/origin/dependabot/cargo/foo-1.2.3"
assert_contains "$MOCK_LOG" "git:rebase:rebase --onto"
unset MOCK_PUSH_REJECT_ONCE

echo "ok: project-deliver-dependabot-bump-pr tests passed"
