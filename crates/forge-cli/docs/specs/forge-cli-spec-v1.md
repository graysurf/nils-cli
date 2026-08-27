# forge-cli Spec v1

## Purpose

This spec is the canonical contract for `forge-cli`, a new binary in the
`nils-cli` workspace. `forge-cli` provides a provider-neutral surface for
the remote forge operations that today are run directly against `gh` and
`glab` from agent-runtime-kit skills (PR/MR lifecycle, Issue lifecycle, CI
wait). Two backends ship together: GitHub (wraps `gh`) and GitLab
(wraps `glab`). Behaviour, validation, and exit semantics are identical
across backends; only the underlying subprocess and the rendered help
text differ.

Goals:

- Replace ad-hoc `gh`/`glab` invocations scattered across agent-runtime-kit
  skills with one binary that enforces branch / body / state policy at
  the type level.
- Manage provider labels from a caller-supplied machine-readable catalog
  without making `forge-cli` the taxonomy source of truth.
- Make the GitHub and GitLab lanes byte-identical from the caller's
  point of view (same flags, same envelope, same exit codes).
- Codify defaults that were previously only described in skill
  Markdown (PR body sections, branch naming, required-check gating,
  draft → ready → merge ordering).
- Stay a thin wrapper: every action delegates to `gh` or `glab` as a
  subprocess. No direct REST client, no extra auth surface, no separate
  rate-limit handling. Auth / SSO / enterprise hosts come from the
  user's existing `gh auth login` and `glab auth login` state.

Non-goals (v1):

- Release management (`gh release`, GitLab releases).
- Arbitrary `gh api` / GitLab REST passthrough — no escape hatch in v1
  on purpose; if a workflow needs it, the call belongs in a focused
  follow-up op, not a generic shim. **Deferred to v2** — see "Open
  questions / v2 candidates" for the re-evaluation criteria. v1
  callers that need a non-CRUD call must keep using `gh api` / `glab
  api` directly from the bash shell until then.
- Issue *macros* beyond create/view/edit/comment/close/reopen — the
  full plan-issue / dispatch-pr-review orchestration stays in
  agent-runtime-kit skills for now.

## Scope

In scope (v1):

- Personal work discovery: `inbox list`, `inbox status`, and
  `inbox next` across GitHub and GitLab.
- Personal activity discovery: `activity commits`, `activity events`, and
  `activity summary`. These v1 personal commands ship GitHub only, with GitLab
  and local backends kept as explicit provider seams for later expansion.
- Repository/project activity discovery: `activity feed` across GitHub and
  GitLab. It normalizes commit and repository/project event surfaces while
  preserving provider-specific event vocabulary.
- PR/MR lifecycle: `create`, `view`, `list`, `edit`, `comment`, `review`,
  native `reviews` read, `ready`, `merge`, `close`.
- PR/MR checks: `pr checks` (one-shot snapshot) and `pr wait-checks`
  (blocking poll until terminal).
- Issue lifecycle: `issue create`, `view`, `edit`, `comment`, `close`,
  `reopen`.
- Repository label lifecycle: `label list`, `label audit`, and
  `label ensure`.
- Repository helpers: read-only `repo view` and the explicitly governed
  `repo push-default` delivery exception.
- Macro ops: `pr deliver` (kind = `feature` | `bug`), composing the
  atoms above into the agent-runtime-kit standard "open draft → wait CI →
  ready → merge → cleanup" flow.

Out of scope (v1): inbox mutations, release management, label deletion or
rename-by-default, raw REST
passthrough, issue macros, repo creation, branch protection management. `pr
review` posts an outcome comment by default — recording the supplied decision in
the comment without invoking provider-native approval / request-changes state —
and its opt-in `--submit-review` flag submits a native GitHub pull request review
event (`COMMENT` / `APPROVE` / `REQUEST_CHANGES`). With `--thread-file`, it can
also create resolvable GitHub review threads for actionable findings; this stays
GitHub-only because GitLab has no equivalent single review verb/thread mutation
contract in v1. Each remaining out-of-scope item would either widen the parity
gap (GitLab has no equivalent today) or remove the "lock down behaviour" value
(REST passthrough = same as `gh api` + rename).

## Provider parity model

`forge-cli` is a *router + wrapper*, not a client.

- Provider, authority, and repository location are resolved together from
  explicit `--provider` / `--host` / `--repo` inputs and the selected Git
  remote. Unknown or conflicting authorities fail closed as specified under
  "Provider detection"; v1 does not auto-fall-back to HTTPS-only
  Gitea/Forgejo even when the URL shape would permit it.
- All provider API calls go through the backend subprocess. The governed
  `repo push-default` operation additionally invokes local `git` for validation,
  one expected-old-OID compare-and-swap fast-forward, and remote-ref read-back. `forge-cli` does
  not open HTTP sockets, does not hold tokens, and does not write to
  the user's `~/.config/gh` or `~/.config/glab`.
- Backend stdout (typically `--json` from `gh`/`glab`) is parsed and
  re-rendered through the workspace's `cli_contract` envelope. Backend
  stderr is captured; on failure the relevant tail is included in
  `data.error.detail` with secrets redacted (see "Output contract"
  below).
- Backend invocations always force `--json <fields>` (gh) or
  equivalent JSON-bearing flags (glab) where supported. When a `glab`
  subcommand does not support `--json` in the installed version,
  `forge-cli` falls back to text parsing wrapped behind a typed parser
  module so that the brittle bit is isolated.

Parity matrix (v1):

| forge-cli op                                | github backend                                                                                                                               | gitlab backend                                                         | Parity                                                                                                                   |
| ------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| `pr create`                                 | `gh pr create --draft`; qualified user forks use `<user>:<branch>`                                                                           | `glab mr create --draft`                                               | exact for unqualified heads; qualified heads are a GitHub-only seam                                                      |
| `pr view <id>`                              | `gh pr view <id> --json …`                                                                                                                   | `glab mr view <id> -F json`                                            | exact                                                                                                                    |
| `pr list`                                   | `gh pr list --json …`                                                                                                                        | `glab mr list -F json`                                                 | exact                                                                                                                    |
| `pr edit <id>`                              | `gh pr edit <id> …`                                                                                                                          | `glab mr update <id> …`                                                | exact                                                                                                                    |
| `pr comment <id>`                           | `gh pr comment <id> --body …`                                                                                                                | `glab mr note <id> --message …`                                        | exact                                                                                                                    |
| `pr review <id>`                            | guard `gh api …/pulls/{id}`, then outcome comment, native review, or GraphQL pending review + review thread(s) + submit with `--thread-file` | `glab mr note create <id> --message … --resolvable=false`              | outcome comment, native review event, or resolvable review threads (`--thread-file`, GitHub-only); optional issue mirror |
| `pr review validate [id]`                   | local review body/thread-file validation; with `--check-diff`, `gh api repos/{repo}/pulls/{id}/files`                                        | local validation only; `--check-diff` unsupported in v1                | schema/privacy preflight plus optional GitHub diff-coordinate validation                                                 |
| `pr ready <id>`                             | `gh pr ready <id>`                                                                                                                           | `glab mr update <id> --ready`                                          | exact                                                                                                                    |
| `pr review-threads list <id>`               | `gh api graphql` (`reviewThreads` connection)                                                                                                | `glab api …/merge_requests/<iid>/discussions`                          | normalized thread state                                                                                                  |
| `pr review-threads resolve <id> --thread …` | `gh api graphql` (`addPullRequestReviewThreadReply` then `resolveReviewThread`)                                                              | unsupported in v1                                                      | GitHub-only seam                                                                                                         |
| `pr review-threads reply <id> --thread …`   | `gh api graphql` (`addPullRequestReviewThreadReply`)                                                                                         | unsupported in v1                                                      | GitHub-only seam                                                                                                         |
| `pr reviews <id>`                           | `gh api graphql` (native `reviews` connection plus `headRefOid`)                                                                             | unsupported in v1                                                      | GitHub-only normalized current-head/stale review snapshot                                                                |
| `pr pending-review inspect <id> --review …` | PR view + exact-node complete paginated body/inline-comment snapshot                                                                         | unsupported in v1                                                      | GitHub-only receipt-aware read with stable snapshot digest                                                               |
| `pr pending-review resume-submit <id> …`    | exact pending snapshot CAS + `submitPullRequestReview`, or submitted-review read-back                                                        | unsupported in v1                                                      | GitHub-only idempotent recovery for one receipt-bound transaction                                                        |
| `pr pending-review submit <id> …`           | exact pending snapshot CAS + `submitPullRequestReview`                                                                                       | unsupported in v1                                                      | GitHub-only guarded unmarked adoption that preserves inline content                                                      |
| `pr pending-review discard <id> …`          | exact pending snapshot CAS + `deletePullRequestReview`                                                                                       | unsupported in v1                                                      | GitHub-only destructive recovery with distinct inline-content-loss confirmation                                          |
| `pr pending-review delete <id> --review …`  | PR view + complete pending-only membership snapshot + exact-target final read with content/viewer/comment guards + `deletePullRequestReview` | unsupported in v1                                                      | GitHub-only compatibility recovery for one confirmed-abandoned body-only pending review                                  |
| `pr tasks <id>`                             | `gh pr view <id> --json number,url,body`                                                                                                     | `glab mr view <id> -F json` (`description`)                            | normalized task-list state                                                                                               |
| `pr merge <id>`                             | `gh pr merge <id> --squash --delete-branch`                                                                                                  | `glab api --method PUT .../merge` after gates                          | exact (method honoured per repo cfg)                                                                                     |
| `pr close <id>`                             | `gh pr close <id>`                                                                                                                           | `glab mr close <id>`                                                   | exact                                                                                                                    |
| `pr checks <id>`                            | `gh pr checks <id> --json …` plus `--required` for gating                                                                                    | `glab mr view -F json` + `glab api .../pipelines/<id>/jobs`            | emulated on GitLab                                                                                                       |
| `pr wait-checks <id>`                       | poll `gh pr checks` / `gh pr checks --required`; fall back to head-SHA REST checks on rollup permission errors                               | poll structured MR pipeline/jobs snapshot                              | emulated; same envelope                                                                                                  |
| `issue create`                              | `gh issue create …`                                                                                                                          | `glab issue create …`                                                  | exact                                                                                                                    |
| `issue view <id>`                           | `gh issue view <id> --json …`                                                                                                                | `glab issue view <id> -F json`                                         | exact                                                                                                                    |
| `issue edit <id>`                           | `gh issue edit <id> …`                                                                                                                       | `glab issue update <id> …`                                             | exact                                                                                                                    |
| `issue comment <id>`                        | `gh issue comment <id> --body …`                                                                                                             | `glab issue note <id> --message …`                                     | exact                                                                                                                    |
| `issue close <id>`                          | `gh issue close <id>`                                                                                                                        | `glab issue close <id>`                                                | exact                                                                                                                    |
| `issue reopen <id>`                         | `gh issue reopen <id>`                                                                                                                       | `glab issue reopen <id>`                                               | exact                                                                                                                    |
| `label list`                                | `gh label list --json …`                                                                                                                     | paged `glab label list --output json --per-page … --page …`            | exact                                                                                                                    |
| `label audit`                               | read labels, compare with caller catalog                                                                                                     | same                                                                   | exact                                                                                                                    |
| `label ensure`                              | `gh label create/edit`                                                                                                                       | `glab label create/edit`                                               | exact (no delete/rename by default)                                                                                      |
| `auth status`                               | `gh auth status --hostname <authority>`                                                                                                      | `glab auth status --hostname <authority>`                              | exact (text → typed)                                                                                                     |
| `repo view`                                 | `gh repo view <slug-or-host/slug> --json …`                                                                                                  | `glab repo view <slug-or-https-url> -F json`                           | exact                                                                                                                    |
| `repo push-default`                         | local Git validation/push plus host-qualified `gh repo view --json …` default-branch resolution                                              | local Git validation/push plus host-qualified `glab repo view -F json` | exact; no provider force path                                                                                            |
| `inbox list`                                | `gh search prs/issues --json …`                                                                                                              | `glab api --hostname <host> …`                                         | normalized aggregation                                                                                                   |
| `inbox status`                              | same provider reads as `inbox list`                                                                                                          | same provider reads as `inbox list`                                    | bounded counts                                                                                                           |
| `inbox next`                                | same provider reads as `inbox list`                                                                                                          | same provider reads as `inbox list`                                    | ranked bounded subset                                                                                                    |
| `activity commits`                          | `gh api search/commits`                                                                                                                      | unsupported in v1                                                      | GitHub-only seam                                                                                                         |
| `activity events`                           | `gh api users/<login>/events[/public]`                                                                                                       | unsupported in v1                                                      | GitHub-only seam                                                                                                         |
| `activity feed`                             | `gh api repos/<owner>/<repo>/commits` + repository activity REST                                                                             | `glab api projects/:id/repository/commits` + project events            | normalized open vocabulary                                                                                               |
| `activity summary`                          | `gh api graphql` contribution query                                                                                                          | unsupported in v1                                                      | GitHub-only seam                                                                                                         |
| `pr deliver`                                | macro: exact user/repository-aware lookup → `pr create` or adopt → `pr wait-checks` → `pr ready` → `pr merge`                                | branch lookup → same remaining composition                             | exact for unqualified heads; qualified user-fork lookup is a GitHub-only seam                                            |

"emulated" means the backend's native command differs in shape, but
`forge-cli` normalises both into the same `cli.forge-cli.pr.checks.v1`
payload so callers can branch on `data.state` regardless of host.

GitLab capability status:

- Supported with stable JSON/API: PR/MR create, view, list, edit, comment,
  ready, close, checks, wait-checks, merge, deliver; issue create/view/edit,
  comment, close, reopen; label list/audit/ensure; auth status; repo view and
  governed repo push-default;
  inbox list/status/next; activity feed.
