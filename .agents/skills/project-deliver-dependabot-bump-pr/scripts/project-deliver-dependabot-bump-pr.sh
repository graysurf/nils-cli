#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  project-deliver-dependabot-bump-pr --pr <N> [options]
  project-deliver-dependabot-bump-pr --all-open [options]

Refresh third-party artifacts on a dependabot bump PR whose CI is failing
because THIRD_PARTY_LICENSES.md / THIRD_PARTY_NOTICES.md drifted, wait for
CI green, then merge the PR.

Required:
  --pr <N>               Dependabot PR number.
  --all-open             Deliver all open dependabot PRs sequentially.

Optional:
  --sync-main            Fast-forward merge origin/main into the PR branch (default).
  --no-sync-main         Skip fast-forward merge from main.
  --skip-merge           Push the fix commit but do not merge the PR.
  --skip-push            Generate and commit locally only (no push, no merge).
  --merge-method <m>     squash | merge | rebase (default: squash).
  --no-ci-wait           Do not block on CI via `gh pr checks --watch`.
  --allow-non-dependabot Skip dependabot author/branch guard.
  -h, --help             Show this help.

Exit codes:
  0  success
  1  failure (prereqs / push / CI / merge / etc.)
  2  usage or invalid inputs
USAGE
}

die() {
  echo "error: $*" >&2
  exit 1
}

note() {
  echo "info: $*" >&2
}

json_get() {
  local path="$1"
  python3 -c '
import json
import sys

path = [part for part in sys.argv[1].split(".") if part]
value = json.load(sys.stdin)
for part in path:
    if isinstance(value, dict):
        value = value.get(part, "")
    else:
        value = ""
        break
if isinstance(value, bool):
    print(str(value).lower())
elif value is None:
    print("")
else:
    print(value)
' "$path"
}

fetch_pr_meta() {
  local pr="$1"
  gh pr view "$pr" --json number,state,title,headRefName,headRefOid,isCrossRepository,author 2>/dev/null || true
}

parse_dep_name() {
  local title="$1"
  python3 - "$title" <<'PY'
import re, sys
title = sys.argv[1]

patterns = [
    r"bump\s+([A-Za-z0-9_\-./]+)\s+from\s+",
    r"bump\s+the\s+([A-Za-z0-9_\-./]+)\s+group",
    r"update\s+([A-Za-z0-9_\-./]+)\s+requirement",
    r"bump\s+([A-Za-z0-9_\-./]+)\s+in\s+",
]
for pattern in patterns:
    match = re.search(pattern, title, re.IGNORECASE)
    if match:
        print(match.group(1))
        sys.exit(0)
print("")
PY
}

list_open_dependabot_prs() {
  gh pr list --author app/dependabot --state open --limit 100 \
    --json number,title,headRefName,author,isCrossRepository |
    python3 -c '
import json
import sys

prs = json.load(sys.stdin)
for pr in sorted(prs, key=lambda item: int(item.get("number", 0))):
    author = (pr.get("author") or {}).get("login", "")
    head_ref = pr.get("headRefName") or ""
    if author in {"dependabot", "app/dependabot"} and head_ref.startswith("dependabot/"):
        print(pr["number"])
'
}

load_pr_meta() {
  local pr="$1"
  pr_meta="$(fetch_pr_meta "$pr")"
  if [[ -z "$pr_meta" ]]; then
    die "could not fetch PR #${pr} metadata via gh"
  fi

  pr_state="$(printf '%s' "$pr_meta" | json_get state)"
  pr_title="$(printf '%s' "$pr_meta" | json_get title)"
  pr_head_ref="$(printf '%s' "$pr_meta" | json_get headRefName)"
  pr_head_oid="$(printf '%s' "$pr_meta" | json_get headRefOid)"
  pr_author="$(printf '%s' "$pr_meta" | json_get author.login)"
  pr_cross_repo="$(printf '%s' "$pr_meta" | json_get isCrossRepository)"
}

