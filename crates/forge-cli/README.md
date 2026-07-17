# forge-cli

Provider-neutral CLI for remote forge operations (personal inbox discovery,
PR/MR lifecycle, Issue lifecycle, CI wait, and repository label catalog
maintenance). Two backends ship together: GitHub (wraps `gh`) and GitLab
(wraps `glab`). Adopts `cli-output-contract-v1` from day one.

## Read first

- Docs index: [docs/README.md](docs/README.md)
- Contract: [docs/specs/forge-cli-spec-v1.md](docs/specs/forge-cli-spec-v1.md)
- Op catalog: [docs/specs/forge-cli-ops-v1.yaml](docs/specs/forge-cli-ops-v1.yaml)
- Workspace envelope contract:
  [`/docs/specs/cli-output-contract-v1.md`](../../docs/specs/cli-output-contract-v1.md)

## Quick start

```sh
cargo run -p nils-forge-cli -- --help
cargo run -p nils-forge-cli -- inbox status --format json
cargo run -p nils-forge-cli -- auth status --format json
cargo run -p nils-forge-cli -- label audit --catalog labels.yaml --format json
cargo run -p nils-forge-cli -- pr deliver --kind feature --dry-run --format json
cargo run -p nils-forge-cli -- pr review 123 --decision comments-only --comment-file review.md --mirror-issue --issue 456 --format json
cargo run -p nils-forge-cli -- pr review validate --comment-file review.md --thread-file review-threads.json --format json
cargo run -p nils-forge-cli -- pr review validate 123 --check-diff --comment-file review.md --thread-file review-threads.json --format json
cargo run -p nils-forge-cli -- pr review 123 --decision comments-only --submit-review --expected-head <sha> --comment-file review.md --thread-file review-threads.json --format json
cargo run -p nils-forge-cli -- pr reviews 123 --format json
cargo run -p nils-forge-cli -- pr pending-review delete 123 --review PRR_pending --dry-run --format json
cargo run -p nils-forge-cli -- pr merge 123 --review-convergence --format json
cargo run -p nils-forge-cli -- repo push-default --expected-base <sha> --reason-file reason.md --dry-run --format json
```

`repo push-default` is a narrow, policy-driven exception to PR delivery. It
requires a clean non-default checkout whose `HEAD` is exactly one locally
verified signed commit ahead of `--expected-base`; proves fast-forward ancestry;
uses an exact-old-object lease as a compare-and-swap; and verifies the remote
SHA afterward. The selected remote must expose exactly one push URL, and that
actual destination must match the provider repository; all remote reads and the
push are pinned to that URL. HTTP(S) userinfo and any second-stage Git URL
rewrite are rejected, including empty rewrite prefixes that match every URL.
Release builds fix the Git executable, timeout, and
capture cap; provider metadata and every Git subprocess are bounded. The
command exposes no caller-controlled force mode. Callers remain responsible for
obtaining explicit user authorization and recording it in a regular
`--reason-file`.

`--thread-file` is for actionable findings only: max 50 threads, 16 KiB body
each. Use `pr review validate` for local schema/privacy checks, and add
`--check-diff` with a PR id when you want GitHub changed-file/line validation
before posting. Put non-blocking notes in the review body.

`forge-cli` does NOT introduce a `--json` boolean flag. Use
`--format text|json` exclusively.

Native review convergence is compatibility-preserving and off by default.
Enable it per invocation with `--review-convergence`, per repository in
`.forge-cli.toml`, or in the user-global config. The first `observed` bot mode
never waits for a bot that has not submitted a review. Once relevant
current-head review activity exists, it waits for the configured quiet period,
reports bounded native review summaries, and blocks native
`CHANGES_REQUESTED`. The complete paginated review snapshot is read again
immediately before merge; partial provider data (including a review without a
commit OID) or late review activity fails
closed, and the initial non-empty provider head is bound through the final
merge compare-and-swap. GitHub is the only supported provider in v1; enabled
GitLab dry-runs fail with the same `provider_unsupported` result as live runs.
Merge and deliver dry-run envelopes expose the resolved policy under
`data.review_convergence`. Existing unresolved-thread enforcement remains an
independent merge gate. See the contract for config precedence, duration
bounds, and the JSON snapshot.

Provider-valid pending reviews are listed separately under
`pr reviews data.pending_reviews`; they are not submitted review activity. To
recover a stuck draft, copy its `PRR_...` id into `pr pending-review delete`.
The command verifies PR membership, `PENDING` state, provider-native
`viewerDidAuthor`, and `viewerCanDelete` before deleting that exact node. It
works for GitHub App installation actors without relying on the user-only
`GET /user` endpoint.

`pr review --submit-review` requires `--expected-head <sha>`, performs the same
pending-only ownership preflight, and compares the provider head before any
native review mutation. A viewer-owned draft returns the typed
`github_pending_review_exists` error with the provider head and counts; head
drift returns `github_review_head_changed`; drafts owned by other viewers remain
non-blocking. The expected head is also bound to the provider mutation.