- Intentionally unsupported in v1: GitLab `activity commits`,
  `activity events`, `activity summary`, and `search` operations.
- Fragile fallback: branch-only `pr checks <branch>` / `pr wait-checks
  <branch>` without a repo/project path uses `glab ci status -b <branch>` text
  parsing and keeps the `glab_version_unsupported` guard. Numeric MR lifecycle
  paths use structured MR pipeline/jobs API data and do not depend on the text
  parser minor range.

## Naming and topology

- Crate: `nils-forge-cli` (matches the `nils-*` package convention).
- Binary: `forge-cli`.
- Library entry: `crates/forge-cli/src/lib.rs`.
- Wrapper (Homebrew formula bin): `wrappers/forge-cli`.
- Completions: `completions/zsh/_forge-cli`, `completions/bash/forge-cli`.

Command tree:

```text
forge-cli
├── pr
│   ├── create
│   ├── view
│   ├── list
│   ├── edit
│   ├── comment
│   ├── review
│   │   └── validate
│   ├── review-threads
│   │   ├── list
│   │   ├── resolve
│   │   └── reply
│   ├── reviews
│   ├── pending-review
│   │   ├── inspect
│   │   ├── resume-submit
│   │   ├── submit
│   │   ├── discard
│   │   └── delete
│   ├── ready
│   ├── merge
│   ├── close
│   ├── checks
│   ├── wait-checks
│   └── deliver           (macro)
├── issue
│   ├── create
│   ├── view
│   ├── edit
│   ├── comment
│   ├── close
│   └── reopen
├── activity
│   ├── commits
│   ├── events
│   └── summary
├── label
│   ├── list
│   ├── audit
│   └── ensure
├── inbox
│   ├── status
│   ├── list
│   └── next
├── repo
│   ├── view
│   └── push-default
├── auth
│   └── status
└── completion              (workspace standard)
```

Global flags (every subcommand):

- `--format text|json` (workspace standard).
- `--remote <name>` (default `origin`).
- `--provider github|gitlab|local` (override auto-detect).
- `--host <hostname[:port]>` — select a canonical forge authority. The input
  is an authority only: schemes, userinfo, paths, query strings, fragments,
  whitespace, control characters, malformed DNS labels, and invalid ports are
  rejected. HTTPS port `443` is normalized away; other ports are retained.
  With `--provider`, a valid custom authority is accepted unless it is
  positively identified as the other provider. Without `--provider`, the host
  must identify a supported provider.
- `--repo <path>` (override the remote-derived repository path). Repository-
  scoped commands validate `owner/name` for GitHub,
  `group[/subgroup...]/project` for GitLab, and a non-empty local-compatible
  path for Local; invalid paths return `DATA 65` with
  `error.kind=repo_invalid`. Repository-independent commands such as `auth
  status` and personal `activity` commands do not reject an otherwise
  irrelevant repository shape.
- `--dry-run` — render the backend command that *would* run plus all
  validation checks, but do not invoke it. Output envelope carries the
  exact argv under `data.plan` for atomic commands or `data.actions[].plan`
  for `label ensure`. `repo push-default` instead runs its read-only local and
  provider preflight and emits `pushed=false` with the exact `push_refspec`;
  it never invokes `git push` in dry-run mode.

Every backend subprocess is bound to the resolved authority for that call.
`forge-cli` removes ambient `GH_HOST` and `GITLAB_HOST` from the child
environment, then sets the selected provider's variable (`GH_HOST` or
`GITLAB_HOST`) explicitly, so unrelated shell configuration cannot retarget a
request.

Inbox-local flags:

- `forge-cli inbox list|status --limit <n>` bounds each provider query family
  (default `30`).
- `forge-cli inbox next --limit <n>` bounds the returned ranked candidates
  (default `5`); provider reads still use at least `30` candidates.
- `--kind review|assigned|todo|authored|involved` is repeatable and selects
  inbox *reasons* (why an item appears). `involved` is opt-in because it can
  be broad on GitHub.
- `--item-type all|pr|issue` selects *result classes* (pull/merge requests vs
  issues) and is distinct from `--kind`. Default is `all`. PR-only mode skips
  GitHub issue searches and GitLab issue API calls; issue-only mode skips PR
  searches (GitHub review-requested is dropped). GitLab `todos` are kept when
  their `target_type` (or target URL) matches the selected item type;
  unclassifiable todos appear only in `all` mode.
- Global `--host` is rejected for `inbox list|status|next` with an actionable
  `provider_unsupported` error. Inbox has a separate multi-provider resolver;
  use inbox-local `--gitlab-host` for a non-default GitLab authority. GitHub
  Enterprise inbox search is unsupported in v1.
- `--gitlab-host <host>` is scoped to inbox commands only and is passed to
  every GitLab `glab api` invocation as `--hostname <host>`.
- When `--gitlab-host` is omitted, GitLab host resolution uses
  `FORGE_CLI_INBOX_GITLAB_HOST`, then GitLab remote inference, then
  `gitlab.com`.
- With no `--provider`, inbox queries both default providers. `--provider
  github|gitlab` narrows the inbox just as it narrows lifecycle commands, but
  inbox does not reuse the single-provider remote resolver internally.
- `--gitlab-vpn off|optional|required` controls an inbox-local GitLab
  readiness gate. `required` runs a readiness check before any GitLab backend
  call and reports `vpn_unavailable` without invoking `glab` when the check
  fails. `optional` may report a warning but still attempts GitLab. `off` is
  the default.
- `--gitlab-vpn-check tcp:<host>:<port>|cmd:<program>|openvpn` selects the
  readiness check. `tcp:` probes reachability, `cmd:` delegates to a local
  operator script, and `openvpn` verifies local OpenVPN CLI/profile
  prerequisites only; `forge-cli inbox` never starts or stops a VPN.
- `--gitlab-openvpn-profile <path>` is local-only input for probes. Profile
  paths are redacted from dry-run JSON, warnings, provider errors, cache files,
  issue records, and docs examples.
- `--gitlab-vpn-check-timeout <duration>` bounds readiness checks (default
  `2s`). `--provider-timeout <duration>` bounds GitLab inbox identity/API
  backend calls (default `20s`; `0s` disables). GitHub queries remain
  independent from GitLab timeout behavior.
- `--strict-providers` makes partial provider failure a non-zero
  `provider_failed` failure envelope with `error.details.providers[]`.
- `--cache-fallback` includes recent stale cached provider items when a
  selected provider is VPN-unavailable or times out. Cache fallback is opt-in,
  age-bounded by `--cache-max-age` (default `30m`), and disabled with
  `--no-cache`.

Activity-local flags:

- `forge-cli activity commits --user <login|@me> --since <date-or-datetime>
  --limit <n>` searches recent commits authored by the selected GitHub user.
- `forge-cli activity events --user <login|@me> --public-only --limit <n>`
  lists recent user events. Without `--public-only`, `@me` can use the
  authenticated user's event endpoint.
- `forge-cli --repo <owner/name|group/project> activity feed
  --since <date-or-datetime> --limit <n>` lists recent repository/project
  activity. GitHub reads commits plus repository activity; GitLab reads
  commits plus project events.
- `forge-cli activity summary --user <login|@me> --since <date-or-datetime>
  --limit <n>` summarizes commit contributions by repository.
- Activity limits are effective provider page sizes and are clamped to the v1
  per-page maximum of `100`.
- `@me` resolves through `gh api user`; `--dry-run` includes both the identity
  lookup and the data query under `data.plans[]`.

Label-local flags:

- `forge-cli label list --limit <n>` reads at most `<n>` provider labels and
  emits `cli.forge-cli.label.list.v1`. The limit is a provider-neutral total;
  GitLab advances `--page` with a maximum `--per-page 100` until the total is
  reached or a short page proves the catalog is exhausted.
- `forge-cli label audit --catalog <path> --limit <n>` compares provider
  labels to the caller-owned catalog and reports missing labels, color /
  description drift, and unknown shared labels.
- `forge-cli label ensure --catalog <path> [--update-existing]` creates
  missing labels and updates existing color / description drift only when
  explicitly requested. It never deletes or renames labels by default.
- `pr create` and `pr deliver` accept `--label <name>` repeatedly. Catalog
  validation is opt-in via `--label-catalog <path> --strict-labels`.

## Atomic op surface

The full machine-readable list lives in
[`forge-cli-ops-v1.yaml`](forge-cli-ops-v1.yaml). The Markdown table
below is the human-readable summary; the YAML is authoritative for
backend mapping, validation rules, and output schema versions.

### `pr create`

- Input: `--head <branch>` (default current branch; GitHub also accepts
  `<user>:<branch>` for a cross-fork head), `--base <branch>`
  (default repo default branch), `--title <str>`, `--body-file <path>`
  or `--body <str>`, `--kind feature|bug`, `--draft` (default `true`),
  `--reviewer <user>...`, `--label <name>...`,
  `--label-catalog <path>`, `--strict-labels`.
- Validation (see "Lock-down policy" for the full list):
  - the branch, or the branch suffix of a qualified GitHub head, MUST match
    the semantic branch-name rule and align with `--kind`;
  - a qualified GitHub user MUST own the local branch's single upstream push
    repository on the selected GitHub authority; the full `<user>:<branch>`
    ref is preserved for provider dispatch, while local branch checks consume
    only the suffix;
  - immediately after creation, GitHub's `headRefName`, `headRefOid`, and
    `headRepository.nameWithOwner` MUST match the local branch, local commit,
    and upstream repository before success is reported;
  - title length ≤ 70 chars, no trailing whitespace;
  - body MUST contain non-empty `## Summary` and `## Test plan`
    sections;
  - working tree MUST be clean (`git status --porcelain` empty);
  - resolved local head branch MUST be pushed and remote-tracked.
- Output schema: `cli.forge-cli.pr.create.v1`,
  `data = { number, url, head, head_sha?, head_repository?, base, draft,
  title, kind, provider }`.

Qualified heads are deliberately a GitHub-only user-fork seam. GitHub CLI's
`gh pr create --head` contract accepts `<user>:<branch>` but does not support
an organization as the qualifier. `forge-cli` therefore does not advertise
`<owner>:<branch>` or organization-fork support; provider rejection remains
authoritative for account-type distinctions that cannot be proven offline.

### `repo push-default`

- Input: `--head <ref>` (default `HEAD`, and must resolve to the checked-out
  commit), required `--expected-base <full-sha>`, and required
  `--reason-file <path>` naming a regular file of at most 2,000 bytes that
  contains the caller's explicit authorization basis.
- Validation:
  - the selected Git remote must expose exactly one actual push URL (including
    any configured `remote.<name>.pushurl`). Before provider metadata is read,
    the push destination must classify to the selected provider (unless an
    explicit provider plus custom host selected an otherwise unclassified
    authority), and its canonical authority must exactly equal the selected
    authority. The provider metadata URL must then match that same authority
    and repository identity. Canonical comparison maps provider transport
    aliases to their API authority, normalizes HTTPS port `443`, and preserves
    every non-default HTTPS port. Metadata from one authority cannot authorize
    a push to another; the local backend is unsupported;
  - HTTP(S) push URLs containing userinfo are rejected; callers use credential
    helpers rather than embedding credential material in a subprocess argument;
  - after the push URL is expanded once, no effective `url.*.insteadOf` or
    `url.*.pushInsteadOf` rule may match it again; empty rewrite prefixes are
    universal matches and are rejected. This prevents Git from retargeting the
    mutation independently of base/read-back operations;
  - the worktree is clean and checked out on a non-default branch;
  - the remote default branch still equals `--expected-base`;
  - `HEAD` is exactly one commit ahead of that base and the base is its
    ancestor;
  - `git log --format=%G?` reports `G` for the delivered commit.
- Mutation: exactly one command-scoped `git -c push.followTags=false -c
  push.pushOption= -c push.recurseSubmodules=no push --porcelain --no-follow-tags
  --no-recurse-submodules --no-push-option
  --force-with-lease=refs/heads/<default>:<expected-base> --
  <validated-push-url> <head-sha>:refs/heads/<default>` invocation. The exact
  old-OID lease is used only as a compare-and-swap guard; the independent
  ancestry proof keeps the update fast-forward-only. There is no unconstrained
  force, delete, retry, or direct-merge option, and inherited tag, submodule,
  and push-option expansion is disabled. Any concurrent remote change is
  returned as `default_push_rejected`.
- Destination pinning: the expected-base read, push, and post-push read-back all
  use the same validated URL rather than re-resolving the remote name.
- Resource bounds: every Git subprocess has a 120-second timeout and an 8 MiB
  per-stream capture limit. Timeout or output-limit failure kills the complete
  child process group and returns `git_timeout` or `git_output_limit`
  without retrying a push. The provider metadata subprocess uses the same
  finite deadline and backend capture/process-group contract. Shipped release
  builds always execute `git` with these hard bounds; executable and bound
  overrides exist only in debug test builds.
- Read-back: exact `git ls-remote` after the push must equal the delivered SHA;
  otherwise the op fails closed with `default_push_verification_failed` and
  tells the caller to inspect the already-mutated remote.
- Output schema: `cli.forge-cli.repo.push-default.v1`; the receipt contains the
  repository, remote/default branch, authoring branch, head/base SHAs, reason,
  exact refspec, `pushed`, and `observed_remote_sha`.