request_dependabot_rebase_and_wait() {
  local pr="$1"
  local previous_oid="$2"
  local wait_seconds="${DEPENDABOT_BUMP_PR_REBASE_WAIT_SECONDS:-300}"
  local interval="${DEPENDABOT_BUMP_PR_POLL_INTERVAL_SECONDS:-5}"
  local deadline=$((SECONDS + wait_seconds))

  note "requesting dependabot rebase on PR #${pr}"
  gh pr comment "$pr" --body "@dependabot rebase" >/dev/null

  while (( SECONDS <= deadline )); do
    local meta
    local current_oid
    meta="$(fetch_pr_meta "$pr")"
    if [[ -n "$meta" ]]; then
      current_oid="$(printf '%s' "$meta" | json_get headRefOid)"
      if [[ -n "$current_oid" && "$current_oid" != "$previous_oid" ]]; then
        note "dependabot rebased PR #${pr}: ${previous_oid:-unknown} -> ${current_oid}"
        return 0
      fi
    fi
    sleep "$interval"
  done

  die "timed out waiting for dependabot to rebase PR #${pr}"
}

checkout_pr_branch() {
  local pr="$1"
  note "checking out PR branch"
  gh pr checkout "$pr" --force
}

sync_origin_main_or_rebase() {
  local pr="$1"
  note "fetching origin/main and fast-forward merging"
  git fetch origin main
  if git merge --ff-only origin/main 2>&1; then
    return 0
  fi

  note "origin/main is not a fast-forward for PR #${pr}; asking dependabot to rebase"
  request_dependabot_rebase_and_wait "$pr" "$pr_head_oid"
  checkout_pr_branch "$pr"
  load_pr_meta "$pr"
  git fetch origin main
  if ! git merge --ff-only origin/main 2>&1; then
    die "PR #${pr} still cannot fast-forward to origin/main after dependabot rebase"
  fi
}

push_refresh_commit() {
  local pr="$1"
  local head_ref="$2"
  local refresh_base="$3"
  local attempts="${DEPENDABOT_BUMP_PR_PUSH_ATTEMPTS:-3}"
  local attempt

  for (( attempt = 1; attempt <= attempts; attempt++ )); do
    note "pushing refresh commit to origin (attempt ${attempt}/${attempts})"
    if git push origin HEAD 2>&1; then
      return 0
    fi

    if (( attempt == attempts )); then
      break
    fi

    note "push rejected; fetching latest PR head and replaying the refresh commit"
    local remote_ref="refs/remotes/origin/${head_ref}"
    git fetch origin "refs/heads/${head_ref}:${remote_ref}"
    local new_base
    new_base="$(git rev-parse "${remote_ref}^{commit}")"
    if git merge-base --is-ancestor "$new_base" HEAD; then
      note "latest PR head is already an ancestor of HEAD; retrying push"
      continue
    fi

    if ! git rebase --onto "$new_base" "$refresh_base"; then
      git rebase --abort >/dev/null 2>&1 || true
      die "could not replay refresh commit onto origin/${head_ref}; rerun after manual conflict repair"
    fi
    refresh_base="$new_base"
    load_pr_meta "$pr"
  done

  die "git push rejected after ${attempts} attempts; dependabot may have rebased again"
}

select_ci_run_id() {
  local head_oid="$1"
  python3 -c '
import json
import sys

head_oid = sys.argv[1]
runs = json.load(sys.stdin)
for run in runs:
    if head_oid and run.get("headSha") != head_oid:
        continue
    if run.get("event") != "pull_request":
        continue
    workflow = run.get("workflowName") or run.get("name") or ""
    if workflow and workflow != "CI":
        continue
    database_id = run.get("databaseId")
    if database_id:
        print(database_id)
        sys.exit(0)
print("")
' "$head_oid"
}