## Inbox discovery

`forge-cli inbox` is a read-only personal work inbox for agents, scheduled jobs,
and Alfred-style consumers:

```sh
forge-cli inbox list --format json
forge-cli inbox status --provider gitlab --gitlab-host gitlab.example.com --format json
forge-cli inbox next --limit 5 --format json
```

With no `--provider`, inbox queries GitHub and GitLab and keeps successful
provider results when another provider fails. GitLab inbox calls always pass
`--hostname <host>` to `glab api`; set `FORGE_CLI_INBOX_GITLAB_HOST` for a
default self-managed host, or use `--gitlab-host` for a per-command override.
`status` reports bounded counts, and `next` returns a ranked bounded subset
without mutating PRs, issues, merge requests, or todos.

For VPN-dependent GitLab hosts, keep daily mixed-provider usage responsive by
requiring a readiness check and bounding GitLab backend calls:

```sh
forge-cli inbox list --format json \
  --gitlab-host gitlab.example.com \
  --gitlab-vpn required \
  --gitlab-vpn-check tcp:gitlab.example.com:443 \
  --provider-timeout 20s
```

When the VPN check fails, mixed-provider mode still returns GitHub results with
a GitLab `vpn_unavailable` provider row and warning. `--provider github`
intentionally skips GitLab. `--provider gitlab` fails when GitLab is selected
but VPN-unavailable or timed out. Add `--strict-providers` for automation that
must fail any partial provider failure.

`--gitlab-vpn-check cmd:<program>` delegates readiness to a local script, and
`--gitlab-vpn-check openvpn` verifies local OpenVPN CLI/profile prerequisites
without starting or stopping VPN. OpenVPN profile paths are local-only
configuration and are redacted from JSON, warnings, issue records, docs, and
cache files. Install optional OpenVPN CLI support with `brew install openvpn`.

Successful provider reads write local cache snapshots. Stale fallback is
opt-in:

```sh
forge-cli inbox list --format json --cache-fallback --cache-max-age 30m
```

Cached fallback items are marked with `stale` metadata and the provider row
remains `ok=false`, so consumers can distinguish stale context from live data.

## Activity discovery

Personal activity commands (`activity commits`, `activity events`, and
`activity summary`) report GitHub user activity. `activity feed` is
repository/project-scoped and supports GitHub plus GitLab:

```sh
forge-cli --provider github --repo owner/name activity feed --since 2026-06-01 --format json
forge-cli --provider gitlab --repo group/project activity feed --since 2026-06-01 --format json
```

Feed rows expose common `kind` / `action` fields for scanning and keep
provider-native semantics in `provider_event_type` plus `details`, so GitHub
and GitLab event differences are preserved instead of flattened.

### Reason filter (`--kind`) vs item-type filter (`--item-type`)

`--kind` selects inbox *reasons* — why an item should appear (`review`,
`assigned`, `todo`, `authored`, `involved`). `--item-type` selects *result
classes* — pull/merge requests, issues, or all items. They are independent:

```sh
# default: all reasons, all item types
forge-cli inbox list --format json

# pull/merge requests only (skips GitHub issue searches and GitLab issue API calls)
forge-cli inbox list --item-type pr --format json

# issues only (skips PR searches; GitHub review-requested is dropped)
forge-cli inbox list --item-type issue --format json

# review-requested PRs only
forge-cli inbox list --kind review --item-type pr --format json
```

`--item-type` defaults to `all`. Dry-run output reflects the pruned query plan:

```sh
forge-cli --dry-run --format json inbox list --item-type pr
```

GitLab `todos` are classified by `target_type` (or the target URL); todos whose
target cannot be classified appear only in `--item-type all` mode.

## Label catalog operations

`forge-cli label` keeps provider labels aligned with a caller-owned YAML/JSON
catalog. The catalog remains outside `nils-cli`; `forge-cli` only validates,
audits, and applies the provider operations.

```sh
forge-cli label list --format json
forge-cli label audit --catalog manifests/forge-labels.yaml --format json
forge-cli --dry-run label ensure --catalog manifests/forge-labels.yaml --update-existing --format json
```

`label audit` reports missing catalog labels, color / description drift, and
unknown shared labels. `label ensure` creates missing labels and updates
existing color / description drift only with `--update-existing`; it never
deletes labels or renames labels by default.

`pr create` and `pr deliver` accept repeated `--label <name>` flags. Add
`--label-catalog <path> --strict-labels` when the caller wants `forge-cli` to
reject unknown, not-applicable, or mutually exclusive labels before a PR/MR is
opened.

### Latency notes

Provider adapters and independent query families run concurrently, so
default-mode latency is bounded by the slowest single backend call rather than
their sum. Identity lookup is only issued when a remaining GitLab query needs
it. Manual smoke timings (provider/network dependent, not a CI assertion):

```sh
time forge-cli --provider github --format json inbox list --limit 30
time forge-cli --provider github --format json inbox list --limit 30 --item-type pr
time forge-cli --provider gitlab --gitlab-host gitlab.example.com --format json inbox list --limit 30
time forge-cli --format json inbox list --gitlab-host gitlab.example.com --limit 30
```

