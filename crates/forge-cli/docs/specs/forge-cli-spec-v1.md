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
  `ready`, `merge`, `close`.
- PR/MR checks: `pr checks` (one-shot snapshot) and `pr wait-checks`
  (blocking poll until terminal).
- Issue lifecycle: `issue create`, `view`, `edit`, `comment`, `close`,
  `reopen`.
- Repository label lifecycle: `label list`, `label audit`, and
  `label ensure`.
- Read-only helpers used by the macros: `auth status`, `repo view`.
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

- Provider is auto-detected from the working tree's remote URL
  (`origin` by default, configurable via `--remote`):
  - `github.com` or matches `gh auth status` hosts → backend `github`,
    subprocess `gh`.
  - `gitlab.com` or matches `glab auth status` hosts → backend
    `gitlab`, subprocess `glab`.
  - Other hosts → `USAGE 64` with `error.kind = "provider_unsupported"`
    and a hint to file a follow-up; v1 does not auto-fall-back to
    HTTPS-only Gitea/Forgejo even though the URL shape would allow it.
- All remote calls go through the backend subprocess. `forge-cli` does
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

| forge-cli op                                | github backend                                                                                                                               | gitlab backend                                              | Parity                                                                                                                   |
| ------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| `pr create`                                 | `gh pr create --draft`                                                                                                                       | `glab mr create --draft`                                    | exact                                                                                                                    |
| `pr view <id>`                              | `gh pr view <id> --json …`                                                                                                                   | `glab mr view <id> -F json`                                 | exact                                                                                                                    |
| `pr list`                                   | `gh pr list --json …`                                                                                                                        | `glab mr list -F json`                                      | exact                                                                                                                    |
| `pr edit <id>`                              | `gh pr edit <id> …`                                                                                                                          | `glab mr update <id> …`                                     | exact                                                                                                                    |
| `pr comment <id>`                           | `gh pr comment <id> --body …`                                                                                                                | `glab mr note <id> --message …`                             | exact                                                                                                                    |
| `pr review <id>`                            | guard `gh api …/pulls/{id}`, then outcome comment, native review, or GraphQL pending review + review thread(s) + submit with `--thread-file` | `glab mr note create <id> --message … --resolvable=false`   | outcome comment, native review event, or resolvable review threads (`--thread-file`, GitHub-only); optional issue mirror |
| `pr review validate [id]`                   | local review body/thread-file validation; with `--check-diff`, `gh api repos/{repo}/pulls/{id}/files`                                        | local validation only; `--check-diff` unsupported in v1     | schema/privacy preflight plus optional GitHub diff-coordinate validation                                                 |
| `pr ready <id>`                             | `gh pr ready <id>`                                                                                                                           | `glab mr update <id> --ready`                               | exact                                                                                                                    |
| `pr review-threads list <id>`               | `gh api graphql` (`reviewThreads` connection)                                                                                                | `glab api …/merge_requests/<iid>/discussions`               | normalized thread state                                                                                                  |
| `pr review-threads resolve <id> --thread …` | `gh api graphql` (`addPullRequestReviewThreadReply` then `resolveReviewThread`)                                                              | unsupported in v1                                           | GitHub-only seam                                                                                                         |
| `pr review-threads reply <id> --thread …`   | `gh api graphql` (`addPullRequestReviewThreadReply`)                                                                                         | unsupported in v1                                           | GitHub-only seam                                                                                                         |
| `pr tasks <id>`                             | `gh pr view <id> --json number,url,body`                                                                                                     | `glab mr view <id> -F json` (`description`)                 | normalized task-list state                                                                                               |
| `pr merge <id>`                             | `gh pr merge <id> --squash --delete-branch`                                                                                                  | `glab api --method PUT .../merge` after gates               | exact (method honoured per repo cfg)                                                                                     |
| `pr close <id>`                             | `gh pr close <id>`                                                                                                                           | `glab mr close <id>`                                        | exact                                                                                                                    |
| `pr checks <id>`                            | `gh pr checks <id> --json …` plus `--required` for gating                                                                                    | `glab mr view -F json` + `glab api .../pipelines/<id>/jobs` | emulated on GitLab                                                                                                       |
| `pr wait-checks <id>`                       | poll `gh pr checks` / `gh pr checks --required`; fall back to readable `statusCheckRollup` rows on rollup permission errors                  | poll structured MR pipeline/jobs snapshot                   | emulated; same envelope                                                                                                  |
| `issue create`                              | `gh issue create …`                                                                                                                          | `glab issue create …`                                       | exact                                                                                                                    |
| `issue view <id>`                           | `gh issue view <id> --json …`                                                                                                                | `glab issue view <id> -F json`                              | exact                                                                                                                    |
| `issue edit <id>`                           | `gh issue edit <id> …`                                                                                                                       | `glab issue update <id> …`                                  | exact                                                                                                                    |
| `issue comment <id>`                        | `gh issue comment <id> --body …`                                                                                                             | `glab issue note <id> --message …`                          | exact                                                                                                                    |
| `issue close <id>`                          | `gh issue close <id>`                                                                                                                        | `glab issue close <id>`                                     | exact                                                                                                                    |
| `issue reopen <id>`                         | `gh issue reopen <id>`                                                                                                                       | `glab issue reopen <id>`                                    | exact                                                                                                                    |
| `label list`                                | `gh label list --json …`                                                                                                                     | `glab label list --output json`                             | exact                                                                                                                    |
| `label audit`                               | read labels, compare with caller catalog                                                                                                     | same                                                        | exact                                                                                                                    |
| `label ensure`                              | `gh label create/edit`                                                                                                                       | `glab label create/edit`                                    | exact (no delete/rename by default)                                                                                      |
| `auth status`                               | `gh auth status`                                                                                                                             | `glab auth status`                                          | exact (text → typed)                                                                                                     |
| `repo view`                                 | `gh repo view --json …`                                                                                                                      | `glab repo view -F json`                                    | exact                                                                                                                    |
| `inbox list`                                | `gh search prs/issues --json …`                                                                                                              | `glab api --hostname <host> …`                              | normalized aggregation                                                                                                   |
| `inbox status`                              | same provider reads as `inbox list`                                                                                                          | same provider reads as `inbox list`                         | bounded counts                                                                                                           |
| `inbox next`                                | same provider reads as `inbox list`                                                                                                          | same provider reads as `inbox list`                         | ranked bounded subset                                                                                                    |
| `activity commits`                          | `gh api search/commits`                                                                                                                      | unsupported in v1                                           | GitHub-only seam                                                                                                         |
| `activity events`                           | `gh api users/<login>/events[/public]`                                                                                                       | unsupported in v1                                           | GitHub-only seam                                                                                                         |
| `activity feed`                             | `gh api repos/<owner>/<repo>/commits` + repository activity REST                                                                             | `glab api projects/:id/repository/commits` + project events | normalized open vocabulary                                                                                               |
| `activity summary`                          | `gh api graphql` contribution query                                                                                                          | unsupported in v1                                           | GitHub-only seam                                                                                                         |
| `pr deliver`                                | macro: `pr list` lookup → `pr create` or adopt → `pr wait-checks` → `pr ready` → `pr merge`                                                  | same composition with gitlab atoms                          | exact (same macro logic on both)                                                                                         |