# After a push, `gh pr view` can briefly report the pre-push head OID
# (read-after-write lag); a CI watch keyed on that stale OID selects the
# previous head's already-finished run and fails on its expected drift.
wait_for_pr_head_oid() {
  local pr="$1"
  local expected_oid="$2"
  local wait_seconds="${DEPENDABOT_BUMP_PR_HEAD_SYNC_WAIT_SECONDS:-120}"
  local interval="${DEPENDABOT_BUMP_PR_POLL_INTERVAL_SECONDS:-5}"
  local deadline=$((SECONDS + wait_seconds))

  note "waiting for PR #${pr} head to report pushed commit ${expected_oid}"
  while (( SECONDS <= deadline )); do
    load_pr_meta "$pr"
    if [[ "$pr_head_oid" == "$expected_oid" ]]; then
      return 0
    fi
    sleep "$interval"
  done

  note "PR #${pr} head still reports ${pr_head_oid}; watching CI for pushed commit anyway"
}

wait_for_ci() {
  local pr="$1"
  local head_ref="$2"
  local head_oid="$3"

  note "waiting for CI checks to complete on PR #${pr}"
  local checks_output
  local checks_rc
  set +e
  checks_output="$(gh pr checks "$pr" --watch --fail-fast 2>&1)"
  checks_rc=$?
  set -e
  printf '%s\n' "$checks_output"
  if [[ "$checks_rc" -eq 0 ]]; then
    return 0
  fi
  if [[ "$checks_output" != *"no checks reported"* ]]; then
    die "CI did not pass for PR #${pr}; inspect failures and retry"
  fi

  note "PR check summary has no checks yet; falling back to workflow-run watch"
  local wait_seconds="${DEPENDABOT_BUMP_PR_CI_RUN_POLL_SECONDS:-180}"
  local interval="${DEPENDABOT_BUMP_PR_POLL_INTERVAL_SECONDS:-5}"
  local deadline=$((SECONDS + wait_seconds))
  while (( SECONDS <= deadline )); do
    local runs_json
    local run_id
    runs_json="$(gh run list --branch "$head_ref" --commit "$head_oid" --event pull_request --limit 10 \
      --json databaseId,status,conclusion,headSha,workflowName,event,url 2>/dev/null || true)"
    if [[ -n "$runs_json" ]]; then
      run_id="$(printf '%s' "$runs_json" | select_ci_run_id "$head_oid")"
      if [[ -n "$run_id" ]]; then
        note "watching CI workflow run ${run_id}"
        if ! gh run watch "$run_id" --exit-status; then
          die "CI workflow run ${run_id} failed for PR #${pr}"
        fi
        return 0
      fi
    fi
    sleep "$interval"
  done

  die "no CI workflow run appeared for PR #${pr} head ${head_oid}"
}

pr_number=""
all_open=0
sync_main=1
skip_merge=0
skip_push=0
merge_method="squash"
ci_wait=1
allow_non_dependabot=0

while [[ $# -gt 0 ]]; do
  case "${1:-}" in
    --pr)
      [[ $# -ge 2 ]] || { echo "error: --pr requires a value" >&2; usage >&2; exit 2; }
      pr_number="$2"
      shift 2
      ;;
    --all-open)
      all_open=1
      shift
      ;;
    --sync-main)
      sync_main=1
      shift
      ;;
    --no-sync-main)
      sync_main=0
      shift
      ;;
    --skip-merge)
      skip_merge=1
      shift
      ;;
    --skip-push)
      skip_push=1
      shift
      ;;
    --merge-method)
      [[ $# -ge 2 ]] || { echo "error: --merge-method requires a value" >&2; usage >&2; exit 2; }
      merge_method="$2"
      shift 2
      ;;
    --no-ci-wait)
      ci_wait=0
      shift
      ;;
    --allow-non-dependabot)
      allow_non_dependabot=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: ${1:-}" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -n "$pr_number" && "$all_open" -eq 1 ]]; then
  echo "error: use either --pr <N> or --all-open, not both" >&2
  usage >&2
  exit 2