- Typed failures: `push_destination_missing`,
  `push_destination_ambiguous`, `push_destination_credentials_unsupported`,
  `push_destination_rewrite_ambiguous`,
  `provider_mismatch`, `provider_unsupported`, `repository_mismatch`,
  `dirty_worktree`, `detached_head`, `default_branch_checkout`,
  `head_not_checked_out`, `expected_base_mismatch`,
  `expected_base_missing`, `expected_base_not_ancestor`,
  `direct_commit_count_invalid`, `commit_signature_unverified`,
  `reason_file_unreadable`, `reason_invalid`, `local_path_present`,
  `object_id_invalid`, `remote_default_lookup_failed`,
  `remote_default_branch_missing`, `git_timeout`, `git_output_limit`,
  `backend_timeout`, `backend_output_limit`,
  `default_push_rejected`, `default_push_verification_failed`, and
  `software_error`. Callers must preserve their declared DATA, RUNTIME,
  UNAVAILABLE, SOFTWARE, or USAGE exit class.

### `pr wait-checks`

- Input: `<id>` (or `--head <branch>` to resolve PR by branch),
  `--timeout <duration>` (default `30m`), `--interval <duration>`
  (default `20s`), `--required-only` (default `true`).
- Behaviour: polls the backend until every required check is in a
  terminal state (`success`, `failure`, `cancelled`, `skipped`,
  `neutral`). `--required-only=true` ignores non-required checks for
  the gating decision but still reports them in `data.checks`. On
  GitHub, required-check classification comes from an explicit
  `gh pr checks --required` call; the JSON field set is
  `name,state,bucket,workflow,link,startedAt,completedAt,description`
  so the backend stays compatible with `gh 2.92.0`. If `gh pr checks`
  fails on a `statusCheckRollup` permission traversal, the GitHub path
  reads `headRefOid` alone and falls back to REST `gh api` commit
  check-runs plus combined status contexts for the same head SHA. If a
  REST check-runs or combined status
  response is truncated, the fallback adds a pending synthetic row instead of
  reporting a clean gate from an incomplete page. When `--required-only=true`, those
  fallbacks cannot recover GitHub's required classification, so they
  fail-closed gate every readable fallback row and synthesize a pending
  required row when the fallback snapshot is empty. They also add
  `github_status_rollup_requiredness_unknown_all_rows_gated` to
  `data.warnings[]`. On GitLab,
  numeric MR ids use `glab mr view -F json` for the MR head pipeline and
  `glab api --hostname <host> projects/<project>/pipelines/<id>/jobs`
  for job rows; `allow_failure=true` jobs remain visible in
  `data.checks` but are not required. Branch-only GitLab snapshots
  without project context fall back to the version-pinned
  `glab ci status -b <branch>` text parser.
- Terminal states map to envelope `ok`:
  - all required `success`, with at least one check reported → `ok = true`.
  - any required `failure`/`cancelled`/`timed_out` → `ok = false`,
    exit `RUNTIME 1`, `error.kind = "checks_failed"`.
  - timeout reached with checks still running → `ok = false`, exit
    `UNAVAILABLE 69`, `error.kind = "checks_timeout"`.
  - timeout reached having never seen a required check or a visible row →
    `ok = false`, exit `DATA 65`, `error.kind = "checks_not_registered"`.
    An empty snapshot is **not** terminal (rule 8): the poll continues
    through the provider's check-registration window rather than reading
    "nothing is failing" as "everything passed". `--allow-no-checks` makes
    the empty set terminal for a repository that configures none.
- Output schema: `cli.forge-cli.pr.checks.v1`,
  `data = { state, required_count, success_count, failed:[…], pending:[…], checks:[…], duration_ms, warnings? }`.

### `pr review-threads` (read)

- The `pr review-threads list <id>` read surface emits
  `cli.forge-cli.pr.review-threads.v1`. Each thread now carries an
  `id` handle, complete ordered comment stream, and normalized diff anchor in
  addition to its state fields:
  `data.threads[] = { id, resolved, outdated, author, path, diff_side,
  line, original_line, original_start_line, start_diff_side, start_line,
  subject_type, created_at, url, body, comments }`. On GitHub `id` is the
  `reviewThreads` node id
  (`PRRT_...`) — the single handle consumed by both write ops below
  (as `threadId` for resolve and `pullRequestReviewThreadId` for
  reply). On GitLab `id` is the discussion id. The field is additive;
  existing consumers are unaffected.
- Text output includes the same thread id on each thread line so terminal users
  can copy the `--thread` value without switching to JSON.
- GitHub thread and per-thread comment connections are independently paginated.
  The PR head and thread identity must remain stable across the complete read;
  partial GraphQL results, missing required anchor/state fields, cursor loops,
  or head drift fail closed as `review_snapshot_incomplete`.
- `--dry-run` emits the planned thread-list backend call without running the
  preliminary PR/MR view lookup or touching the provider network. It resolves
  the repo/project from `--repo` or the configured remote URL.

### `pr reviews` (read)

- `pr reviews <id>` emits `cli.forge-cli.pr.reviews.v1` and is GitHub-only in
  v1. GitLab and local return `provider_unsupported` (`USAGE 64`) rather than
  claiming an empty snapshot.
- The operation reads the PR's `headRefOid` and all pages of native review
  objects (100 nodes per page, with a 100-page safety limit), then
  returns `data = { provider, number, url, head_sha, current_head_reviews,
  stale_reviews, pending_reviews }`. Submitted reviews include `id`,
  `database_id`, `url`, `author`, native `state`, `commit_sha`, `submitted_at`,
  `summary`, and `summary_truncated`. Provider-valid `PENDING` reviews have no
  `submitted_at`; they are returned separately under `pending_reviews` and
  never participate in submitted-review convergence.
- `summary` is evidence only and is bounded to 4096 UTF-8 bytes. The operation
  does not parse natural-language review prose to derive a verdict. Reviews
  whose `commit_sha` differs from `head_sha` remain visible under
  `stale_reviews` but do not satisfy current-head policy.
- `--dry-run` emits the planned GraphQL call. Repository resolution uses
  `--repo owner/name` or a recognized forge remote.
- GraphQL partial errors, missing required review/page fields, unknown native
  states, cursor loops, a head change between pages, or the page safety limit
  return `review_snapshot_incomplete` (`DATA 65`). The gate never treats a
  partial native-review snapshot as empty.

### Review transaction state and pending-review recovery

- Threaded native reviews use the provider-visible, append-only
  `forge-cli.review-loop.v1` state chain. A record requires `schema`,
  `repository`, `pr`, `expected_head`, zero-based contiguous `generation`,
  nullable `previous_digest`, typed `payload`, and `record_digest`. A
  `review-run-receipt` payload requires `review_run_id`, portable
  `route_lenses`, `decision`, `expected_head`, `round`, `summary_digest`, and an
  ordered `inline_manifest[]` of `index`, `path`, optional line/range anchors,
  side, subject type, and normalized `body_digest`.
- Digests are lowercase SHA-256 values prefixed by `sha256:`. Their preimages
  are compact UTF-8 JSON in the declared field order. `review_run_id` binds
  repository, PR number, expected head, round, semantic route lenses, decision,
  normalized summary digest, and ordered inline manifest. A record digest also
  binds the prior record digest. Byte-identical append retries are deduplicated;
  conflicting duplicates, forks, missing/unreachable generations, target
  mismatches, malformed markers, or digest mismatches fail as
  `review_state_conflict` before native-review mutation. Encoded state markers
  are limited to 64 KiB before provider mutation, and the complete rendered
  comment body — visible text plus marker — is limited to 64 KiB before provider
  mutation. An oversized marker returns `review_state_record_too_large`; an
  oversized complete body returns `review_state_comment_too_large`.
- Provider marker grammar is versioned and deterministic:
  `<!-- forge-cli:review-state:v1 <lowercase-hex-record-json> -->`,
  `<!-- forge-cli:review-run:v1 run=<review_run_id> -->`, and
  `<!-- forge-cli:review-finding:v1 run=<review_run_id> digest=<body_digest> -->`.
  Owned markers remain in raw provider content but are removed before semantic
  body comparison and digesting. State reads retain comments authored by the
  authenticated viewer or a provider-classified `OWNER`, `MEMBER`, or
  `COLLABORATOR`. For the self-author comparison only, a terminal GitHub App
  `[bot]` suffix on the authenticated viewer is canonicalized to the comment
  author form; no other login normalization occurs. Marker-shaped comments
  from unprivileged actors cannot extend or poison the transaction chain. This
  lets a later authorized session resume the same provider-visible chain
  without a machine-local ledger.
- Record encoding and comment presentation are separate. Every state comment the
  CLI posts leads with the tool-neutral notice
  `Review checkpoint — review progress recorded.`, followed by the unchanged
  marker on its own line.
  GitHub hides HTML comments when it renders Markdown, so a marker-only body
  appears in the timeline as a blank comment under the operator's identity; the
  visible notice prevents a confusing blank entry without exposing forge-cli's
  generation, payload-kind, or head bookkeeping to ordinary readers. A combined
  delivery outcome is appended *after* both, separated by a
  `---` rule. That ordering is load-bearing, not cosmetic: a Markdown HTML block
  opened in caller text runs until a line containing `-->`, so caller text placed
  above the notice could swallow both the notice and the marker.
- Presentation text never enters a record digest, the chain order, or the parser,
  so historical bare-marker comments and new rendered comments validate as one
  chain with no migration and no edit or deletion of existing comments. The
  chain therefore deduplicates by *record*, not by comment body: two concurrent
  sessions that compute the identical transition converge on one record even if
  their comments differ, so a divergent concurrent outcome is possible and is
  detected only for the writing session (see `review_outcome_not_posted` below).
- The visible notice is constant plain Markdown. Record-specific details stay in
  the hidden marker and never enter the human-facing timeline label.
- Receipt fields intentionally exclude authentication tokens, credentials,
  environment-variable values, local paths, and private identity/profile names.
  The durable identity route contains only portable lens names and the semantic
  decision; environment-owned routing resolves the actor outside the receipt.
- `pr pending-review inspect <id> --review <PRR_...>` emits
  `cli.forge-cli.pr.pending-review.inspect.v1`. Its complete snapshot requires
  PR/head/review identity, author, commit, raw and semantic body,
  `viewerDidAuthor`, `viewerCanDelete`, provenance, every inline comment with
  normalized body digest plus `diffSide`/`startDiffSide` and line/range anchors,
  and `snapshot_digest`. Every page must repeat identical review metadata and a
  stable `totalCount`; partial data, cursor loops, count mismatch, head drift,
  an aggregate above 1,000 comments, or a decoded retained snapshot above 4 MiB
  returns `review_snapshot_incomplete` instead of a partial snapshot. An
  excessive `totalCount` is rejected before another page. Provenance is
  `receipt-bound` or `unmarked`; mixed, missing,
  or digest-mismatched owned markers return `pending_review_manifest_mismatch`.
- `pr pending-review resume-submit <id> --review <PRR_...> --review-run-id
  <digest> --expected-head <sha> --expected-commit <sha> --expected-snapshot
  <digest> --decision <decision>` emits
  `cli.forge-cli.pr.pending-review.resume-submit.v2`. Version 2 makes
  `snapshot_digest` nullable and adds `snapshot_provenance` so idempotent
  recovery never claims a pending snapshot it cannot prove. It submits only an exact
  viewer-owned receipt-bound snapshot after recomputing the run id and matching
  the summary plus every immutable inline-manifest field. If that review was
  already submitted, the command reads submitted reviews and threads, verifies
  the authenticated author, same run id, review id, head, commit, decision
  state, and complete receipt-bound manifest, and returns the same successful
  envelope without another mutation. Because a submitted review cannot prove
  the caller's former pending-snapshot digest, that idempotent envelope reports
  `snapshot_digest: null` with `snapshot_provenance:
  pending-snapshot-unverified`; a live submit reports its guarded digest with
  `snapshot_provenance: pending-cas+submitted-reconciled`. Provider-null
  side/range values on a `FILE` comment are normalized away when matching a
  receipt; `LINE` and range anchors remain exact.
- `pr pending-review submit <id> --review <PRR_...> --expected-head <sha>
  --expected-commit <sha> --expected-snapshot <digest> --decision <decision>
  --confirm-unmarked-submit` emits `cli.forge-cli.pr.pending-review.submit.v1`.
  Its released v1 `snapshot_digest` remains a required string. It accepts only
  `unmarked` provenance and preserves the exact body and all inline comments
  while submitting. It never auto-adopts a unmarked draft.
- `pr pending-review discard <id> --review <PRR_...> --expected-head <sha>
  --expected-commit <sha> --expected-snapshot <digest> --confirm-discard`
  emits `cli.forge-cli.pr.pending-review.discard.v1`. It is destructive and
  requires the additional `--confirm-inline-content-loss` whenever the complete
  snapshot contains an inline comment. No normal `pr review` recovery path
  invokes discard or delete.
- Live inspect data is required for these four commands, so their current v1
  dry-run form fails with `pending_review_snapshot_required`. All mutating forms
  perform the exact head/commit/snapshot and viewer-identity checks under a
  private cross-process forge-cli lease keyed by provider, host, repository,
  PR, and authenticated viewer. Submission and deletion are immediately read
  back and succeed only when the exact submitted manifest or absence is
  reconciled; otherwise they return `pending_review_reconciliation_failed`.
  A busy or unsafe lease returns `pending_review_lease_busy` or
  `pending_review_lease_unsafe`. GitHub exposes no compare-and-swap token on
  review submission, so the lease closes races among cooperating forge-cli
  processes but cannot exclude a same-identity non-cooperating API client in
  the irreducible interval between the final read and mutation. Mismatches return `pending_review_head_changed`,
  `pending_review_commit_mismatch`, `pending_review_manifest_mismatch`,
  `pending_review_identity_mismatch`, `pending_review_pr_mismatch`, or
  `pending_review_not_found` with no mutation.

### `pr review-loop inspect` / `observe` / `extend` / `validate`

