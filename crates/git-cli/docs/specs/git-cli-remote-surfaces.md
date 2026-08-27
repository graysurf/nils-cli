# git-cli Remote Surfaces

## Ownership

This is a crate-local specification for `git-cli push`,
`git-cli sync-default`, and `git-cli sync-branch`.

## Why These Exist

Agent policy assigns an owner to every Git mutation an agent performs: commits
go through `semantic-commit`, worktrees through `git-cli worktree`, PR/MR
records and merges through `forge-cli pr`, and default-branch delivery through
`semantic-commit default-branch` / `forge-cli repo push-default`.

Three mutations had no owner:

- publishing a feature branch, and
- advancing the local default branch to a commit already on its remote, and
- advancing a checked-out persistent integration branch to its published head.

Both had to fall back to raw `git`, which the delivery guard is built to
distrust because a raw invocation cannot prove what it will touch. These two
commands close that gap by making the safe cases provable rather than inferred.

## `git-cli push`

```text
git-cli push [--remote <name>] [--expect-default <branch>] [--bootstrap]
             [--force-with-lease] [--dry-run] [--format text|json]
```

Publishes the checked-out branch to the same-named branch on `<remote>`
(default `origin`).

The push always uses the fully qualified refspec
`refs/heads/<branch>:refs/heads/<branch>`. That is the point of the command: it
removes every input that makes a bare `git push` unclassifiable — `push.default`,
`remote.pushDefault`, configured push refspecs, and upstream inference all stop
affecting the destination. The only way the command could reach the default
branch is by being *on* it, which it refuses.

It adds `--set-upstream` whenever the configured upstream is not already this
branch's own ref on this remote. That covers first publish for a branch created
by `git-cli worktree add --no-track`, and it repairs a branch created before
that flag existed, which carries the *default* branch as its upstream. Checking
only for the presence of an upstream would leave exactly the broken case
unrepaired.

Refusals:

| `error.code` | Condition |
| --- | --- |
| `detached-head` | HEAD is not attached to a branch |
| `default-branch-unresolved` | `refs/remotes/<remote>/HEAD` is not cached and no `--expect-default` was given |
| `default-branch-unverifiable` | the remote head is not cached and the checked-out branch is a conventional default-branch name |
| `expect-default-mismatch` | `--expect-default` disagrees with the cached remote head |
| `refuse-default-branch` | the checked-out branch is the remote's default branch |
| `remote-has-no-branches` | the remote advertises no refs, so `--bootstrap` is the route |
| `bootstrap-remote-not-empty` | `--bootstrap` was passed but the remote already has refs |
| `bootstrap-conflicting-flag` | `--bootstrap` was combined with `--expect-default` or `--force-with-lease` |
| `remote-unreadable` | `--bootstrap` could not list the remote's refs, so emptiness is unproven |
| `unknown-remote` | `<remote>` is not configured |

`--expect-default` names what the default branch *is* when the remote HEAD is
not cached locally, which is the offline path. It is an escape hatch, never a
second opinion, so it can only ever *add* a refusal:

- when the remote head **is** cached, cached truth wins and a disagreeing
  assertion is `expect-default-mismatch`;
- when it is **not** cached, the assertion cannot clear a branch whose name is
  conventionally a default (`main`, `master`, `trunk`, `develop`,
  `development`, `default`) — that is `default-branch-unverifiable`.

Without those two rules `--expect-default develop` while standing on `main`
would publish the default branch, which is the thing this command exists to
refuse.

Pushing the default branch is a delivery decision, not a publish step, so
`refuse-default-branch` points at `forge-cli repo push-default` rather than
offering a flag.

### `--bootstrap`

A remote that advertises no refs has no default branch, so publishing its first
branch cannot move one. That is the single case none of the rules above can
satisfy: there is nothing to cache, `git remote set-head <remote> --auto` fails
because the remote has no HEAD to read, `--expect-default` is refused as
unverifiable for a conventional name, and `forge-cli repo push-default` needs an
expected base that does not exist. Before `--bootstrap` a newly created
repository could not receive its first branch through any governed surface.