fi
if [[ -z "$pr_number" && "$all_open" -eq 0 ]]; then
  echo "error: --pr <N> or --all-open is required" >&2
  usage >&2
  exit 2
fi
if [[ -n "$pr_number" ]] && ! [[ "$pr_number" =~ ^[1-9][0-9]*$ ]]; then
  die "invalid --pr value: $pr_number (expected positive integer)"
fi

case "$merge_method" in
  squash|merge|rebase) ;;
  *) die "invalid --merge-method: $merge_method (expected squash|merge|rebase)" ;;
esac

if [[ "$skip_push" -eq 1 && "$skip_merge" -eq 0 ]]; then
  note "--skip-push implies --skip-merge; disabling merge"
  skip_merge=1
fi
if [[ "$skip_push" -eq 1 && "$ci_wait" -eq 1 ]]; then
  note "--skip-push implies --no-ci-wait; disabling CI wait"
  ci_wait=0
fi

for cmd in git cargo python3 gh semantic-commit; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    die "required command not found on PATH: $cmd"
  fi
done

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  die "must run inside a git work tree"
fi
cd "$repo_root"

generator_script="scripts/generate-third-party-artifacts.sh"
if [[ ! -f "$generator_script" ]]; then
  die "missing generator script: $generator_script"
fi

if ! git diff --quiet || ! git diff --cached --quiet; then
  die "working tree is dirty; commit or stash changes before running"
fi

if ! gh auth status >/dev/null 2>&1; then
  die "gh is not authenticated; run 'gh auth login'"
fi

starting_ref="$(git symbolic-ref --quiet --short HEAD 2>/dev/null || git rev-parse HEAD)"
note "starting ref: ${starting_ref}"

# shellcheck disable=SC2329  # invoked via trap
restore_starting_ref() {
  if [[ -z "${starting_ref:-}" ]]; then
    return 0
  fi
  if git symbolic-ref --quiet --short HEAD >/dev/null 2>&1; then
    local current
    current="$(git symbolic-ref --short HEAD)"
    if [[ "$current" == "$starting_ref" ]]; then
      return 0
    fi
  fi
  note "restoring starting ref: ${starting_ref}"
  git checkout "$starting_ref" >/dev/null 2>&1 || note "could not restore ${starting_ref}; leaving current checkout"
}
trap restore_starting_ref EXIT

script_path="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"

if [[ "$all_open" -eq 1 ]]; then
  if [[ "$starting_ref" != "main" ]]; then
    die "--all-open must start from main so each merged PR can fast-forward the queue"
  fi

  dependabot_prs=()
  while IFS= read -r queued_pr; do
    [[ -n "$queued_pr" ]] && dependabot_prs+=("$queued_pr")
  done < <(list_open_dependabot_prs)
  if [[ "${#dependabot_prs[@]}" -eq 0 ]]; then
    note "no open dependabot PRs found"
    exit 0
  fi

  note "delivering ${#dependabot_prs[@]} open dependabot PR(s): ${dependabot_prs[*]}"
  for queued_pr in "${dependabot_prs[@]}"; do
    single_args=(--pr "$queued_pr")
    if [[ "$sync_main" -eq 1 ]]; then
      single_args+=(--sync-main)
    else
      single_args+=(--no-sync-main)
    fi
    if [[ "$skip_merge" -eq 1 ]]; then
      single_args+=(--skip-merge)
    fi
    if [[ "$skip_push" -eq 1 ]]; then
      single_args+=(--skip-push)
    fi
    single_args+=(--merge-method "$merge_method")
    if [[ "$ci_wait" -eq 0 ]]; then
      single_args+=(--no-ci-wait)
    fi
    if [[ "$allow_non_dependabot" -eq 1 ]]; then
      single_args+=(--allow-non-dependabot)
    fi

    note "delivering queued dependabot PR #${queued_pr}"
    "$script_path" "${single_args[@]}"

    git checkout "$starting_ref" >/dev/null 2>&1 || die "could not restore ${starting_ref} after PR #${queued_pr}"
    if [[ "$skip_push" -eq 0 && "$skip_merge" -eq 0 ]]; then
      git fetch origin main
      git merge --ff-only origin/main
    fi
  done

  note "all queued dependabot PRs delivered"
  exit 0