- These GitHub-only commands make the repair/re-review loop resumable from the
  append-only `forge-cli.review-loop.v1` chain. `inspect <id>` validates the
  complete chain and emits its typed latest state. `observe <id>
  --expected-head <sha> --findings-file <path> [--expected-state <digest>]
  [--body <text> | --body-file <path>]`
  accepts either a delivery-mode `review-specialists merge` envelope or a
  finding-observation array, evaluates one deterministic transition, and uses
  both the exact PR head and state tip as compare-and-swap inputs.
- `observe --body <text>` / `--body-file <path>` (`-` reads stdin; mutually
  exclusive) posts a human-readable delivery outcome in the SAME provider comment
  as the appended ledger record, replacing a separate final outcome comment.
  Every rule on the body runs before the first provider call, so a dry run
  returns the same verdict a live run would and a rejected body costs no provider
  round trip. The body must be non-empty, within the comment-body limit
  (`review_state_comment_too_large`), free of any HTML comment
  (`review_state_comment_invalid` — a review-state marker of its own would shadow
  this record because the parser takes the first marker in a body, and an
  unterminated `<!--` would hide the visible label), and held to the same
  portability rules as a review comment (`local_path_present`,
  `markdown_escaped_control`). Those portability rules are the shared
  provider-payload guard, which is pattern-based and allowlisted; the outcome body
  is operator-attested text, not a machine-derived receipt field, so the stronger
  "review state excludes local paths" clause above is not a guarantee about it.
  `--body-file -` consumes stdin once, so a dry-run-then-live sequence must not
  share a piped body.
- Because the outcome rides the append, it inherits the append's idempotency: an
  unchanged observation appends nothing, so a supplied outcome is either already
  present on the pull request from the earlier attempt — reported as
  `outcome_posted: true` with `appended: false` — or would be silently dropped,
  which fails closed as `review_outcome_not_posted` instead. An identical retry
  therefore cannot create a duplicate outcome or ledger comment, and cannot lose
  one either. `outcome_posted` is never inferred from the flag: it is set only
  after a post-write read-back confirms a privileged comment carrying both this
  record's marker and this outcome, and a durable record whose outcome is not
  visible fails as `review_outcome_not_posted`. A durable hard-stop receipt is
  always appended without an outcome body.
- Applicability is narrow and deliberate. The combined form fits a caller whose
  delivery outcome is decided once and never revised, because a ledger record is
  immutable and an append is conditional: an unchanged observation has no append
  to carry a revised outcome. It does NOT fit a workflow whose outcome must be
  posted after repairs and refreshed on retry — there the outcome is a mutable
  artifact and the ledger is not, so welding them either fails closed, appends a
  semantically empty generation per refresh, or leaves a stale outcome beside a
  newer one. `agent-runtime-kit`'s delivery skills are in that second category:
  their posting-order contract requires the disposition to post last and to be
  refreshed on merge-convergence retries, so they keep the outcome as its own
  comment by design. They still benefit from the visible notice above,
  which is what stops a ledger comment from rendering blank. Do not adopt the
  combined form in a workflow without first checking that its outcome is
  single-shot.
- Observation-array rows may include `status` or `disposition` with `open`,
  `fixed`, `accepted`, `preference`, or `follow-up`. Terminal dispositions are
  durable and non-blocking. Omitting an open finding or changing its blocking
  bit on the same head is not a disposition and fails closed.
- The default budget is five repair rounds, two consecutive no-progress rounds,
  and zero automatic reopens for a previously fixed lifecycle fingerprint.
  Same-head/same-findings retries append nothing. Only a new reviewed head
  advances a round, and only a strict reduction in open-finding cardinality is
  mechanical progress. Stable-fingerprint collisions, reopens, no progress,
  and the round limit fail closed as typed terminal errors. An extendable error
  first appends a durable hard-stop receipt, so a restart returns the same stop.
- A durable terminal budget error includes an exact extension proposal, but
  never extends itself. `extend` is a separate action requiring the stopped state tip,
  expected head, proposal digest, field/increment, stop code, and a newer
  comment on the same PR from an `OWNER`, `MEMBER`, or `COLLABORATOR` carrying
  the exact approval marker. The stop code must map to its canonical budget
  field and the proposal must match the active receipt. Each proposal is
  consumable once; retry and delivery paths cannot silently increase a budget.
- An observation can only be appended at the current provider head, so history
  cannot be backfilled, and a `fixed` disposition requires a repaired head.
  Together those force the observation to precede the repair push: observe the
  reviewed head as `open`, push the repair, observe the repaired head as
  `fixed`, then merge at that head. Doing every repair first and then trying to
  record the history is unrecoverable, because the pre-repair head is gone and
  the round count cannot be reconstructed.
- The three provider-backed commands emit their own
  `cli.forge-cli.pr.review-loop.*.v1` schema with `data.appended` and
  `data.outcome_posted`. Only `observe` can set `outcome_posted`, and never
  without `appended`. `inspect` and `extend` dry-runs are offline and report the
  command-specific read/transition plan.
- `validate --findings-file <path>` is the offline payload check, mirroring
  `pr review validate`. It emits `cli.forge-cli.pr.review-loop.validate.v1` with
  `data = { shape, row_count, identity_count, dispositions[], blocking_count,
  duplicate_identities? }` and carries neither `appended` nor `outcome_posted`,
  because it never appends. It takes no id, resolves no provider context and
  runs no backend, so it is provider-independent and `--provider`, `--repo`,
  `--host` and `--dry-run` have no effect on it.

  Its accept/reject decision is not a second implementation of the payload
  rules: it calls the same canonicalization `observe` calls, so lifecycle
  fingerprint form, identity collisions (`review_fingerprint_collision`) and
  blocking normalization for terminal dispositions are decided by one piece of
  code. Head and state-tip compare-and-swap, the transition against stored
  state, and the rendered comment need the provider and stay with
  `observe --dry-run`. Failing `validate` therefore proves an append cannot
  succeed; passing means the payload itself is acceptable.

  Accepted dispositions are `open`, `fixed`, `accepted`, `preference` and
  `follow-up`. A finding that reappears is submitted as `open`; the state
  machine decides whether that is a reopen, and `reopened` is not an input.
- `observe --dry-run` is a faithful non-mutating preflight. It reads and
  validates the findings payload and any outcome body, resolves the pull request,
  performs the head
  and state-tip compare-and-swap comparisons, evaluates the transition, and
  renders the exact comment the live append would post, then reports each
  verdict in `data.preflight[]` — the same element shape as
  `pr deliver --dry-run`'s `local_preflight[]`, under a different name because
  these rules include provider reads. `data.preflight_ok` is the conjunction and
  `data.would_append` reports whether the real run would append a new
  generation — true for an accepted state-changing transition, and also for an
  extendable budget error, which appends a durable hard-stop receipt before
  failing, so predicting only "this fails" would understate it. When an
  unextended durable hard stop already exists the live path returns that stop
  *without* appending, and the dry run mirrors that: `would_append` is false and
  no comment is planned.
  `data.planned_comment` reports the planned write as
  `{visible_metadata, includes_outcome_body, bytes}`, where the established
  `visible_metadata` field now carries the tool-neutral notice and `bytes` is the
  complete rendered body the size limit binds. It is present only when a write is
  planned, and the `state_comment_body` verdict is likewise reported only then: an
  already-current chain writes nothing, so there is no body to check and no
  outcome to post. The sweep does
  not short-circuit, so the local payload verdict is
  reported even when the provider is unreachable; that is the supported way to
  check a findings file without writing durable provider-visible state, which a
  live `observe` does on success.

### `pr pending-review delete` compatibility surface

- `pr pending-review delete <id> --review <PRR_...> --expected-head <sha>
  --expected-commit <sha> (--expected-body <text> | --expected-body-file <path>)
  --confirm-abandoned` emits
  `cli.forge-cli.pr.pending-review.delete.v1` and is a GitHub-only recovery
  primitive. GitLab and local return `provider_unsupported` (`USAGE 64`).
- Before mutation it fetches the named PR, reads a complete paginated
  pending-only membership snapshot while retaining only the named review body,
  and requires `--review` to name an entry whose PR head, review commit, and
  normalized body exactly match the required expected values. The entry must
  also have provider-native
  `viewerDidAuthor: true` and `viewerCanDelete: true`. A missing or
  already-submitted node returns `pending_review_not_found`; ownership and
  delete-capability failures return `pending_review_author_mismatch` and
  `pending_review_not_deletable`; content drift returns the corresponding
  `pending_review_*_mismatch`. Submitted-review parsing is independent and
  cannot block this recovery path.
- After the membership checks, the command takes the private cross-process
  viewer/repository/PR lease and reads the exact review node again immediately
  before deletion. It revalidates PR membership, head, commit,
  body, ownership, and delete capability. `pending_review_pr_mismatch` rejects
  a moved or inconsistent target. Reviews with inline draft comments return
  `pending_review_inline_comments_present` and require manual provider recovery;
  this primitive deletes only confirmed abandoned body-only drafts.
- Only the revalidated node id is passed to `deletePullRequestReview`, the
  returned id must match, and an exact-node read must reconcile provider absence
  before success. The success payload is
  `data = { provider, number, url, head_sha, commit_sha, review_id, review_url,
  author, deleted }`. GitHub does not expose content compare-and-swap on this
  deletion mutation. The lease excludes cooperating forge-cli processes, while
  explicit abandonment confirmation, immutable content guards, reconciliation,
  and the inline-comment refusal bound the irreducible same-identity
  non-cooperating-client window between the exact final read and delete.
- `--dry-run` is offline. It validates a named `--expected-body-file`, renders
  the PR guard, complete snapshot, exact-target read, and delete plans, and
  never emits the expected body or its file path.
- Expected and provider review bodies are limited to 64 KiB. Inline values and
  named files are checked before provider access; file and live stdin inputs use
  bounded reads. Oversized expected input returns
  `pending_review_body_too_large`, while an oversized provider body fails the
  snapshot closed as `review_snapshot_incomplete`.

### `pr review`

- The `pr review <id>` posting surface emits
  `cli.forge-cli.pr.review.v1`. It accepts an already-rendered review outcome
  body via `--comment <text>` or `--comment-file <path>`, plus a
  `--decision comments-only|approve|request-changes` metadata value and
  repeatable `--lens <name>` entries.
- By default `--decision` is recorded in the envelope and generated issue mirror
  body only (the outcome-comment form); it does not call provider-native
  approve/request-changes APIs.
- With `--submit-review --expected-head <sha>` (GitHub-only in v1) the command
  instead submits a native
  pull request review event: it POSTs `gh api repos/{repo}/pulls/{id}/reviews`
  with `--decision` mapped to the review `event` and the reviewed head bound as
  `commit_id`
  (`comments-only→COMMENT`, `approve→APPROVE`, `request-changes→REQUEST_CHANGES`),
  creating the `#pullrequestreview-` object reported as `data.pr_comment_url`
  with `data.submitted_review = true`. The review is authored by whatever
  identity the inherited `gh` token carries, so a reviewer-bot token (for example
  via `FORGE_BOT_PROFILE`) yields a bot-authored review. A body is required for
  `COMMENT` and `REQUEST_CHANGES` and optional for `APPROVE` (a body-less approve
  omits the `body` field). The same PR-existence guard runs first. For a
  summary-only native review it is followed by a complete pending-only review
  snapshot; threaded reviews use the receipt transaction below. The provider
  head must still equal
  `--expected-head`; drift returns `github_review_head_changed` (`DATA 65`)
  before any mutation. The successful payload exposes the bound head as
  `data.head_sha`. If the authenticated viewer already
  owns a pending review during a summary-only submission, the command returns
  `github_pending_review_exists` (`RUNTIME 1`) before any review mutation. Its
  detail includes the provider head, viewer-owned pending count, and deletable
  count so callers can inspect the exact node. Pending reviews owned by other
  viewers do not block submission. The pending guard and reviews POST are
  rendered in `--dry-run` as
  `data.pending_review_guard_plan` and `data.plan`. Omitting the expected head
  returns `expected_review_head_required` (`DATA 65`); supplying it without
  `--submit-review` returns `expected_review_head_requires_submit_review`
  (`DATA 65`). `--submit-review` on
  GitLab / Local returns `provider_unsupported` (`USAGE 64`).
  If GitHub rejects the native review submission with HTTP 422, the command
  returns `github_native_review_rejected` (`RUNTIME 1`) and preserves the raw
  backend detail plus retry guidance; this covers identities that can comment
  but are not eligible to submit an approval review, such as some GitHub App bot
  identities.
