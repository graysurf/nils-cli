#!/usr/bin/env bash
# Block pushes that contain unsigned commits.
#
# Local pre-push safety net: verify that every local commit not yet present on
# any remote-tracking branch carries a good ("G") or valid/unknown-trust ("U")
# signature before it leaves the machine. This catches commits created in an
# environment without the signing key (sandboxed agents, CI shells) before they
# reach the remote.
#
# Pre-push only, by design: CI runs on checkouts without the user's keyring and
# often without remote-tracking refs, where this check would misfire, so it is
# intentionally NOT wired into the repository CI entrypoints. It is also
# bypassable (LEFTHOOK=0 / git push --no-verify) for deliberate escapes.

set -euo pipefail

# Commits reachable from HEAD but absent from every remote = about to be pushed.
commits="$(git rev-list HEAD --not --remotes 2>/dev/null || true)"
if [ -z "$commits" ]; then
  exit 0
fi

fail=0
while IFS= read -r sha; do
  [ -z "$sha" ] && continue
  status="$(git log -1 --format='%G?' "$sha")"
  case "$status" in
    G | U) ;;
    *)
      if [ "$fail" -eq 0 ]; then
        echo "✖ push blocked: unsigned/unverifiable commit(s):" >&2
        fail=1
      fi
      printf '  %s %s %s\n' "$status" "$sha" "$(git log -1 --format='%s' "$sha")" >&2
      ;;
  esac
done <<EOF
$commits
EOF

if [ "$fail" -ne 0 ]; then
  echo >&2
  echo "Sign the offending commits before pushing, e.g.:" >&2
  echo "  git rebase --exec 'git commit --amend --no-edit -S' <base>" >&2
  echo "%G? legend: G=good  U=good/unknown-trust  B=bad  E=cannot-check  N=none" >&2
  exit 1
fi

exit 0