"emulated" means the backend's native command differs in shape, but
`forge-cli` normalises both into the same `cli.forge-cli.pr.checks.v1`
payload so callers can branch on `data.state` regardless of host.

GitLab capability status:

- Supported with stable JSON/API: PR/MR create, view, list, edit, comment,
  ready, close, checks, wait-checks, merge, deliver; issue create/view/edit,
  comment, close, reopen; label list/audit/ensure; auth status; repo view;
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
│   └── view
├── auth
│   └── status
└── completion              (workspace standard)
```

Global flags (every subcommand):

- `--format text|json` (workspace standard).
- `--remote <name>` (default `origin`).
- `--provider github|gitlab` (override auto-detect).
- `--repo <owner/name>` (override remote-derived repo slug; passed to
  backend's `--repo` / `-R` equivalent).
- `--dry-run` — render the backend command that *would* run plus all
  validation checks, but do not invoke it. Output envelope carries the
  exact argv under `data.plan` for atomic commands or `data.actions[].plan`
  for `label ensure`.

`forge-cli` itself does not expose `--token`, `--host`, or any auth
override. Those belong to `gh`/`glab` and are configured there.

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

- `forge-cli label list --limit <n>` reads provider labels and emits
  `cli.forge-cli.label.list.v1`.
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

- Input: `--head <branch>` (default current branch), `--base <branch>`
  (default repo default branch), `--title <str>`, `--body-file <path>`
  or `--body <str>`, `--kind feature|bug`, `--draft` (default `true`),
  `--reviewer <user>...`, `--label <name>...`,
  `--label-catalog <path>`, `--strict-labels`.
- Validation (see "Lock-down policy" for the full list):
  - branch name MUST match `^(feat|fix)/[a-z0-9][a-z0-9-]{1,63}$` and
    align with `--kind`;
  - title length ≤ 70 chars, no trailing whitespace;
  - body MUST contain non-empty `## Summary` and `## Test plan`
    sections;
  - working tree MUST be clean (`git status --porcelain` empty);
  - resolved head branch MUST be pushed and remote-tracked.