- With `--thread-file <path>` (GitHub-only in v1), the command creates
  resolvable review threads for actionable findings. The file must be a JSON
  array of entries shaped like
  `{ "path": "src/lib.rs", "line": 42, "side": "RIGHT", "body": "..." }`.
  `side` defaults to `RIGHT`; `startLine` / `startSide` are accepted for ranged
  comments; omitting `line` creates a file-level thread (`subjectType=FILE`),
  while line comments use `subjectType=LINE`. The thread file is capped at
  256 KiB, 50 specs, 1024-byte paths, and 16 KiB bodies; put lower-priority
  findings in the summary body or split them into a later review. `--thread-file`
  requires `--submit-review`; omit it for a summary-only review. A live GitHub
  run first reads a lightweight, fully paginated root-comment fingerprint of
  review threads, skips findings
  whose semantic `(path, body)` already has a live non-resolved/non-outdated
  thread, computes a deterministic `review_run_id`, and appends an immutable
  `forge-cli.review-loop.v1` receipt before native-review mutation. It then
  takes the same private provider/host/repository/PR/viewer cross-process lease
  used by pending-review recovery and selects only an exact viewer-owned
  receipt-bound pending review, or creates a
  new review bound to `--expected-head` through `commitOID`. The review body and
  every finding carry owned run/digest markers. An exact pending manifest must
  be an ordered prefix of the receipt manifest; the command adds only the
  missing suffix and performs a final complete snapshot before
  `submitPullRequestReview`. Success requires an immediate exact-node read-back
  whose submitted state and complete receipt-bound manifest reconcile.

  Every interrupted stage is resumable: a lost create response, any lost inline
  comment response, a pre-submit failure, and a lost submit response preserve
  completed content. Re-running the same immutable inputs resumes or returns
  the authenticated viewer's already-submitted review by `review_run_id`;
  submitted-review history is scanned only when the exact immutable receipt
  already exists, and no automatic path deletes a
  draft. A unmarked, ambiguous, different-head/commit/identity, or manifest-mismatched
  pending review fails closed before mutation. An outdated semantic thread match
  is posted fresh.
  `data.threads_skipped_idempotent` reports the number skipped, and when every
  finding is already threaded the review event itself is skipped
  (`submitted_review` is `false`). An exact completed rerun also reports
  `submitted_review: false` while retaining the original submitted review URL;
  it does not emit another issue mirror activity entry. JSON output includes
  `data.review_threads[] = { id, url, path, line, subject_type }`, where `id` is
  the `PRRT_...` handle consumed by `pr review-threads resolve`. Dry-run output
  includes `data.target_plan`, `data.thread_plan[]`, `data.submit_plan`,
  `data.review_state_plan`, `data.review_receipt_plan`,
  `data.review_state_verify_plan`, and `data.planned_review_threads`. If GitHub
  rejects an individual thread mutation
  with HTTP 422 because the path/line is not commentable on the diff, the command
  returns `github_review_thread_rejected` (`RUNTIME 1`) with the raw backend
  detail and the failed spec index/path/line while preserving the pending
  review. Other interruption points return
  `pending_review_transaction_incomplete` (`DATA 65`) with the pending review
  intact and an identical rerun as the recovery action. Malformed or oversized
  specs return `invalid_review_thread_spec` (`DATA 65`); `--thread-file` without
  `--submit-review` returns `thread_file_requires_submit_review` (`DATA 65`);
  GitLab / Local return `provider_unsupported` (`USAGE 64`) before any backend
  call. Findings that cannot be mapped to a changed file/line should stay in
  the summary review body instead of being forced into a thread spec.
- On GitHub, PR comments use the issue-comments API endpoint so JSON output can
  report the created comment URL directly: `data.pr_comment_url`. Because that
  endpoint accepts both issues and pull requests, the command first verifies
  `<id>` is a pull request (`gh api repos/{repo}/pulls/{id}`) before posting — so
  a typo'd or non-PR number can never silently post a review outcome onto an
  unrelated issue. Only a `404 Not Found` from that guard yields
  `id_not_pull_request` (`DATA 65`); any other non-zero result (rate limit, 5xx,
  forbidden/SSO) surfaces as a retryable `backend_error` (`RUNTIME 1`) since it
  may have hit a valid PR. The guard read is also rendered in `--dry-run` output
  as `data.guard_plan`. On GitLab, a single `glab mr note create --help` probe
  selects the review-note form across every `glab` version class: when
  `--resolvable` is advertised it posts a non-resolvable status note
  (`glab mr note create … --resolvable=false`) so it does not register as an
  unresolved MR discussion that blocks the next `forge-cli pr merge`; when the
  `create` subcommand exists but lacks `--resolvable` it drops only that flag
  (`glab mr note create … --message`, which stays resolvable); and when the build
  has no `mr note create` subcommand at all it uses the bare
  `glab mr note <id> --message` form. If the backend prints a URL, it is
  surfaced as `data.pr_comment_url`.
- With `--mirror-issue --issue <number>`, the command posts a compact issue
  activity comment linking to the PR review comment and reports
  `data.issue_comment_url` when the backend returns one. `--mirror-issue`
  without `--issue` returns the `issue_required` (`DATA 65`) envelope at runtime
  (the CLI does not impose a clap parse-time requirement, so JSON consumers can
  branch on the error kind). The generated mirror body references the PR/MR with
  the provider-correct sigil (`#<n>` on GitHub, `!<n>` on GitLab). Its
  user-controlled `--lens` content is run through the same `no_local_path` and
  `no_escaped_control_markdown` guards as the review body, and that validation —
  plus the `--issue` requirement — is enforced before any backend post, so a
  rejected mirror can never leave a posted review outcome with no mirror.
- Output schema:
  `data = { provider, number, decision, submitted_review, head_sha?,
  pr_comment_url, issue_number, issue_comment_url, mirrored, lenses,
  review_threads?, threads_skipped_idempotent? }`. `head_sha` is present for
  `--submit-review`; `threads_skipped_idempotent` is present (non-zero) when
  cross-run duplicates were skipped.

### `pr review validate`

- The `pr review validate [id]` preflight surface emits
  `cli.forge-cli.pr.review.validate.v1` and never posts provider-visible review
  activity. It accepts `--comment <text>` / `--comment-file <path>` and
  `--thread-file <path>` using the same body limits, JSON shape, local-path
  guard, escaped-control guard, and size limits as `pr review`.
- Without `--check-diff`, validation is local-only and works for GitHub, GitLab,
  and Local provider contexts. This is the format/content dry-run path for
  agents that want to validate `review-report.md` and `review-threads.json`
  before a native review submission.
- With `--check-diff`, an `id` is required and the command is GitHub-only in v1.
  It fetches `gh api repos/{repo}/pulls/{id}/files --paginate`, parses each
  patch hunk, and verifies every line thread maps to a changed line on the
  requested `side` (`RIGHT` default, `LEFT` supported). Ranged threads must keep
  their start and end coordinates in one patch hunk with start before end.
  File-level threads only require the file to be part of the PR file list. A
  missing file returns `review_thread_file_not_changed` (`DATA 65`); a
  non-commentable line returns `review_thread_line_not_in_diff` (`DATA 65`); a
  reversed or cross-hunk range returns `review_thread_range_not_in_diff`
  (`DATA 65`). With global `--dry-run`, `--check-diff` does not invoke GitHub:
  JSON output includes `data.diff_plan` and
  `data.review_threads.diff_checked=false`.
- JSON output includes
  `data = { provider, number?, check_diff, comment, review_threads }`, where
  `comment = { present, bytes, lines }` and
  `review_threads = { count, diff_checked, specs[] }`. Each normalized
  `specs[]` entry includes `{ index, path, line?, side, start_line?, start_side?,
  subject_type, body_bytes }`.

### `pr review-threads resolve` / `pr review-threads reply`

- GitHub-first write surfaces over a single review thread, keyed by the
  thread `id` from the read surface. GitLab and Local have no
  GitHub-shaped thread-mutation surface, so both return
  `provider_unsupported` (`USAGE 64`) before any backend call.
- Live GitHub writes first verify that `--thread <thread_id>` belongs to the
  positional PR id. A mismatch fails with
  `review_thread_pr_mismatch` (`DATA 65`) before any reply or resolve mutation.
  `--dry-run` remains offline and does not perform this validation lookup.
- `pr review-threads resolve <id> --thread <thread_id>
  [--note <text> | --note-file <path>]`:
  - With `--note` / `--note-file`, posts a reply first via
    `addPullRequestReviewThreadReply(input: { pullRequestReviewThreadId,
    body })`, then resolves via `resolveReviewThread(input: { threadId
    })`. Without a note, only resolves.
  - Idempotent: `resolveReviewThread` succeeds on an already-resolved
    thread, so resolving twice is success rather than an error.
  - Output schema: `cli.forge-cli.pr.review-threads.resolve.v1`,
    `data = { provider, thread_id, resolved, replied }`.
- `pr review-threads reply <id> --thread <thread_id>
  --body <text> | --body-file <path>`:
  - Posts a reply via `addPullRequestReviewThreadReply` only; never
    resolves the thread.
  - Output schema: `cli.forge-cli.pr.review-threads.reply.v1`,
    `data = { provider, thread_id, comment_url }`.
- The `no_local_path` privacy guard runs over the reply note / body
  before the backend call, matching `pr comment`.

### `pr merge`

- Preconditions enforced before invoking the backend:
  - PR exists and is not draft (call `pr ready` first if needed);
  - working tree clean;
  - required checks all green **and at least one check reported**
    (re-checked even if `pr wait-checks` succeeded earlier — TTL-zero
    gate). A head with no required checks and no visible rows is refused
    with `checks_not_registered`; `--allow-no-checks` with a non-empty
    `--allow-no-checks-reason` bypasses it and records the reason in
    `data.no_checks_override_reason`. `pr deliver` accepts the same pair
    and forwards it to its wait-checks and merge steps;
  - target branch is the repo default branch OR explicitly approved
    via `--allow-non-default-base`;
  - when resolved review convergence is enabled, no current-head native
    `CHANGES_REQUESTED`, and any already-observed configured bot activity has
    stayed quiet for the resolved quiet period; the initial provider view must
    also expose a non-empty head OID or the merge fails closed with
    `review_convergence_head_missing`;
  - existing review-loop ledgers are gated independently of the quiet-
    convergence override: the latest typed state is bound to the exact merge
    head, has no active hard stop, and has no open blocking findings. Enabling
    review convergence requires an explicit genesis observation when no ledger
    exists. The chain tip must remain unchanged through the final pre-merge
    recheck; there is no force/rationale bypass for existing state;
  - no non-outdated unresolved review threads (outdated unresolved threads
    are mechanically dispositioned `stale` and recorded, not blocking) OR
    the remaining live threads explicitly bypassed via
    `--allow-unresolved-threads` with a recorded
    `--allow-unresolved-threads-reason`;
  - no unchecked task-list items in the description OR explicitly
    bypassed via `--allow-unchecked-tasks` with a recorded
    `--allow-unchecked-tasks-reason` (the description is the delivery
    contract: every `- [ ]` is checked off or rewritten as
    dispositioned before merge);
  - `--method squash|merge|rebase` (default `squash`, configurable
    per repo).
- Direct callers may pass `--expected-head <sha>`. The first provider snapshot
  must still expose that exact head or the command returns
  `test_first_evidence_provider_head_mismatch` before any merge mutation. The
  same reviewed head remains bound through the provider merge compare-and-swap.
- With convergence enabled, complete native-review and review-thread/comment
  snapshots are fetched once more after the thread/task gates and immediately
  before the provider merge. A late request blocks; any semantic digest change,
  including removal or cleanup after the quiet snapshot, returns
  `review_convergence_activity_changed` so the caller can rerun convergence.
- `--dry-run` validates the same enabled-policy provider contract as live
  merge and includes the resolved policy in `data.review_convergence`. GitLab
  therefore returns `provider_unsupported` before either path touches the
  provider. `pr deliver --no-merge` remains exempt because it has no merge
  convergence phase.
- Post-merge: deletes the remote branch (default `true`, disable via
  `--keep-branch`). GitHub passes `--match-head-commit <head_sha>`;
  GitLab performs the merge mutation through
  `glab api --method PUT projects/<project>/merge_requests/<iid>/merge`
  after all gates pass, including `sha=<head_sha>` when the MR view
  exposes it so the source branch HEAD cannot drift silently between
  checks and merge. If a backend reports failure after another actor completed
  the merge, idempotent recovery succeeds only when the merged head still
  equals that same OID.
- Output schema: `cli.forge-cli.pr.merge.v1`,
  `data = { number, url, merge_sha, method, deleted_branch,
  unchecked_tasks_override_reason?, unresolved_threads_override_reason?,
  no_checks_override_reason?,
  stale_thread_dispositions?, review_convergence?, review_loop? }`.
  `unchecked_tasks_override_reason` / `unresolved_threads_override_reason` /
  `no_checks_override_reason` are
  present only when the matching bypass was used; `stale_thread_dispositions`
  lists the outdated threads dispositioned `stale` at rule 13 (omitted when
  none). The additive convergence snapshot contains
  `required`, `head_sha`, `observed_reviews`, `stale_reviews`,
  `unresolved_threads`, `changes_requested_by`, `missing_reviewers`,
  `latest_activity_at`, `quiet_until`, `quiet_period_ms`, `timeout_ms`,
  `waited_ms`, and the resolved `bots`. It is absent while the feature is off.

### `label audit` / `label ensure`

- Input: `--catalog <path>` pointing at a caller-owned YAML/JSON catalog with
  `groups[]` and `labels[]`. Labels declare `name`, `group`, `color`,
  `description`, and `applies_to`.
- `label audit` emits `cli.forge-cli.label.audit.v1` with
  `data = { provider, status, missing, drift, unknown_shared }`.
- `label ensure` emits `cli.forge-cli.label.ensure.v1` with
  `data.actions[]` create/update plans. Missing labels are created; existing
  label drift is updated only with `--update-existing`.
- Provider behavior:
  - GitHub: `gh label list/create/edit`.
  - GitLab: `glab label list/create/edit`.
- Non-goals: deletion, rename-by-default, and moving catalog ownership into
  `nils-cli`.

(See `forge-cli-ops-v1.yaml` for the remaining ops.)

## Macro: `pr deliver`

`pr deliver` is the canonical end-to-end flow agent-runtime-kit's
`deliver-{feature,bug}-pr` skills compose today. It is implemented in
Rust so behaviour is fixed at the type level and identical across
providers.

Synopsis:

```text
forge-cli pr deliver \
  --kind feature|bug \
  [--title <str>] [--body-file <path>] \
  [--base <branch>] [--head <branch>] \
  [--method squash|merge|rebase] \
  [--reviewer <user>...] \
  [--label <name>...] \
  [--label-catalog <path> --strict-labels] \
  [--timeout <duration>] \
  [--review-convergence[=<true|false>]] \
  [--no-merge]        # stop after wait-checks; useful in CI
```

Steps:

Before any step, `--strict-labels` validates `--label-catalog` and every
selected label against the provider target. This same pure preflight runs for
dry-run, create, and adopt delivery, so `label_catalog_missing` and related
label errors are returned before provider access in every mode.