fi

load_pr_meta "$pr_number"
if [[ "$pr_state" != "OPEN" ]]; then
  die "PR #${pr_number} state is ${pr_state}; expected OPEN"
fi
if [[ "$pr_cross_repo" != "false" ]]; then
  die "PR #${pr_number} is cross-repository (fork); cannot push a fix commit"
fi

if [[ "$allow_non_dependabot" -eq 0 ]]; then
  if [[ "$pr_author" != "dependabot" && "$pr_author" != "app/dependabot" ]]; then
    die "PR #${pr_number} author is '${pr_author}'; expected dependabot (use --allow-non-dependabot to override)"
  fi
  if [[ "$pr_head_ref" != dependabot/* ]]; then
    die "PR #${pr_number} head branch is '${pr_head_ref}'; expected 'dependabot/*' (use --allow-non-dependabot to override)"
  fi
fi

note "PR #${pr_number}: ${pr_title}"
note "head branch: ${pr_head_ref}"

checkout_pr_branch "$pr_number"

if [[ "$sync_main" -eq 1 ]]; then
  sync_origin_main_or_rebase "$pr_number"
fi

dep_name="$(parse_dep_name "$pr_title")"
if [[ -z "$dep_name" ]]; then
  dep_name="dependabot bump"
  note "could not parse dep name from PR title; using generic label"
else
  note "derived dep name: ${dep_name}"
fi

set +e
bash "$generator_script" --check
check_exit=$?
set -e

case "$check_exit" in
  0)
    note "third-party artifacts already up-to-date; no refresh commit needed"
    refresh_committed=0
    ;;
  1)
    note "drift detected; regenerating third-party artifacts"
    refresh_base="$(git rev-parse HEAD)"
    bash "$generator_script" --write

    git add THIRD_PARTY_LICENSES.md THIRD_PARTY_NOTICES.md
    if git diff --cached --quiet; then
      note "generator reported drift but no staged changes produced; aborting"
      die "generator did not produce any changes"
    fi

    {
      printf 'fix(ci): refresh third-party artifacts for %s bump\n\n' "$dep_name"
      printf -- '- Regenerate third-party artifacts after the %s bump.\n' "$dep_name"
    } | semantic-commit commit

    refresh_committed=1
    ;;
  *)
    die "generator check failed unexpectedly (exit ${check_exit}): bash ${generator_script} --check"
    ;;
esac

pushed_head_oid=""
if [[ "$refresh_committed" -eq 1 && "$skip_push" -eq 0 ]]; then
  push_refresh_commit "$pr_number" "$pr_head_ref" "$refresh_base"
  pushed_head_oid="$(git rev-parse HEAD)"
fi

if [[ "$ci_wait" -eq 0 ]]; then
  note "skipping CI wait (--no-ci-wait or --skip-push)"
elif [[ -n "$pushed_head_oid" ]]; then
  wait_for_pr_head_oid "$pr_number" "$pushed_head_oid"
  wait_for_ci "$pr_number" "$pr_head_ref" "$pushed_head_oid"
else
  load_pr_meta "$pr_number"
  wait_for_ci "$pr_number" "$pr_head_ref" "$pr_head_oid"
fi

if [[ "$skip_merge" -eq 1 ]]; then
  note "skipping merge (--skip-merge or --skip-push)"
  exit 0
fi

note "merging PR #${pr_number} via --${merge_method}"
if ! gh pr merge "$pr_number" "--${merge_method}"; then
  die "gh pr merge failed for PR #${pr_number} (branch protection / required reviews?)"
fi

note "PR #${pr_number} delivered"
exit 0