- Output schema: `cli.forge-cli.pr.create.v1`,
  `data = { number, url, head, base, draft, title, kind, provider }`.

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
  falls back to `gh pr view --json headRefOid,statusCheckRollup` and
  treats the readable head-SHA rollup rows as the snapshot. If that
  projection is also permission-blocked, it reads `headRefOid` alone and
  falls back to REST `gh api` commit check-runs plus combined status
  contexts for the same head SHA. If a REST check-runs or combined status
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
  - all required `success` → `ok = true`.
  - any required `failure`/`cancelled`/`timed_out` → `ok = false`,
    exit `RUNTIME 1`, `error.kind = "checks_failed"`.
  - timeout reached → `ok = false`, exit `UNAVAILABLE 69`,
    `error.kind = "checks_timeout"`.
- Output schema: `cli.forge-cli.pr.checks.v1`,
  `data = { state, required_count, success_count, failed:[…], pending:[…], checks:[…], duration_ms, warnings? }`.

### `pr review-threads` (read)

- The `pr review-threads list <id>` read surface emits
  `cli.forge-cli.pr.review-threads.v1`. Each thread now carries an
  `id` handle in addition to its state fields:
  `data.threads[] = { id, resolved, outdated, author, path, created_at,
  url, body }`. On GitHub `id` is the `reviewThreads` node id
  (`PRRT_...`) — the single handle consumed by both write ops below
  (as `threadId` for resolve and `pullRequestReviewThreadId` for
  reply). On GitLab `id` is the discussion id. The field is additive;
  existing consumers are unaffected.
- Text output includes the same thread id on each thread line so terminal users
  can copy the `--thread` value without switching to JSON.
- `--dry-run` emits the planned thread-list backend call without running the
  preliminary PR/MR view lookup or touching the provider network. It resolves
  the repo/project from `--repo` or the configured remote URL.

### `pr review`

- The `pr review <id>` posting surface emits
  `cli.forge-cli.pr.review.v1`. It accepts an already-rendered review outcome
  body via `--comment <text>` or `--comment-file <path>`, plus a
  `--decision comments-only|approve|request-changes` metadata value and
  repeatable `--lens <name>` entries.
- By default `--decision` is recorded in the envelope and generated issue mirror
  body only (the outcome-comment form); it does not call provider-native
  approve/request-changes APIs.