1. `auth status` — fail-fast on missing auth.
2. `repo view` — resolve default branch, repo slug, default merge
   method override.
3. Head-branch lookup (`pr list --state open --head <branch>`) — when an
   open PR/MR already exists for the resolved head branch, the macro
   adopts it instead of creating: the create step (and its create-input
   gates) is skipped, an `adopt` step carrying the PR's `pr.view`
   payload is recorded, and the lifecycle resumes from step 5. The
   adopted PR's *actual* body (fetched via `pr view`) is re-validated
   against the body-section gate, and the branch-kind / clean-worktree /
   resolved-head push-state rules still apply. `--title` / `--body`
   inputs are ignored on adoption — the existing PR keeps its own.
   For a qualified GitHub `<user>:<branch>` head, `gh pr list --head` is not
   given the unsupported qualified syntax. Delivery lists a bounded open set
   and adopts only a row whose branch and `headRepository` exactly match the
   locally bound user/upstream repository; a same-named branch from any other
   fork is ignored. A saturated bounded result without that exact row fails
   closed instead of assuming absence. The subsequent `pr view` must still match branch,
   repository, and local SHA, and that SHA becomes the merge CAS input.
4. `pr create --draft` — atom; validates branch / title / body. Only
   runs when the lookup found nothing.
5. `pr wait-checks` — atom; blocks until terminal within one cumulative
   `--timeout` budget. Delivery keeps the explicit atom's required-only
   behavior, but on GitHub a successful required-only snapshot with zero
   required checks and visible check rows is re-gated against all visible
   checks. A terminal retained snapshot needs no extra provider request;
   pending visible checks continue polling in all-check mode with only the
   remaining timeout. Failed visible checks block delivery. A genuinely empty
   check set completes immediately. GitLab behavior is unchanged.
6. `pr ready` — atom; only if previous step is `success`.
7. `pr merge` — atom; honours `--method`, repo override, and the same resolved
   review-convergence policy as a direct merge. `--no-merge` performs no
   convergence wait because the merge phase is skipped.
8. Emit single envelope summarising every sub-step output under
   `data.steps[]` (`create` and `adopt` are mutually exclusive). The
   macro's own schema is `cli.forge-cli.pr.deliver.v1`.

Failure semantics:

- A failing step short-circuits the macro. The envelope still lists
  every step attempted, with the failing one's `ok = false` and the
  remaining ones omitted (not present in the array).
- The macro's outer exit code is the failing atom's exit code, not
  remapped. So `checks_failed` propagates as `RUNTIME 1`, `policy`
  violations propagate as `DATA 65`, and so on.

## Lock-down policy

These rules are enforced regardless of which backend runs. They are
declared in `forge-cli-ops-v1.yaml` (`validations:` per op) so the
backend implementations cannot diverge.

1. **Branch naming.** Feature work MUST be on `feat/<slug>`, bug work
   on `fix/<slug>`. Slug is `[a-z0-9][a-z0-9-]{1,63}`. Ticket prefix
   `abc-123-` is allowed inside the slug. `--kind` and the branch
   prefix MUST agree.
2. **Body schema.** PR/MR body MUST contain non-empty `## Summary` and
   `## Test plan` sections. Order is not enforced; presence and
   non-emptiness are.
3. **Title length.** ≤ 70 characters, no trailing whitespace, no
   trailing punctuation other than `?`.
4. **Working tree.** `git status --porcelain` MUST be empty for
   `create`, `merge`, and `deliver`. (`view`/`list`/`comment` are
   read/append-only and do not check.)
5. **Push state.** The resolved head branch MUST be pushed to a
   remote-tracking branch before `create` or `deliver` runs.
6. **Default-branch protection.** PR/MR delivery remains the default.
   `forge-cli` refuses any unconstrained force-push, caller-controlled lease,
   delete, or direct merge
   into the repo default branch. The sole direct-delivery exception is
   `repo push-default`: one clean, locally verified signed commit on the exact
   expected remote base, one uniquely bound actual push destination, delivered
   with an exact old-OID compare-and-swap fast-forward and remote SHA read-back.
   `--allow-non-default-base` applies only to PR/MR base branches; no flag
   bypasses the default-branch force refusal.
7. **Draft → ready → merge ordering.** `pr merge` refuses to merge a
   draft. There is no `--merge-as-draft`. Callers MUST run `pr ready`
   first (or use `pr deliver`, which sequences them).
8. **Required-check gating.** `pr merge` re-checks required-check
   state immediately before invoking the backend, even if
   `pr wait-checks` was called earlier in the macro. This is the
   TTL-zero re-check that addresses the
   `github-pr-required-check-gating` operation record.

   **Absence is not success.** "All required checks passed" is vacuously
   true over an empty set, so a snapshot reporting no required checks
   *and* no visible check rows means nothing ran — not that everything
   passed. `pr merge` fails closed with `checks_not_registered`
   (`DATA 65`) and `pr wait-checks` treats the empty set as non-terminal,
   polling out its budget before reporting the same kind. Both accept
   `--allow-no-checks` (with a recorded `--allow-no-checks-reason`) for a
   repository that genuinely configures none.

   The predicate deliberately requires *both* halves. A repository can run
   CI without branch protection, producing zero required checks alongside
   visible rows; refusing those would block every such repository. What is
   refused is the case with nothing to fall back to, which is also what the
   provider reports during the check-registration window after a
   force-with-lease — the window in which delivery previously reported
   success against an unchecked head.

   Be precise about what happens to the zero-required-with-visible-rows
   case, because the two commands differ. `pr deliver` re-gates those rows
   against the full visible set — but only on GitHub
   (`should_gate_visible_checks`). Standalone `pr merge` does **not**
   re-gate them on any provider: its snapshot is taken with
   `required_only`, so a non-required failing row is not in `failed` and
   the gate accepts the head. That gap predates this rule and is not
   closed by it.

   The rule is provider-neutral, and on GitLab that is a behaviour change
   worth stating: an MR whose head has no pipeline — no `.gitlab-ci.yml`,
   excluded by `workflow:rules`, or pipelines disabled on a fork — reports
   the same empty snapshot and is now refused rather than merged. That is
   the intended reading of "absence is not success", and
   `--allow-no-checks` is the declared way to say a project has no CI.
9. **Merge method.** Default `squash`. Repo override allowed via
   `.forge-cli.toml` `[merge] method = "squash" | "merge" | "rebase"`.
   Per-invocation `--method` overrides the repo override; both are
   logged in `data.method`.
10. **Branch cleanup.** `pr merge` deletes the remote branch by
    default (`--delete-branch` on `gh`, `--remove-source-branch` on
    `glab`). Disable with `--keep-branch`. Local branch cleanup is
    out of scope for `forge-cli` and remains the bash wrapper's job
    in agent-runtime-kit skills.
11. **Portable paths.** PR/MR and issue title, body, and comment text MUST
    NOT embed a machine-local home path (`/Users/<owner>/…`,
    `/home/<owner>/…`). This mirrors the repo-side portable-paths file hook on
    the provider egress path so local paths cannot leak into provider content.
    The error `detail` enumerates each offending line and its `$HOME`-relative
    fix without echoing the original personal path. Literal container /
    CI-runner home roots (`/home/agent`, `/home/linuxbrew`, and the CI runner
    work root) are allowlisted — see the
    `no_local_path` entry in `forge-cli-ops-v1.yaml` for the exact list; set
    `FORGE_CLI_ALLOW_LOCAL_PATH=1` to bypass a verified false positive. Enforced
    by `pr create`, `pr edit`, `issue create`, `issue edit`, `pr comment`,
    `pr review`, and `issue comment`.
12. **Opt-in native-review convergence.** `pr merge` and the merge step of
    `pr deliver` resolve `--review-convergence[=true|false]` over repo and
    global config. The default is off. In v1, configured bots use `observed`:
    an absent bot never waits or fails, while already-observed current-head
    review, live non-outdated thread, or unowned comment activity must stay quiet
    for `quiet_period`. New semantic activity restarts that window; marker-only
    forge-cli disposition replies, empty `COMMENTED` reviews, provider ids and
    timestamps, and resolve/outdated transitions do not. Reopening a live thread
    does restart the window. `timeout` bounds the complete active wait, including provider
    calls and rate-limit waits. Polling occurs at most once every 10 seconds.
    Current-head native `CHANGES_REQUESTED` fails with
    `review_changes_requested`; `COMMENTED`
    prose is never parsed as a verdict. Older-head reviews are returned as
    stale evidence. A head change during an active wait fails closed with
    `review_convergence_head_changed`; a missing initial provider head fails
    with `review_convergence_head_missing` before review collection. The
    complete paginated review plus thread/comment snapshot is read
    again immediately before merge; any digest drift fails with
    `review_convergence_activity_changed`, and incomplete or unknown provider
    data fails with `review_snapshot_incomplete`. Every review must carry a
    non-empty `commit.oid` before current-head/stale classification. For each
    reviewer, a later
    native `APPROVED` or `DISMISSED` supersedes an earlier request on the same head;
    `COMMENTED` does not alter that opinionated state. GitHub is the only
    supported provider in v1; an enabled policy elsewhere returns
    `provider_unsupported`.
13. **Review-thread gating.** `pr merge` (and the `pr deliver` merge
    step) fetches review threads during the final merge gates and refuses to
    merge while any **non-outdated** thread is unresolved
    (GitHub `reviewThreads`, GitLab resolvable discussions). An **outdated**
    unresolved thread (its anchored diff hunk changed) is mechanically
    dispositioned `stale` — recorded in the merge payload as
    `stale_thread_dispositions` (thread id, author, path, summary, rationale),
    never silently dropped — and no longer blocks, so an accumulation of stale
    bot threads can no longer wedge convergence. Bypass any remaining live
    threads with `--allow-unresolved-threads` plus a required
    `--allow-unresolved-threads-reason`, recorded as
    `unresolved_threads_override_reason`. The local provider has no
    thread model and passes trivially. The error `detail` lists each
    blocking (non-outdated) unresolved thread (author, file anchor, first line).
14. **Task-list gating.** `pr merge` (and the `pr deliver` merge step)
    parses GFM task-list items out of the PR/MR description fetched at
    merge time and refuses to merge while any `- [ ]` item is
    unchecked (`- [x]` / `- [X]` count as done, GitLab's `- [~]` as
    inapplicable; fenced code blocks are skipped). Bypass with
    `--allow-unchecked-tasks` plus a required
    `--allow-unchecked-tasks-reason`, which is recorded in the merge
    payload as `unchecked_tasks_override_reason`. Providers without a
    body model (local) pass trivially. The error `detail` lists each
    unchecked item (line number, text).
15. **Review-thread write ownership.** `pr review-threads reply` and
    `pr review-threads resolve` verify live GitHub writes by fetching the
    positional PR's review threads and confirming `--thread <id>` is present
    before posting a reply or resolving. `--dry-run` remains offline and skips
    this lookup.
16. **Pending-review recovery ownership and content binding.** `inspect`,
    `resume-submit`, guarded unmarked `submit`, and `discard` read the exact
    `--review` node plus every inline-comment page and bind mutations to the
    resulting snapshot digest, PR head, review commit, PR membership, and
    provider-native viewer identity. Receipt-bound automatic recovery adds only
    a missing ordered manifest suffix and never deletes a draft; an
    already-submitted matching run is idempotent success. Unmarked submit and all
    discard paths require explicit confirmation, with a distinct confirmation
    for inline-content loss. The compatibility `delete` additionally requires
    `--confirm-abandoned` and an exact bounded body/body-file value, retains only
    the target during complete membership pagination, re-fetches the exact node,
    and refuses any inline comment. These provider-native viewer fields support
    both user and GitHub App installation actors without `GET /user`. GitHub
    exposes no content CAS on delete/discard, so the documented
    final-read-to-delete race remains after exact snapshot guards.
17. **No agent attribution.** PR/MR and issue title, body, and comment text
    MUST NOT carry agent self-attribution: a generator marker line
    (`Generated with …` prose, or a `claude.com/claude-code` /
    `claude.ai/code` link) or a `Co-Authored-By` trailer whose value names the
    model family or carries the vendor no-reply address
    (`noreply@anthropic.com`). The marker forms are defined once in
    `nils_common::agent_attribution` and shared with `semantic-commit`'s
    `claude-coauthor-trailer` / `claude-generated-marker` blocked-message rules,
    so the commit path and the provider path cannot diverge. Enforcement lives
    in the CLI rather than in an agent-harness hook, so the rule holds whether or
    not the calling runtime declares a matching hook of its own. Text *about*
    the rule is allowed: fenced
    code blocks and inline code spans are stripped before the scan, so a body
    documenting `` `Co-Authored-By: Claude ...` `` passes while a bare
    attribution line does not (commit messages get no such exemption — that
    scan is verbatim). The error `detail` enumerates each offending line and its
    fix without echoing the marker; set
    `FORGE_CLI_ALLOW_AGENT_ATTRIBUTION=1` to bypass a verified false positive.
    Enforced by `pr create`, `pr edit`, `issue create`, `issue edit`,
    `pr comment`, `issue comment`, `pr review`, `pr review-threads reply`, and
    `pr review-threads resolve`.

Violations map to `DATA 65` with one of these `data.error.kind` values:

