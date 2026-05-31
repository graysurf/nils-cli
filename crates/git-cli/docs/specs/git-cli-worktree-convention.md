# git-cli Worktree Convention

## Ownership

This is a crate-local specification for `git-cli worktree`.

## Path Convention

`git-cli worktree add <slug>` creates worktrees under:

```text
$AGENT_HOME/worktrees/<repo-key>/<branch-slug>
```

- `<repo-key>` is `<repo-basename>-<short-hash>`, where the short hash is a
  stable hash of the absolute repository root. This prevents collisions between
  repositories with the same basename.
- `<branch-slug>` is a filesystem-safe, lowercase slug derived from the user
  argument.
- The new branch is always `feat/<branch-slug>`.

If `AGENT_HOME` is unset, the CLI falls back to
`${XDG_STATE_HOME:-$HOME/.local/state}/agent-runtime-kit`.

## Commands

- `git-cli worktree add <slug> [--from <ref>] [--format text|json]`
- `git-cli worktree list [--format text|json]`
- `git-cli worktree remove <slug-or-path> [--format text|json]`
- `git-cli worktree prune [--format text|json]`

`remove` refuses to remove the primary checkout or the current worktree. It uses
`git worktree remove --force` for linked non-primary worktrees, then prunes stale
worktree metadata.

## JSON Contract

Every command accepts `--format text|json`. JSON output uses the shared
workspace envelope:

- `cli.git-cli.worktree.add.v1`
- `cli.git-cli.worktree.list.v1`
- `cli.git-cli.worktree.remove.v1`
- `cli.git-cli.worktree.prune.v1`

Error responses use stable `error.code` values such as `branch-exists`,
`worktree-path-exists`, `worktree-not-found`, `refuse-primary-worktree`, and
`git-worktree-remove-failed`.

## Interaction With plan-issue

`plan-issue cleanup-worktrees` keeps its issue-ledger targeting and
`$ISSUE_ROOT/worktrees/<mode>/<id>` dispatch convention. `git-cli` consolidates
its own worktree listing/removal parser so `git-cli worktree` and
`git-cli branch cleanup --remove-worktrees` share one code path; the
`plan-issue` flow intentionally remains separate because it is driven by
plan rows rather than the git-cli managed path convention.