- With `--submit-review` (GitHub-only in v1) the command instead submits a native
  pull request review event: it POSTs `gh api repos/{repo}/pulls/{id}/reviews`
  with `--decision` mapped to the review `event`
  (`comments-only→COMMENT`, `approve→APPROVE`, `request-changes→REQUEST_CHANGES`),
  creating the `#pullrequestreview-` object reported as `data.pr_comment_url`
  with `data.submitted_review = true`. The review is authored by whatever
  identity the inherited `gh` token carries, so a reviewer-bot token (for example
  via `FORGE_BOT_PROFILE`) yields a bot-authored review. A body is required for
  `COMMENT` and `REQUEST_CHANGES` and optional for `APPROVE` (a body-less approve
  omits the `body` field). The same PR-existence guard runs first, and the
  reviews POST is rendered in `--dry-run` as `data.plan`. `--submit-review` on
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
  requires `--submit-review`; omit it for a summary-only review. A live GitHub run first
  looks up the PR node id, creates a pending review, adds each thread with
  `addPullRequestReviewThread`, then publishes the review with
  `submitPullRequestReview`. JSON output includes
  `data.review_threads[] = { id, url, path, line, subject_type }`, where `id` is
  the `PRRT_...` handle consumed by `pr review-threads resolve`. Dry-run output
  includes `data.target_plan`, `data.thread_plan[]`, `data.submit_plan`, and
  `data.planned_review_threads`. If GitHub rejects an individual thread mutation
  with HTTP 422 because the path/line is not commentable on the diff, the command
  returns `github_review_thread_rejected` (`RUNTIME 1`) with the raw backend
  detail and the failed spec index/path/line after attempting pending-review
  cleanup. If any other thread mutation or final review submit fails after the
  pending review is created, the command attempts a best-effort
  `deletePullRequestReview` cleanup before returning the original failure; if
  cleanup also fails, error details include the pending review id/url and cleanup
  failure. Malformed or oversized
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
  `data = { provider, number, decision, submitted_review, pr_comment_url,
  issue_number, issue_comment_url, mirrored, lenses, review_threads? }`.

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
  - required checks all green (re-checked even if `pr wait-checks`
    succeeded earlier — TTL-zero gate);
  - target branch is the repo default branch OR explicitly approved
    via `--allow-non-default-base`;
  - no unresolved review threads OR explicitly bypassed via
    `--allow-unresolved-threads` (bot reviewers post asynchronously, so
    this is re-checked at merge time — the last action);
  - no unchecked task-list items in the description OR explicitly
    bypassed via `--allow-unchecked-tasks` with a recorded
    `--allow-unchecked-tasks-reason` (the description is the delivery
    contract: every `- [ ]` is checked off or rewritten as
    dispositioned before merge);
  - `--method squash|merge|rebase` (default `squash`, configurable
    per repo).
- Post-merge: deletes the remote branch (default `true`, disable via
  `--keep-branch`). GitLab performs the merge mutation through
  `glab api --method PUT projects/<project>/merge_requests/<iid>/merge`
  after all gates pass, including `sha=<head_sha>` when the MR view
  exposes it so the source branch HEAD cannot drift silently between
  checks and merge.
- Output schema: `cli.forge-cli.pr.merge.v1`,
  `data = { number, url, merge_sha, method, deleted_branch }`.

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
  [--no-merge]        # stop after wait-checks; useful in CI
```

Steps:

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
4. `pr create --draft` — atom; validates branch / title / body. Only
   runs when the lookup found nothing.
5. `pr wait-checks` — atom; blocks until terminal.
6. `pr ready` — atom; only if previous step is `success`.
7. `pr merge` — atom; honours `--method` and repo override.
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
6. **Default-branch protection.** `forge-cli` refuses any operation
   that would force-push, delete, or merge directly into the repo
   default branch except via a merged PR/MR. `--allow-non-default-base`
   exists for non-default *base* branches (e.g. release lanes); there
   is no flag to bypass the default-branch force-push refusal.
7. **Draft → ready → merge ordering.** `pr merge` refuses to merge a
   draft. There is no `--merge-as-draft`. Callers MUST run `pr ready`
   first (or use `pr deliver`, which sequences them).
8. **Required-check gating.** `pr merge` re-checks required-check
   state immediately before invoking the backend, even if
   `pr wait-checks` was called earlier in the macro. This is the
   TTL-zero re-check that addresses the
   `github-pr-required-check-gating` operation record.
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
12. **Review-thread gating.** `pr merge` (and the `pr deliver` merge
    step) fetches review threads immediately before invoking the
    backend and refuses to merge while any thread is unresolved
    (GitHub `reviewThreads`, GitLab resolvable discussions). Bypass
    with `--allow-unresolved-threads`. The local provider has no
    thread model and passes trivially. The error `detail` lists each
    unresolved thread (author, file anchor, first line).
13. **Task-list gating.** `pr merge` (and the `pr deliver` merge step)
    parses GFM task-list items out of the PR/MR description fetched at
    merge time and refuses to merge while any `- [ ]` item is
    unchecked (`- [x]` / `- [X]` count as done, GitLab's `- [~]` as
    inapplicable; fenced code blocks are skipped). Bypass with
    `--allow-unchecked-tasks` plus a required
    `--allow-unchecked-tasks-reason`, which is recorded in the merge
    payload as `unchecked_tasks_override_reason`. Providers without a
    body model (local) pass trivially. The error `detail` lists each
    unchecked item (line number, text).
14. **Review-thread write ownership.** `pr review-threads reply` and
    `pr review-threads resolve` verify live GitHub writes by fetching the
    positional PR's review threads and confirming `--thread <id>` is present
    before posting a reply or resolving. `--dry-run` remains offline and skips
    this lookup.

Violations map to `DATA 65` with one of these `data.error.kind` values:

| `error.kind`                | Triggered by rule |
| --------------------------- | ----------------- |
| `branch_name_invalid`       | 1                 |
| `branch_kind_mismatch`      | 1                 |
| `body_missing_summary`      | 2                 |
| `body_missing_test_plan`    | 2                 |
| `title_too_long`            | 3                 |
| `dirty_worktree`            | 4                 |
| `head_not_pushed`           | 5                 |
| `default_branch_protected`  | 6                 |
| `draft_merge_refused`       | 7                 |
| `checks_pending`            | 8                 |
| `checks_failed`             | 8 (`RUNTIME 1`)   |
| `merge_method_unsupported`  | 9                 |
| `keep_branch_conflict`      | 10                |
| `local_path_present`        | 11                |
| `unresolved_review_threads` | 12                |
| `unchecked_task_items`      | 13                |
| `review_thread_pr_mismatch` | 14                |

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
  --limit <n> --json …`; `--match` defaults to `title,body,comments` and is
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