| `error.kind`                               | Triggered by rule     |
| ------------------------------------------ | --------------------- |
| `branch_name_invalid`                      | 1                     |
| `branch_kind_mismatch`                     | 1                     |
| `body_missing_summary`                     | 2                     |
| `body_missing_test_plan`                   | 2                     |
| `title_too_long`                           | 3                     |
| `dirty_worktree`                           | 4                     |
| `head_not_pushed`                          | 5                     |
| `default_branch_protected`                 | 6                     |
| `push_destination_missing`                 | 6                     |
| `push_destination_ambiguous`               | 6                     |
| `push_destination_credentials_unsupported` | 6                     |
| `push_destination_rewrite_ambiguous`       | 6                     |
| `provider_mismatch`                        | 6                     |
| `provider_unsupported`                     | 6 (`USAGE 64`)        |
| `repository_mismatch`                      | 6                     |
| `detached_head`                            | 6                     |
| `default_branch_checkout`                  | 6                     |
| `head_not_checked_out`                     | 6                     |
| `expected_base_mismatch`                   | 6                     |
| `expected_base_missing`                    | 6                     |
| `expected_base_not_ancestor`               | 6                     |
| `direct_commit_count_invalid`              | 6                     |
| `commit_signature_unverified`              | 6                     |
| `reason_file_unreadable`                   | 6                     |
| `reason_invalid`                           | 6                     |
| `object_id_invalid`                        | 6                     |
| `remote_default_lookup_failed`             | 6 (`UNAVAILABLE 69`)  |
| `remote_default_branch_missing`            | 6                     |
| `git_timeout`                              | 6 (`UNAVAILABLE 69`)  |
| `git_output_limit`                         | 6 (`UNAVAILABLE 69`)  |
| `backend_timeout`                          | 6 (`UNAVAILABLE 69`)  |
| `backend_output_limit`                     | 6 (`UNAVAILABLE 69`)  |
| `default_push_rejected`                    | 6 (`RUNTIME 1`)       |
| `default_push_verification_failed`         | 6 (`RUNTIME 1`)       |
| `software_error`                           | 6 (`SOFTWARE 70`)     |
| `draft_merge_refused`                      | 7                     |
| `checks_pending`                           | 8                     |
| `checks_failed`                            | 8 (`RUNTIME 1`)       |
| `checks_not_registered`                    | 8                     |
| `merge_method_unsupported`                 | 9                     |
| `keep_branch_conflict`                     | 10                    |
| `local_path_present`                       | 11                    |
| `agent_attribution_present`                | 17                    |
| `review_changes_requested`                 | 12                    |
| `review_convergence_head_missing`          | 12                    |
| `review_convergence_head_changed`          | 12                    |
| `review_convergence_activity_changed`      | 12                    |
| `review_snapshot_incomplete`               | 12, 16                |
| `invalid_review_convergence_config`        | 12                    |
| `unresolved_review_threads`                | 13                    |
| `unchecked_task_items`                     | 14                    |
| `review_thread_pr_mismatch`                | 15                    |
| `pending_review_not_found`                 | 16                    |
| `pending_review_author_mismatch`           | 16                    |
| `pending_review_not_deletable`             | 16                    |
| `pending_review_head_mismatch`             | 16                    |
| `pending_review_commit_mismatch`           | 16                    |
| `pending_review_body_mismatch`             | 16                    |
| `pending_review_body_too_large`            | 16                    |
| `pending_review_pr_mismatch`               | 16                    |
| `pending_review_inline_comments_present`   | 16                    |
| `pending_review_lease_busy`                | 16 (`UNAVAILABLE 69`) |
| `pending_review_lease_unsafe`              | 16                    |
| `pending_review_reconciliation_failed`     | 16                    |

## Activity output contract

`activity` is read-only and reports recent activity through the same workspace
envelope used by the parity ops. Personal commands remain GitHub-only in v1;
`activity feed` is repo/project-scoped and supports GitHub and GitLab. Local
providers return `provider_unsupported`.

- `activity commits` emits `cli.forge-cli.activity.commits.v1` with
  `data.provider`, `data.host`, `data.user`, `data.since`, `data.limit`,
  `data.item_count`, `data.limited`, and normalized `data.items[]`.
- Commit items include `repo`, `sha`, `url`, `message`, `authored_at`,
  `committed_at`, `author_name`, and `author_email`.
- `activity events` emits `cli.forge-cli.activity.events.v1` with
  `data.provider`, `data.host`, `data.user`, `data.public_only`,
  `data.limit`, `data.item_count`, `data.limited`, and normalized
  `data.items[]`.
- Event items include `id`, `event_type`, `repo`, `actor`, `public`,
  `created_at`, `summary`, and `url`.
- `activity feed` emits `cli.forge-cli.activity.feed.v1` with
  `data.provider`, `data.host`, `data.repo`, `data.since`, `data.limit`,
  `data.item_count`, `data.limited`, and normalized `data.items[]`.
- Feed items include `id`, `external_id`, `provider_event_type`, `kind`,
  `action`, `repo`, `target_kind`, `target_ref`, `target_iid`, `title`, `url`,
  `actor`, `occurred_at`, `summary`, and `details`.
- Feed `kind` and `action` are open vocabulary. Consumers may group common
  values such as `commit` / `committed`, `branch` / `pushed`, and
  `change_request` / `merged`, but unknown provider events stay represented
  instead of being rejected. The provider-native event name is retained in
  `provider_event_type`; provider-specific fields such as refs, commit ranges,
  and GitLab target types live under `details`.
- `activity summary` emits `cli.forge-cli.activity.summary.v1` with
  `data.provider`, `data.host`, `data.user`, `data.since`, `data.limit`,
  `data.total_commit_contributions`, `data.repository_count`,
  `data.limited`, and normalized `data.repositories[]`.
- Summary repository rows include `repo`, `commit_contributions`, and
  `latest_commit_at`.
- `limited=true` means the returned row count reached the requested bound, so
  more provider-side rows may exist.

## Search output contract

`search` is read-only and runs free-text / reverse-reference queries the
structured `issue list` / `pr list` filters cannot express. It delegates to the
provider's search primitives and builds no index. GitHub is the v1 target;
GitLab and Local return a structured `provider_unsupported` error (search is
GitHub-only in v1), never a silent empty result.

Role split (documented in `forge-cli search --help`): `issue list` / `pr list`
filter by structured fields within one repo, `inbox` is the personal cross-repo
work queue, and `search` is full-text and reverse-reference query.

All three subcommands are single-repo scoped: the repo slug comes from `--repo
owner/name` or the detected forge remote. Every item is the shared normalized
`SearchItem`: `kind` (`issue` | `pr`), `number`, `url`, `title`, `state`,
`repo`, and `matched_field` (best-effort; `null` when the provider does not
report which field matched, which the GitHub path never does).

- `search issues <query>` emits `cli.forge-cli.search.issues.v1` and
  `search prs <query>` emits `cli.forge-cli.search.prs.v1`, each with
  `data.provider`, `data.host`, `data.repo`, `data.query`, `data.match_fields`,
  `data.limit`, `data.item_count`, `data.limited`, and normalized `data.items[]`.
  They run `gh search <issues|prs> <query> --repo <slug> --match <fields>
  --limit <n> --json …`. The `--repo` value is always the raw `owner/name`
  slug; the resolved authority is selected separately through the call-local
  `GH_HOST` binding. `--match` defaults to `title,body,comments` and is
  narrowable. A body-only hit the structured `issue list` cannot surface is
  returned; an empty result is a well-formed empty envelope.
- `search refs-to <ref>` emits `cli.forge-cli.search.refs-to.v1` with
  `data.provider`, `data.host`, `data.repo`, `data.reference_number`,
  `data.limit`, `data.item_count`, `data.limited`, and normalized
  `data.items[]`. `<ref>` parses as a GitHub URL, `owner/name#number`, or
  `#number` / `number` (repo from context); an unparseable ref is
  `DATA 65` with `error.kind=ref_invalid`. It runs `gh api graphql` over the
  target's `CROSS_REFERENCED_EVENT` timeline items and normalizes the
  referencing issues / PRs into `SearchItem`s.
- All three render the exact backend argv under `--dry-run` (`data.plan`),
  honor `--format json`, and set `limited=true` when the returned row count
  reached the requested bound.

## Inbox output contract

`inbox` is read-only and aggregates personal work discovery across selected
providers:

- `inbox list` emits `cli.forge-cli.inbox.list.v1` with
  `data.providers[]`, `data.limit`, and normalized `data.items[]`.
- `inbox status` emits `cli.forge-cli.inbox.status.v1` with bounded count rows
  grouped by provider, host, kind, and reason. Counts are bounded by the
  effective query limit, not guaranteed global totals.
- `inbox next` emits `cli.forge-cli.inbox.next.v1` with ranked candidates;
  review-requested work ranks ahead of assigned, todo, authored, and broad
  involved work.
- Every item includes `provider`, `host`, `kind`, `reasons`, `repo`, `number`,
  `title`, `url`, `updated_at`, `author`, and `source`.
- `reasons` is a deterministic de-duplicated array. Allowed v1 values are
  `review`, `assigned`, `todo`, `authored`, and `involved`.
- Partial provider success exits `SUCCESS 0`: successful provider items remain
  in `data.items[]`, failed providers are represented in
  `data.providers[].error`, and top-level `warnings[]` carries string warnings under the shared
  workspace envelope contract.
- With `--strict-providers`, any selected provider failure exits `RUNTIME 1`
  with `error.code=provider_failed`; `error.details.providers[]` preserves the
  same provider rows that a non-strict success envelope would have emitted.
- If every selected provider fails, `inbox` exits non-zero through the normal
  `cli_contract` failure envelope with `error.details.providers[]` instead of
  returning an empty successful inbox.
- GitLab VPN readiness failures use `vpn_unavailable` when the configured probe
  failed and `vpn_probe_dependency_missing` when an optional probe dependency,
  such as the `openvpn` CLI, is missing. GitLab backend subprocess timeouts use
  `backend_timeout`.
- Cached fallback items include a `stale` object with `reason`,
  `cached_at_unix`, and `age_seconds`; the provider row remains `ok=false` and
  carries `cache.used=true` so stale data never masquerades as live success.
- GitLab rows come from host-aware `glab api --hostname <host>` calls. The user
  id and username are discovered with `glab api user --hostname <host>` for the
  current invocation and are not persisted. The identity lookup is skipped when
  no remaining query family needs it (for example `--kind assigned --kind
  todo`, or `--item-type issue` paired with reasons that drop the
  identity-dependent families).
- Selected provider adapters and independent query families within a provider
  run concurrently. The output contract — provider order, deduplicated items,
  warnings, and partial/all-failure exit behavior — is deterministic and
  independent of thread completion order. Live wall-clock latency depends on
  `gh`/`glab` and remote API responsiveness and is not asserted in CI.

## CLI output contract conformance

`forge-cli` follows
[`cli-output-contract-v1`](../../../../docs/specs/cli-output-contract-v1.md)
without exception:

- Canonical flag is `--format text|json`. No `--json` boolean alias
  is introduced (new binary, no migration debt).
- Envelope: `{schema_version, ok, data, warnings}`. Snake_case
  throughout. `data` omitted when no payload. `warnings` omitted when
  empty.
- Schema version literals follow `cli.forge-cli.<command>.v1`. Macro
  steps embed their own atom schema versions inside `data.steps[].
  payload.schema_version`.
- Parse / unknown-subcommand errors go through
  `nils_common::cli_contract::emit_parse_error` so `--format json`
  works at the parse-error layer too.
- Sensitive data: backend stderr is captured and tail-trimmed to 2 KiB
  before being placed under `data.error.detail`. Token-shaped strings
  (`gh[ps]_*`, `glpat-*`, `ghr_*`, `gho_*`) are redacted before
  rendering. URLs are kept as-is — they are not secrets and the user
  wants click-through.

## Exit code map

`forge-cli` uses only the six BSD sysexits-aligned constants from
`nils_common::cli_contract::exit`. Discriminators go in
`data.error.kind`, not in numeric exit codes.

| Constant      | Value | `forge-cli` triggers                                                                                                                                                                                                                                                                                         |
| ------------- | ----- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `SUCCESS`     | `0`   | Op completed; required state achieved.                                                                                                                                                                                                                                                                       |
| `RUNTIME`     | `1`   | Remote semantic failure: required checks failed, merge conflict, draft already ready.                                                                                                                                                                                                                        |
| `USAGE`       | `64`  | Bad CLI syntax, unknown subcommand, unsupported provider.                                                                                                                                                                                                                                                    |
| `DATA`        | `65`  | Lock-down policy violation (any rule above); body parse failure; invalid VPN config.                                                                                                                                                                                                                         |
| `UNAVAILABLE` | `69`  | `gh`/`glab` missing, auth required, remote 5xx/network error, wait-checks expiry when checks were seen but did not finish (an expiry with none ever reported is `DATA 65` / `checks_not_registered`), review-convergence timeout, GitLab VPN probe failure, backend timeout or backend output-limit failure. |
| `SOFTWARE`    | `70`  | Internal invariant violation (backend JSON did not match expected shape).                                                                                                                                                                                                                                    |

Callers (agent-runtime-kit skills, CI scripts) MUST branch on `error.kind`
when finer granularity is needed. Numeric exit codes alone are
intentionally not enough to distinguish "branch name invalid" from
"missing Test plan section" — both are `DATA 65` because both are
"the input did not meet the documented contract". The discriminator
is in the envelope, where consumers parse it deliberately.

## Configuration

Per-repo overrides live in `.forge-cli.toml` at the repo root:

```toml
[merge]
method = "squash"            # squash | merge | rebase
delete_branch = true

[body]
summary_heading = "## Summary"
test_plan_heading = "## Test plan"

[branch]
feature_prefix = "feat/"
bug_prefix = "fix/"

[checks]
timeout = "30m"
interval = "20s"
required_only = true

[inbox]
gitlab_vpn = "off"                    # off | optional | required
gitlab_vpn_check = "tcp:gitlab.example.com:443"
gitlab_vpn_check_timeout = "2s"
gitlab_openvpn_profile = "<local-openvpn-profile>"
provider_timeout = "20s"
strict_providers = false
cache_fallback = false
cache_max_age = "30m"
no_cache = false

[test_first]
require = false                       # when true, pr create / pr deliver for
                                      # feature/bug kinds must carry verified
                                      # test-first evidence (see below)

[review_convergence]
require = false                       # compatibility default
quiet_period = "2m"                  # after relevant activity is observed
timeout = "20m"                      # bounds an active convergence wait

[[review_convergence.bots]]
login = "example-review-bot"
mode = "observed"                    # absence never waits or fails
```

