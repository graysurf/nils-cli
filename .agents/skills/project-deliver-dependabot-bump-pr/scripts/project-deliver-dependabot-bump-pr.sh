#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  project-deliver-dependabot-bump-pr --pr <N> [options]

Refresh third-party artifacts on a dependabot bump PR whose CI is failing
because THIRD_PARTY_LICENSES.md / THIRD_PARTY_NOTICES.md drifted, wait for
CI green, then merge the PR.

Required:
  --pr <N>               Dependabot PR number.

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

pr_number=""
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

if [[ -z "$pr_number" ]]; then
  echo "error: --pr <N> is required" >&2
  usage >&2
  exit 2
fi
if ! [[ "$pr_number" =~ ^[1-9][0-9]*$ ]]; then
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

pr_meta="$(gh pr view "$pr_number" --json number,state,title,headRefName,isCrossRepository,author 2>/dev/null || true)"
if [[ -z "$pr_meta" ]]; then
  die "could not fetch PR #${pr_number} metadata via gh"
fi

pr_state="$(printf '%s' "$pr_meta" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("state",""))')"
pr_title="$(printf '%s' "$pr_meta" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("title",""))')"
pr_head_ref="$(printf '%s' "$pr_meta" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("headRefName",""))')"
pr_author="$(printf '%s' "$pr_meta" | python3 -c 'import json,sys; d=json.load(sys.stdin).get("author") or {}; print(d.get("login",""))')"
pr_cross_repo="$(printf '%s' "$pr_meta" | python3 -c 'import json,sys; print(str(json.load(sys.stdin).get("isCrossRepository", False)).lower())')"

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

note "checking out PR branch"
gh pr checkout "$pr_number"

if [[ "$sync_main" -eq 1 ]]; then
  note "fetching origin/main and fast-forward merging"
  git fetch origin main
  if ! git merge --ff-only origin/main 2>&1; then
    die "cannot fast-forward merge origin/main into PR branch (conflicts or non-FF); rebase manually via '@dependabot rebase' comment and retry"
  fi
fi

dep_name="$(python3 - "$pr_title" <<'PY'
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
)"
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
    bash "$generator_script" --write

    git add THIRD_PARTY_LICENSES.md THIRD_PARTY_NOTICES.md
    if git diff --cached --quiet; then
      note "generator reported drift but no staged changes produced; aborting"
      die "generator did not produce any changes"
    fi

    {
      printf 'fix(ci): refresh third-party artifacts for %s bump\n\n' "$dep_name"
      printf -- '- Regenerate THIRD_PARTY_LICENSES.md and THIRD_PARTY_NOTICES.md via scripts/generate-third-party-artifacts.sh --write after the %s bump.\n' "$dep_name"
    } | semantic-commit commit

    refresh_committed=1
    ;;
  *)
    die "generator check failed unexpectedly (exit ${check_exit}): bash ${generator_script} --check"
    ;;
esac

if [[ "$refresh_committed" -eq 1 && "$skip_push" -eq 0 ]]; then
  note "pushing refresh commit to origin"
  if ! git push origin HEAD 2>&1; then
    die "git push rejected; dependabot may have rebased — comment '@dependabot rebase' on the PR and retry"
  fi
fi

if [[ "$ci_wait" -eq 0 ]]; then
  note "skipping CI wait (--no-ci-wait or --skip-push)"
else
  note "waiting for CI checks to complete on PR #${pr_number}"
  if ! gh pr checks "$pr_number" --watch --fail-fast; then
    die "CI did not pass for PR #${pr_number}; inspect failures and retry"
  fi
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