The safety argument is the emptiness, and it is checked against the remote with
`git ls-remote`, never inferred from local state — a fresh clone has no
remote-tracking refs either. Emptiness that cannot be established is
`remote-unreadable`, not an assumption. The push itself is an ordinary
create-only push with no force of any kind, so a remote that gained a ref
between the check and the push rejects it as a non-fast-forward.

Because there is no default branch and no prior value to lease against,
`--expect-default` and `--force-with-lease` are refused alongside it rather than
ignored. `default_branch` is `null` in the result, and `bootstrapped` is `true`.

When the ordinary path fails only because the remote is empty, the refusal is
`remote-has-no-branches` and names this flag, instead of the generic hint to
cache a remote head that cannot exist yet.

## `git-cli sync-default`

```text
git-cli sync-default [--remote <name>] [--no-fetch] [--dry-run]
                     [--format text|json]
```

Fast-forwards the local default branch to its remote-tracking ref. Fetches that
one ref first unless `--no-fetch` is passed.

The safety argument is that the local ref only ever moves forward onto a commit
that is already published: nothing is authored, no content changes, nothing is
published, and `git reset --hard @{1}` reverses it. Anything else is refused.

Three strategies, selected by where the default branch is checked out:

| `strategy` | Condition | Mechanism |
| --- | --- | --- |
| `noop` | local and remote refs already match | none |
| `merge-ff-only` | the default branch is checked out here | `git merge --ff-only` after a clean-checkout check |
| `update-ref` | no worktree holds the default branch | `git update-ref <ref> <new> <old>`, a compare-and-swap |

`update-ref` is the everyday agent case — working on a topic branch while local
`main` lags — and it moves the ref without touching any working tree.

Refusals:

| `error.code` | Condition |
| --- | --- |
| `not-fast-forward` | the local default branch carries commits the remote does not have |
| `dirty-checkout` | the default branch is checked out here with staged or unstaged changes |
| `default-branch-checked-out-elsewhere` | another worktree holds the default branch |
| `local-default-branch-missing` | the repository has no local default branch |
| `remote-default-branch-missing` | the remote-tracking ref does not resolve |
| `default-branch-unresolved` | `refs/remotes/<remote>/HEAD` is not cached |
| `git-fetch-failed` | the fetch failed; `--no-fetch` syncs against the already-fetched ref |

## `git-cli sync-branch`

```text
git-cli sync-branch [--remote <name>] [--no-fetch] [--dry-run]
                    [--format text|json]
```

Fast-forwards the checked-out non-default branch to the same-named branch on
`<remote>`. The branch must already track exactly `refs/heads/<branch>` on that
remote, so the command cannot infer or redirect the destination. Unless
`--no-fetch` is passed, it fetches only that branch through an explicit
fully-qualified refspec.

The command refuses detached HEAD, the remote default branch, an unknown or
mismatched upstream, divergence, and a dirty checkout. It uses
`git merge --ff-only` for the sole mutation; it never authors, rebases, resets,
pushes, or changes upstream configuration.

## JSON Contract

Both commands accept `--format text|json` and use the shared workspace envelope:

- `cli.git-cli.push.v1`
- `cli.git-cli.sync-default.v1`
- `cli.git-cli.sync-branch.v1`

`push` returns `branch`, `remote`, `remote_branch`, `refspec`, `head`,
`default_branch`, `pushed`, `dry_run`, `created_remote_branch`, `bootstrapped`,
`upstream`, and `forced`. `default_branch` is `null` exactly when
`bootstrapped` is `true`.

`sync-default` returns `default_branch`, `remote`, `remote_ref`,
`previous_head`, `new_head`, `strategy`, `already_current`, `fast_forward`,
`dry_run`, and `fetched`.

`sync-branch` returns `branch`, `remote`, `remote_ref`, `previous_head`,
`new_head`, `strategy`, `already_current`, `fast_forward`, `dry_run`, and
`fetched`.

`--dry-run` performs every read-only check and reports the strategy and target
head it would use, without mutating anything.
