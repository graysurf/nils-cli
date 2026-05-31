# Local Provider Contract v1

## Status

- This spec defines the binding on-disk store format and per-method behaviour
  for the future `forge-cli` `Provider::Local` file-backed backend.
- It promotes the capability split, JSON store schema, and per-method contract
  from the "provider-neutral + local backend" scoping spike into a durable,
  normative crate-local spec.
- It is normative for two consumers that share the same on-disk contract:
  1. `forge-cli` `Provider::Local` — the in-process, file-backed backend that
     reads and writes this store (rollout Task 2.1).
  2. The plan-tracking e2e driver's `driver-writes-JSON` seeding path — the
     driver stages `prs/<n>.json` records directly into the store in the
     documented format, then asserts them back through
     `forge-cli … --provider local` (rollout Tasks 1.1+).
- Tracked by the `provider-neutral + local backend` rollout
  ([sympoies/nils-cli#696](https://github.com/sympoies/nils-cli/issues/696)).

## Scope

In scope:

- The capability split: which `ProviderAdapter` methods can be implemented
  faithfully against a local file store (REAL) versus which can only return
  test-seeded values (STUB / SEED).
- The on-disk JSON store schema (`RepoFile`, `IssueRecord`, `PrRecord`).
- The per-method contract for the 11 `ProviderAdapter` methods, against the
  as-built trait signatures.
- Determinism rules for synthesized URLs, timestamps, and numbering.
- The store layout on disk and its teardown model.

Out of scope:

- The `forge-cli` `Provider::Local` implementation itself (Task 2.1). This spec
  is the contract it must satisfy, not the implementation.
- `plan-issue-cli` local routing — parameterizing the hardcoded
  `--provider gitlab` base args at
  `crates/plan-issue-cli/src/forge_cli_adapter.rs:127` (Task 2.2).
- The cross-provider conformance suite scope (Task 3.1). This spec defines the
  shape the local backend must conform to; it does not enumerate the shared
  scenario table.

(The store-locator surface — flag, env, and `--repo` slug shape — was an open
question in the source spike; it is resolved below in
[Store Locator](#store-locator).)

## Source of Truth (as-built)

The schema below is anchored to the as-built provider boundary, not to any
earlier design draft. The authoritative code locations are:

| What | Where |
| --- | --- |
| `ProviderAdapter` trait (11 methods, `repo: &str`) | `crates/plan-issue-cli/src/github.rs:10` (re-exported from `crates/plan-issue-cli/src/provider.rs:24`) |
| `PrMergeSummary` struct | `crates/plan-issue-cli/src/github.rs:69` |
| `CloseReason` enum (`Completed` \| `NotPlanned`) | `crates/plan-issue-cli/src/commands/plan.rs:9` |
| `Provider` / `Repo` / `select_adapter` / `resolve_repo` | `crates/plan-issue-cli/src/provider.rs:30,52,142,111` |
| GitHub impl (`GhCliAdapter`, shells `gh`) | `crates/plan-issue-cli/src/github.rs` |
| GitLab impl (`ForgeCliAdapter`, shells `forge-cli`) | `crates/plan-issue-cli/src/forge_cli_adapter.rs` |
| Provider-capability contract + third-provider recipe | `crates/plan-issue-cli/docs/runbooks/provider-routing-runbook.md` §3, §5 |

The trait takes `repo: &str` (the slug) and has no `provider()` method; call
sites pass `&repo.slug`. The provider-routing runbook §4.1 was reconciled to
this shape alongside this spec.

## The Capability Split

The 11 `ProviderAdapter` methods cleave into two halves. This split is the
single decision that bounds how faithful a local backend (and any future
networked service derived from it) can be.

### Half A — issue / timeline (8 methods): REAL locally

For these methods the local store *is* the source of truth, so the backend can
implement them faithfully. A future plan/issue-tracking service is literally
"this store, networked and persisted".

`create_issue` · `issue_body` · `issue_evidence` · `list_open_tracker_issues`
· `edit_issue_body` · `comment_issue` · `edit_issue_labels` · `close_issue`

### Half B — PR / merge / CI (3 methods): STUB / SEED only

There is no real VCS or CI behind a local store, so these methods can only
return what a test *seeded*. This is sufficient for e2e — the lifecycle flow
never *creates* a PR through the adapter, it only *reads* PR state — but Half B
does not naturally grow into a service unless a real VCS/CI is wired behind it.
The realistic "own service" is therefore an issue/plan-tracking service; the PR
half stays a test double (or later integrates with whatever VCS is actually
used).

`pr_is_merged` · `pr_merge_summary` · `pr_comments`

Because Half B is seeded, the local backend is especially useful for edge-case
tests (merged-but-checks-red, required-unknown, zero-required-checks, …) that
are awkward to provoke against a real provider.

## On-Disk Store Schema

### Store layout

The store is file-backed JSON under a single root. The root is a hermetic
temp directory per test run; teardown is `rm -rf <store-root>` (versus the
current `gh api` branch/issue wiping the driver performs against GitHub).

```text
<store-root>/
  repo.json
  issues/<n>.json
  prs/<n>.json
```

### `RepoFile` (`repo.json`) — store metadata

```json
{
  "slug": "<name>",
  "provider": "local",
  "next_issue": 13,
  "next_pr": 8,
  "clock": 42
}
```

`next_issue` and `next_pr` are monotonic allocation counters. `clock` is the
monotonic timestamp counter (seconds past the deterministic base) consumed one
tick per synthesized `created_at` (see [Determinism](#determinism)). All three
are owned by `forge-cli`; the driver never writes `repo.json`.

### `IssueRecord` (`issues/<n>.json`) — Half A, authoritative

```json
{
  "number": 12,
  "title": "Plan: <title>",
  "body": "<issue body markdown>",
  "labels": ["plan", "..."],
  "state": "open",
  "close_reason": null,
  "comments": [
    {
      "id": 1,
      "body": "<comment markdown>",
      "author": "local",
      "created_at": "2026-01-01T00:00:00Z",
      "url": "local://<slug>/issues/12#comment-1"
    }
  ]
}
```

Field semantics:

- `state`: `"open"` or `"closed"`.
- `close_reason`: `null` while open; on close, `"completed"` or
  `"not-planned"`, mapping directly to `CloseReason::Completed` /
  `CloseReason::NotPlanned`. The local backend owns its own schema, so it stores
  the reason natively — no GitLab-style comment-prefix hack is needed.
- `comments[].created_at`: a deterministic ISO-8601 timestamp (see
  [Determinism](#determinism)).
- `comments[].url`: the synthetic `local://` URL the backend returns from
  `comment_issue` and later re-scans during resolve-approval, so the scheme
  must round-trip.

### `PrRecord` (`prs/<n>.json`) — Half B, all fields seeded

`PrRecord` mirrors `PrMergeSummary` (`crates/plan-issue-cli/src/github.rs:69`)
plus a comment stream. Because Half B is a stub, **every field here is seeded
by the test, never derived** from real VCS/CI state.

```json
{
  "number": 7,
  "state": "MERGED",
  "merged": true,
  "merge_sha": "0000000000000000000000000000000000000000",
  "checks": "success",
  "required_state": "success",
  "required_count": 0,
  "non_required_failures": [],
  "comments": [
    {
      "body": "...",
      "html_url": "local://<slug>/pull/7#comment-1",
      "author": "...",
      "created_at": "2026-01-01T00:00:00Z"
    }
  ]
}
```

Field semantics are copied verbatim from the `PrMergeSummary` doc comments and
must be honoured by both the driver (when seeding) and `forge-cli` (when
reading):

- `state`: raw GitHub-style PR state — `"MERGED"`, `"OPEN"`, or `"CLOSED"`.
- `merged`: backs `pr_is_merged`.
- `merge_sha`: the merge commit SHA. Absent / `null` means "not merged" for the
  strict close gate (which treats a missing `merge_commit_sha` as
  `linked-pr-not-merged`).
- `checks`: rolled-up status-check state — `"success"`, `"failure"`,
  `"pending"`, `"error"`, or `null`.
- `required_state`: required-check rollup state when classifiable; `null` means
  "the adapter could not classify required vs. non-required", and the close gate
  falls back to `checks`.
- `required_count`: number of required checks. `null` means classification is
  unavailable; `Some(0)` / `0` means zero required checks were declared.
- `non_required_failures`: names of non-required checks that ended in a
  failure-class state; informational evidence only — the close gate never blocks
  on this alone.
- `comments[]`: at minimum `body` and `html_url` (the keys `pr_comments`
  guarantees); `author` and `created_at` are recommended for parity with the
  real providers' comment stream.

## Per-Method Contract

The table is ordered to match the trait definition in
`crates/plan-issue-cli/src/github.rs:10`. Every signature takes `repo: &str`
(the slug) as its first argument.

| # | Method | Returns | Local implementation | Kind |
| --- | --- | --- | --- | --- |
| 1 | `issue_body(repo, issue)` | `String` | read `issues/<n>.json` `.body` | REAL |
| 2 | `issue_evidence(repo, issue)` | `(String, String)` = (body, comments_json) | read `.body` + serialize `.comments` into the `gh issue view --json comments` shape | REAL |
| 3 | `list_open_tracker_issues(repo, labels)` | `Vec<u64>` | scan `issues/`, keep `state==open` AND `labels ⊇ requested` (AND semantics; an empty slice lists every open issue) | REAL |
| 4 | `create_issue(repo, title, body_file, labels)` | `(u64, String)` = (number, url) | alloc `next_issue`, write `issues/<n>.json`, return `(n, synthetic url)` | REAL |
| 5 | `edit_issue_body(repo, issue, body_file)` | `()` | overwrite `.body` | REAL |
| 6 | `comment_issue(repo, issue, body_file)` | `String` = comment url | append to `.comments`, return its `local://` url | REAL |
| 7 | `edit_issue_labels(repo, issue, add, remove)` | `()` | mutate the `.labels` set (add then remove) | REAL |
| 8 | `close_issue(repo, issue, reason, comment)` | `()` | set `state=closed`, store `close_reason` natively, append optional close comment | REAL |
| 9 | `pr_is_merged(repo, pr)` | `bool` | read seeded `prs/<n>.json` `.merged` | STUB |
| 10 | `pr_merge_summary(repo, pr)` | `PrMergeSummary` | read seeded `PrRecord` into the struct | STUB |
| 11 | `pr_comments(repo, pr)` | `Vec<Value>` (≥ `body`, `html_url`) | read seeded `.comments` | STUB |

Notes:

- Method 2 (`issue_evidence`) must produce `comments_json` in the same shape the
  GitHub adapter gets from `gh issue view --json comments`, because the
  `record audit` fixture parser consumes that shape unchanged across providers.
- Method 4 (`create_issue`) returns the number first because callers store the
  returned number as the issue identity for the rest of the lifecycle.
- Method 8 (`close_issue`) takes a `CloseReason` and an optional close comment;
  the local backend persists the reason in `close_reason` rather than encoding
  it in a comment.

## Determinism

Real providers assign URLs and timestamps server-side. The local backend must
synthesize them **deterministically** — never from the wall clock — so golden
and conformance tests are reproducible.

- **URLs**: `local://<slug>/issues/<n>#comment-<k>` for issue comments and
  `local://<slug>/pull/<n>#comment-<k>` for PR comments. The scheme is stable
  and parseable: the lifecycle flow stores a `comment_issue` URL and later scans
  for it during resolve-approval, so the scheme must round-trip.
- **Timestamps**: a seeded monotonic clock injected at backend construction
  (for example, start at `2026-01-01T00:00:00Z` and advance by a fixed step
  per write), never the system clock.
- **Numbering**: the `next_issue` / `next_pr` counters in `repo.json` allocate
  issue and PR numbers monotonically.

## Seeding Half B (`driver-writes-JSON` v1)

The lifecycle flow never *creates* a PR through the adapter — it only *reads*
PR state. So the local backend needs a way for a test to stage `PrRecord`s
before `record close` runs.

The v1 seed interface is **driver-writes-JSON**: the e2e driver writes
`prs/<n>.json` directly into the store. The driver already inspects provider
state out-of-band, so this fits its model with zero new CLI surface. Because the
local backend is `forge-cli` `Provider::Local`, the file the driver writes
**must conform to the `PrRecord` schema in this spec** — the driver and
`forge-cli` share this one contract. `forge-cli … pr view` / `pr checks
--provider local` then read back exactly what the driver seeded.

A `seed-pr` convenience command is explicitly deferred (a nicety, not v1).

## Store Locator

`forge-cli` selects the local backend with `--provider local` and resolves the
store root in this order:

1. The `--store-root <path>` global flag.
2. Otherwise the `FORGE_CLI_LOCAL_STORE` environment variable.

A missing store root is a hard error (`UNAVAILABLE`, `error.kind =
local_store_unconfigured`) — the backend never falls back to an implicit
location.

The `--repo` slug accepts an optional `local:` scheme prefix (`--repo
local:<name>`); the prefix is stripped and the bare `<name>` is recorded as the
store's `slug` in `repo.json` and embedded in synthetic `local://<name>/…`
URLs. When `--repo` is omitted the slug defaults to `local`. One store root
holds exactly one repository.

`forge-cli` models only the commands this contract covers under
`--provider local` — the issue lifecycle (`issue create / view / list / edit /
comment / close`) and the PR read surface (`pr view / checks / comments`). Any
other command (PR mutation, `repo` / `auth` / `label` / `inbox`, the
`pr deliver` macro) is rejected up front with a `provider_unsupported` error
rather than reaching a backend.

## Relationship to `forge-cli` and the Driver

This contract is the seam that keeps the rollout's two halves uniform:

- **`forge-cli` `Provider::Local`** (Task 2.1) implements this store: the Half A
  methods read/write `repo.json` + `issues/<n>.json` as the source of truth, and
  the Half B methods read seeded `prs/<n>.json`. The meat of the local backend
  lives in `forge-cli`, riding the rail `plan-issue-cli` already uses for GitLab
  via `ForgeCliAdapter`.
- **`plan-issue-cli` local routing** (Task 2.2) is a small change: parameterize
  the hardcoded `--provider gitlab` base args at
  `crates/plan-issue-cli/src/forge_cli_adapter.rs:127` so the same forge-routed
  adapter can emit `--provider local`. No second hand-written adapter is needed.
- **The driver** seeds Half B by writing `PrRecord` JSON (above) and asserts all
  three providers uniformly through `forge-cli … --provider local`, with no
  special-case JSON reads in the assert layer.

## Alignment with the Provider-Routing Runbook

The local backend is the "adding a third provider" exercise from
`provider-routing-runbook.md` §5, with two deliberate divergences:

1. **In-process, not shell-out.** §5 assumes a thin adapter shelling out to a
   provider binary. Local has no binary — `forge-cli` implements the backend
   directly against the file store. Simpler, faster, fully hermetic.
2. **Explicit selection, not URL detection.** §5 step 3 extends URL pattern
   matching. Local has no remote URL; it is selected by an explicit
   `--provider local` (plus a store-root locator), so `resolve_repo` receives a
   `local:`-prefixed slug rather than via host detection.

Everything else from §5 applies: add a `Provider::Local` variant, register it in
`select_adapter`, add per-method fixture tests, and run the §4.5 validation
checkpoints. Half A is conformance-tested for behaviour against
`{local, github, gitlab}`; Half B is local-seeded, so it is conformance-tested
for *shape*, not behaviour. The fake earns trust only by satisfying the same
contract suite as the real adapters — local is a **complement** to real-provider
e2e, never a replacement.

## Conformance Scenario Subset

The cross-provider conformance harness lives at
`crates/forge-cli/tests/integration/conformance.rs`. Every arm runs the real
`forge-cli` binary end to end: `local` against a hermetic temp store (the real
`LocalRunner`), `github` / `gitlab` against `gh` / `glab` stub scripts wired via
`FORGE_CLI_GH_BIN` / `FORGE_CLI_GLAB_BIN` that echo the JSON the real provider
returns for the equivalent state. Each arm therefore exercises its full
pipeline (`build_*_call` → runner → `parse_*_output` → envelope).

The harness asserts conformance on the **observable** envelope after stripping
the fields that differ *by design*: `provider`, `url` (scheme/host), and a
comment's `url` / `author` / `created_at`. The issue comment timeline is
reduced to its ordered bodies. `assignees` are held empty across the subset
because the local store does not model assignees (a documented boundary, not a
silent gap).

| Scenario | Op | Half | Asserted invariant |
| --- | --- | --- | --- |
| `issue view` open | `issue view` | A behaviour | `{number, state, title, body, labels, assignees}` identical |
| `issue view` closed | `issue close` then `issue view` | A behaviour | normalized `state == closed`, rest identical |
| `issue view --with-comments` | comment ×2 then view | A behaviour | ordered comment bodies identical |
| `issue list` label filter | `issue list --label` (single + AND-pair) | A behaviour | row set `{number, state, title, labels}` identical |
| `issue list` empty | `issue list` (no match) | A behaviour | empty item set on all three |
| `pr view` seeded merged | `pr view` | B shape | same schema + `data` key set; seeded `state == merged`, `merge_commit_sha`, `number` match |
| `pr checks` seeded success | `pr checks` | B shape | same schema + `data` key set; seeded `state == success` |

A negative-control test feeds the `github` arm a drifted title and asserts the
observable diverges, so the equality assertions cannot pass vacuously. Half B is
shape-only by design: the local PR half is seeded, so `title` / `head` / `base`
legitimately differ from the canned real-provider values and are excluded from
the value comparison.