Wall-clock latency depends on `gh`/`glab` and remote API responsiveness; treat
these timings as delivery evidence, not deterministic budgets.

## Search

`forge-cli search` runs free-text and reverse-reference queries the structured
`issue list` / `pr list` filters cannot express. It delegates to the provider
search primitives and builds no index. GitHub-only in v1; GitLab and Local
return a structured `provider_unsupported` error, never a silent empty result.

```sh
# Full-text issues / PRs (default --match title,body,comments), single-repo scoped
forge-cli search issues "ratelimit retry" --format json
forge-cli search prs "cache" --match title --limit 10 --format json

# Reverse reference: which issues/PRs reference this ref?
forge-cli search refs-to 123 --format json
forge-cli search refs-to owner/name#123 --format json
forge-cli search refs-to https://github.com/owner/name/pull/123 --format json

# Preview the exact backend argv without calling the provider
forge-cli --dry-run --format json search issues "term"
```

Role split: `issue list` / `pr list` filter by structured fields within one
repo, `inbox` is the personal cross-repo work queue, and `search` is full-text
(`issues` / `prs`) and reverse-reference (`refs-to`) query. The repo slug comes
from `--repo owner/name` or the detected remote. `search issues` / `search prs`
emit `cli.forge-cli.search.issues.v1` / `...search.prs.v1`; `search refs-to`
emits `cli.forge-cli.search.refs-to.v1`. Every hit is the shared `SearchItem`
(`kind`, `number`, `url`, `title`, `state`, `repo`, `matched_field`).

## GitHub checks compatibility

Starting with `nils-cli` `0.17.0`, GitHub `pr checks` calls request only the
`gh 2.92.0` supported JSON fields. Required-check gates use an explicit
`gh pr checks --required` snapshot instead of the removed `isRequired` JSON
field, so `pr checks`, `pr wait-checks`, `pr merge`, and `pr deliver` share the
same compatibility path. If `gh pr checks` fails on a GitHub
`statusCheckRollup` permission traversal, `forge-cli` falls back to
`gh pr view --json headRefOid,statusCheckRollup` and returns the readable
head-SHA rollup rows instead of surfacing a backend error. If that fallback
cannot recover required-check classification, required-only snapshots fail
closed by gating every readable row, synthesize a pending required row when the
readable rollup is empty, and include
`github_status_rollup_requiredness_unknown_all_rows_gated` in `data.warnings[]`.

## GitLab MR delivery compatibility

GitLab MR delivery uses structured API data where `glab` subcommands expose
stable API access but not stable text output:

- `pr checks <iid>` and `pr wait-checks <iid>` read `glab mr view -F json`,
  then `glab api --hostname <host> projects/<project>/pipelines/<id>/jobs` for
  job rows. This path is not blocked by `glab --version` parser ranges.
- Branch-only check snapshots without a repo/project path still use the
  `glab ci status -b <branch>` text-parser fallback and keep the `glab`
  version guard.
- `pr merge <iid>` keeps the existing clean-worktree, draft, default-branch,
  merge-method, branch-cleanup, and required-check gates, then performs the
  GitLab mutation through `glab api --method PUT .../merge` with the MR head
  `sha` when available.
- `pr deliver` inherits the same GitLab checks/wait/merge atoms; there is no
  separate GitLab macro.

## Deterministic linked-issue closeout

After a successful merge, `pr deliver` runs one more step — `issue_closeout` —
that closes every issue the PR references through a `Closes/Fixes #N` closing
keyword. GitHub records those references on the PR as `closingIssuesReferences`
(also surfaced on `pr view` as `closing_issue_refs`), and normally auto-closes
them on merge. That auto-close is **asynchronous**: it can lag the merge by more
than the few seconds a delivery flow takes to check the issue, so a post-merge
"is it closed yet?" probe frequently sees the issue still `OPEN` even though the
link is correct (sympoies/nils-cli#1052). The closeout step removes that
ambiguity by, for each still-open referenced issue, issuing one explicit,
idempotent `issue close --reason completed`. It is a determinism layer over
GitHub's eventually-consistent auto-close, not a workaround for a broken link.

- The step reports one outcome per issue: `closed` (it was open and we closed
  it), `already_closed` (GitHub's auto-close — or a manual close — already
  landed; left untouched), or `error` (the state check or close failed).
- It is **best-effort**: the merge has already landed, so a fetch/close failure
  records an `ok:false` step but never fails the delivery.
- It only ever acts on genuine closing keywords. Plan-tracking / dispatch flows
  that link issues with a non-closing `Refs #N` produce an empty
  `closingIssuesReferences`, so they are untouched and still close through
  `plan-issue record close`.
- GitLab is a no-op today: `glab mr view` does not expose the closes-issues
  connection, so `closing_issue_refs` is always empty there.
- Pass `--no-issue-closeout` to skip the step entirely.