### Global config layer

The same schema may live in a user-global file at
`${XDG_CONFIG_HOME:-$HOME/.config}/forge-cli/config.toml`. It supplies defaults
beneath the per-repo `.forge-cli.toml`, so a setting (e.g. `[test_first]
require = true` or `[merge] method = "rebase"`) applies across every repo
without duplicating it into each checkout. A missing global file is not an
error. The global layer feeds the sections forge-cli actually consumes from
config today — `[merge]`, `[inbox]`, `[test_first]`, and
`[review_convergence]`. The `[checks]`,
`[body]`, and `[branch]` keys are parsed (and validated) for
forward-compatibility but are not yet wired into the corresponding command
paths at either layer, so values placed there are accepted but currently
inert.

Resolution order for any setting: explicit flag > repo `.forge-cli.toml` >
global `config.toml` > spec default. Inbox env vars sit between explicit flags
and `.forge-cli.toml`. Unknown keys produce a `warnings[]` entry, not an
error — forward-compatibility for v2 fields.

Review convergence has one safety-preserving exception: once global
`[review_convergence].require = true`, repo config may add bots, lengthen the
global/default quiet period, or override the failure timeout, but cannot disable
the gate, remove global bots, or shorten the quiet period. The explicit
`--review-convergence=false` flag remains the intentional per-invocation
override.

### `[test_first]` — test-first evidence gate

`require` defaults to `false`; the gate is off unless a repo or the global
config opts in. When it resolves `true`, `pr create` and `pr deliver` (both the
create and adopt paths, and the `--dry-run` preflight) require
`--test-first-evidence <dir>` for `--kind feature` / `bug`. The directory must
hold a verified v2 `test-first-evidence` record with a testable classification,
an actual changed/added/removed behavior, an affected-test decision, meaningful
failing evidence or an explicit waiver, scoped passing validation, and a
residual-gap declaration. The record must also bind an immutable repository and
pre-edit baseline plus a latest delivery head/tree/diff attestation matching
the explicitly selected delivery ref. Create and adopt compare that attested
head with the provider's immutable PR/MR head OID; deliver re-fetches it after
checks and after ready, and merge uses the same OID as a compare-and-swap
condition. Amend, rebase, or post-check push operations invalidate the latest
delivery attestation until `test-first-evidence bind-delivery` appends a new
attempt; the baseline is never replaced. Record v1 remains readable but cannot
satisfy this gate. `docs` / `chore` / `ci` / `refactor` kinds are exempt. Failures
surface as `test_first_evidence_required`, `test_first_evidence_v1`,
`test_first_evidence_classification`, `test_first_evidence_incomplete`,
`test_first_evidence_unbound`, `test_first_evidence_subject_mismatch`,
`test_first_evidence_provider_head_unavailable`,
`test_first_evidence_provider_head_mismatch`, or
`test_first_evidence_unreadable` (exit `DATA`).

### `[review_convergence]` — observed native review gate

`require` defaults to `false`, `quiet_period` to `2m`, and `timeout` to `20m`.
`quiet_period` is limited to `1h`; `timeout` is limited to `24h`. Invalid
or arithmetically overflowing values are warnings while the feature is
disabled, but fail an enabled merge
with `invalid_review_convergence_config` instead of silently falling back.
Normally `bots` is a whole-list override: a repo list replaces the global list.
When global `require = true`, repo bots are unioned with global bots as part of
the safety exception above. The v1 `observed` mode is intentionally
absence-tolerant. If no configured bot review exists against the current head,
merge continues immediately and `missing_reviewers` remains empty. If relevant
activity exists, each new native review resets the quiet window. The active
wait polls no more often than every 10 seconds. Timeout anywhere in that
end-to-end wait returns `review_convergence_timeout` (`UNAVAILABLE 69`). Timed
backend subprocesses retain at most 8 MiB per output stream; overflow kills the
process group and returns `backend_output_limit` (`UNAVAILABLE 69`).

The positive flag form `--review-convergence` enables the resolved policy for
one `pr merge` or `pr deliver`; `--review-convergence=false` explicitly
disables a repo/global opt-in. Precedence is explicit flag > repo config >
global config > default, subject to the enabled-global safety exception.
Disabling this feature never disables the existing unresolved-thread gate.
`pr close` has no convergence flag and remains unchanged.
For `pr merge --dry-run` and the merge-capable `pr deliver --dry-run`, the
resolved policy is additive output under `data.review_convergence`; the same
GitHub-only provider check runs before the dry/live split.

Environment variables (read once at startup, all optional):

- `FORGE_CLI_GH_BIN` — override `gh` discovery path (testing).
- `FORGE_CLI_GLAB_BIN` — override `glab` discovery path (testing).
- `FORGE_CLI_INBOX_GITLAB_HOST` — default GitLab host for inbox commands.
- `FORGE_CLI_INBOX_GITLAB_VPN`,
  `FORGE_CLI_INBOX_GITLAB_VPN_CHECK`,
  `FORGE_CLI_INBOX_GITLAB_VPN_CHECK_TIMEOUT`, and
  `FORGE_CLI_INBOX_GITLAB_OPENVPN_PROFILE` — local GitLab VPN readiness
  settings. Profile values are input-only and redacted from output.
- `FORGE_CLI_INBOX_PROVIDER_TIMEOUT`,
  `FORGE_CLI_INBOX_STRICT_PROVIDERS`,
  `FORGE_CLI_INBOX_CACHE_FALLBACK`,
  `FORGE_CLI_INBOX_CACHE_MAX_AGE`, `FORGE_CLI_INBOX_NO_CACHE`, and
  `FORGE_CLI_INBOX_CACHE_DIR` — inbox timeout/cache controls.
- `FORGE_CLI_DEFAULT_PROVIDER` — fallback provider when remote URL
  doesn't auto-detect.
- `FORGE_CLI_RATE_LIMIT_GATE` — set to `off`/`0`/`false`/`no` to disable the
  GraphQL rate-limit gate (default enabled).
- `FORGE_CLI_RATE_LIMIT_MIN_REMAINING` — GraphQL budget headroom below which the
  gate waits before a GraphQL-backed call (default `50`).
- `FORGE_CLI_RATE_LIMIT_MAX_WAIT_SECS` — upper bound on the gate's wait for the
  GraphQL budget to recover, per gated attempt (default `120`).
- `FORGE_CLI_RATE_LIMIT_POLL_SECS` — re-probe interval while the GraphQL budget
  is below the threshold (default `15`, minimum `1`).

## GraphQL rate-limit gate

GitHub meters the GraphQL API on a budget separate from REST/core, so the
shared GraphQL budget can be drained by other consumers while REST/core still
has thousands of requests remaining. A subsequent GraphQL-backed call then
fails — historically surfacing as a misleading "not available"/not-found error
that risks a wrong conclusion (sympoies/nils-cli#1051).

Two mechanisms harden this:

- Backend stderr that reports an exhausted rate limit is classified as an
  explicit `backend_rate_limited` error (`UNAVAILABLE 69`) rather than a generic
  `backend_error`, so throttling is never mistaken for a missing resource.
- Every op builds its live backend runner through a single `default_runner()`
  factory rather than a bare process runner, so all GraphQL-backed calls — not
  just the PR-lifecycle ops — preflight the FREE `gh api rate_limit` endpoint
  (which consumes no quota) and wait, bounded by
  `FORGE_CLI_RATE_LIMIT_MAX_WAIT_SECS`, for `resources.graphql.remaining` to
  recover before issuing the call; a `backend_rate_limited` failure then
  triggers one wait-and-retry. Centralizing runner construction keeps the
  budget classifier and the wiring from drifting: a newly-added op cannot ship
  ungated, and a guard test rejects an op that constructs a bare runner. REST
  calls (`gh api repos/…`), the probe itself, and non-GitHub backends are never
  gated. Each probe is bound to the triggering call's resolved authority, and
  cached budget readings are keyed by that canonical authority; a reading or
  invalidation for one GitHub host is never reused for another. The gate is
  best-effort: an unreadable probe never blocks real work.

## Provider detection

Provider resolution binds a provider, canonical authority, and optional
repository path before an operation builds backend argv:

1. `--provider` plus `--host` binds both explicitly. A syntactically valid
   custom authority is accepted unless it is positively classified as the
   other provider.
2. `--host` without `--provider` must classify as GitHub or GitLab.
3. `--provider` without `--host` uses an existing selected remote when that
   remote exposes a valid authority positively classified as the selected
   provider. A conflicting ambient remote is ignored in favor of the provider
   default for repository-independent commands and for repository-scoped
   commands with an explicit `--repo`. A repository-scoped command without
   `--repo` still relies on ambient remote authority, so a mismatched,
   malformed, or unclassified existing remote fails closed. Custom authorities
   always require `--host`.
4. With neither flag, the selected remote authority must classify as GitHub or
   GitLab; unknown authorities return `USAGE 64` with
   `error.kind=provider_unsupported`. Provider-resolution diagnostics never
   echo the raw remote URL or its userinfo.

Classification trusts only controlled provider names/suffixes (`github.com`,
`*.github.com`, `*.ghe.com`, `gitlab.com`, and `*.gitlab.com`); branding-like
prefixes such as `gitlab.attacker.example` are not trusted. `ssh.github.com`
canonicalizes to `github.com`, and `altssh.gitlab.com` to `gitlab.com`.
Non-default HTTPS ports remain part of the selected authority; SSH transport
ports are not treated as API ports.

For a repository-scoped command, explicit `--host` also requires an explicit
`--repo` or a repository derived from a remote with the same canonical
authority; otherwise resolution returns `repo_required` before backend
execution. Provider repository shapes are validated as described under Global
flags. Repository-independent commands retain any repository input for
context but skip its shape validation.

## Migration plan: agent-runtime-kit skills → forge-cli

This is the v1 acceptance target. Every row below MUST be reachable
through `forge-cli` before agent-runtime-kit can adopt the new CLI:

| agent-runtime-kit skill                        | forge-cli op                                            |
| ---------------------------------------------- | ------------------------------------------------------- |
| `create-github-pr` / `create-feature-pr`       | `forge-cli pr create --kind feature`                    |
| `create-bug-pr`                                | `forge-cli pr create --kind bug`                        |
| `close-feature-pr` / `close-github-pr` feature | `forge-cli pr merge` (+ optional `pr ready`)            |
| `close-bug-pr`                                 | `forge-cli pr merge`                                    |
| `deliver-feature-pr` / `deliver-github-pr`     | `forge-cli pr deliver --kind feature`                   |
| `deliver-bug-pr`                               | `forge-cli pr deliver --kind bug`                       |
| `gh-fix-ci`                                    | `forge-cli pr wait-checks` + skill's fix-and-push loop  |
| code-review outcome posting                    | `forge-cli pr review`                                   |
| `example:create-feature-mr` / `-bug-mr`        | `forge-cli pr create` (provider auto-detected gitlab)   |
| `example:close-*-mr` / `deliver-*-mr`          | same as github counterparts                             |
| `issue-lifecycle`                              | `forge-cli issue create|view|edit|comment|close|reopen` |
| `issue-follow-up`                              | `forge-cli issue create` (+ subsequent comments)        |

Skills keep their bash shells. The shells:

- Resolve / construct branch names, body content, ticket slugs.
- Call `forge-cli`.
- React to `forge-cli`'s envelope (parse `error.kind`, retry on
  `UNAVAILABLE`, surface `policy` violations to the user verbatim).
- Handle local git operations: `git push`, `git checkout`, branch
  deletion locally, post-merge cleanup.

The shells stop wrapping `gh` / `glab` directly. The `gh pr create …`
and `glab mr create …` invocations are removed.

## Testing strategy

- **Atom unit tests.** Each op has a unit test pair: one against a
  recorded `gh --json …` fixture and one against a recorded `glab …
  -F json` fixture. Fixtures live under
  `crates/forge-cli/tests/fixtures/{github,gitlab}/<op>/`.
- **Exit-code matrix.** Per workspace policy, every binary ships an
  exit-code matrix test covering `success`, `usage`, `data`, and
  `runtime` paths. `forge-cli` extends it to cover `unavailable`
  (forced via `FORGE_CLI_GH_BIN=/bin/false`) and one `software` path
  (mangled fixture).
- **Parity test.** A single test harness drives both backends through
  the same op + the same logical input, then asserts the envelope is
  byte-identical except for `data.provider` and `data.url` host.
- **Dry-run smoke.** Every op supports `--dry-run`; integration tests
  use `--dry-run` to verify the constructed backend argv without
  actually contacting `github.com` or `gitlab.com`.
- **End-to-end is opt-in.** Tests that hit real GitHub / GitLab are
  gated behind `FORGE_CLI_E2E=1` and a designated sandbox repo.
  Default CI does not run them.

## Open questions / v2 candidates

- Releases (`gh release create / view / edit`) — GitLab requires
  glab + tag flow; could fit a `forge-cli release …` tree later.
- `gh api` passthrough — explicitly out of v1 because it defeats the
  lock-down value. Re-evaluate if a real workflow needs a non-CRUD
  call (e.g. CODEOWNERS, branch protection) and only then.
- Repo creation (`gh repo create`) — out of v1; rarely called from
  skills.
- Issue macros (`issue deliver`, `issue close-when-prs-merged`,
  `issue cross-link`) — depend on plan-issue / dispatch-pr-review
  skills converging first; tracked separately.
- Gitea / Forgejo backend — would require a new third backend or a
  Forge-API client. Deliberately deferred until a concrete user
  surfaces.
