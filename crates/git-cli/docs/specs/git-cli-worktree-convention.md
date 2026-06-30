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

- `git-cli worktree add <slug> [--from <ref>] [--kind <kind>] [--format text|json]`
- `git-cli worktree list [--format text|json]`
- `git-cli worktree remove <slug-or-path> [--format text|json]`
- `git-cli worktree prune [--format text|json]`
- `git-cli worktree go <slug-or-branch-or-path> [--shell] [--format text|json]`

`--kind` selects the branch prefix (`feature`->`feat/`, `bug`->`fix/`,
`chore`->`chore/`, `docs`->`docs/`, `ci`->`ci/`, `refactor`->`refactor/`);
default `feature`.

`remove` refuses to remove the primary checkout or the current worktree. It uses
`git worktree remove --force` for linked non-primary worktrees, then prunes stale
worktree metadata.

`go` resolves a single worktree (in priority order: exact branch name, explicit
worktree path, managed slug, then worktree directory basename) and prints its
path so the caller can `cd` into it. `--shell` prints an evaluable
`cd -- <path>` command instead of the bare path, mirroring `utils root --shell`;
the committed `gxwcd` shell helper wraps it and adds worktree-name completion.

## Primary Worktree Resolution

The managed layout — `<repo-key>`, the managed/external classification, and
slug-based `add`/`remove`/`go` resolution — is anchored to the repository's
*primary* worktree, resolved as the first entry of `git worktree list`. This is
independent of the worktree the command is invoked from, so `git-cli worktree`
behaves identically from the primary checkout or from inside any linked
worktree. (`git rev-parse --show-toplevel` would otherwise return the current
linked worktree and make the managed namespace diverge.)

## JSON Contract

Every command accepts `--format text|json`. JSON output uses the shared
workspace envelope:

- `cli.git-cli.worktree.add.v1`
- `cli.git-cli.worktree.list.v1`
- `cli.git-cli.worktree.remove.v1`
- `cli.git-cli.worktree.prune.v1`
- `cli.git-cli.worktree.go.v1`

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