| Constant      | Value | `forge-cli` triggers                                                                                                          |
| ------------- | ----- | ----------------------------------------------------------------------------------------------------------------------------- |
| `SUCCESS`     | `0`   | Op completed; required state achieved.                                                                                        |
| `RUNTIME`     | `1`   | Remote semantic failure: required checks failed, merge conflict, draft already ready.                                         |
| `USAGE`       | `64`  | Bad CLI syntax, unknown subcommand, unsupported provider.                                                                     |
| `DATA`        | `65`  | Lock-down policy violation (any rule above); body parse failure; invalid VPN config.                                          |
| `UNAVAILABLE` | `69`  | `gh`/`glab` missing, auth required, remote 5xx/network error, wait-checks timeout, GitLab VPN probe failure, backend timeout. |
| `SOFTWARE`    | `70`  | Internal invariant violation (backend JSON did not match expected shape).                                                     |

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
```

### Global config layer

The same schema may live in a user-global file at
`${XDG_CONFIG_HOME:-$HOME/.config}/forge-cli/config.toml`. It supplies defaults
beneath the per-repo `.forge-cli.toml`, so a setting (e.g. `[test_first]
require = true` or `[merge] method = "rebase"`) applies across every repo
without duplicating it into each checkout. A missing global file is not an
error. The global layer feeds the sections forge-cli actually consumes from
config today — `[merge]`, `[inbox]`, and `[test_first]`. The `[checks]`,
`[body]`, and `[branch]` keys are parsed (and validated) for
forward-compatibility but are not yet wired into the corresponding command
paths at either layer, so values placed there are accepted but currently
inert.

Resolution order for any setting: explicit flag > repo `.forge-cli.toml` >
global `config.toml` > spec default. Inbox env vars sit between explicit flags
and `.forge-cli.toml`. Unknown keys produce a `warnings[]` entry, not an
error — forward-compatibility for v2 fields.

### `[test_first]` — test-first evidence gate

`require` defaults to `false`; the gate is off unless a repo or the global
config opts in. When it resolves `true`, `pr create` and `pr deliver` (both the
create and adopt paths, and the `--dry-run` preflight) require
`--test-first-evidence <dir>` for `--kind feature` / `bug`. The directory must
hold a verified `test-first-evidence` record — a failing test or an explicit
waiver, plus a passing final validation. `docs` / `chore` / `ci` / `refactor`
kinds are exempt. Failures surface as `test_first_evidence_required`,
`test_first_evidence_incomplete`, or `test_first_evidence_unreadable`
(exit `DATA`).

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

## Provider detection

```text
explicit --provider flag
  ↓ (else)
remote URL parse from `git remote get-url <--remote>`
  ↓
host classification:
  github.com OR matches `gh auth status` host  → github
  gitlab.com OR matches `glab auth status` host → gitlab
  any other host                                → USAGE 64
                                                  error.kind=provider_unsupported
```

A forced `--provider` overrides provider classification only. The host
still resolves from the remote URL when one is available and its host
classifies to the forced provider (self-hosted GitLab / GHE shapes
included); the provider default host (`github.com` / `gitlab.com`) is
used only when no remote resolves or the remote host classifies to a
different provider.

`gh auth status` and `glab auth status` are *cached* per `forge-cli`
invocation (single call per provider, memoised). They are not refreshed
mid-run.

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
