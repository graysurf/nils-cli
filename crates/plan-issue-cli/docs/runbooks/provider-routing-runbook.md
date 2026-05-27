# Plan-Issue CLI provider routing runbook

This is the long-term runbook for the `plan-issue-cli` provider routing layer.
It was promoted from the Sprint 1 design note after the multi-provider routing
work (Sprints 1–4) shipped through sympoies/nils-cli#498, #499, #503, and #505.
For the historical sprint deliberation, see the original design note in the
archived bundle
`agent-plan-archive:plans/github.com/sympoies/nils-cli/2026-05-25-plan-issue-cli-provider-abstraction/`;
this runbook is the durable reference for contributors who maintain or extend
the routing layer.

## 1. Routing strategy

`plan-issue-cli` keeps the GitHub branch on the in-tree `GhCliAdapter` (which
shells out to `gh`) and routes the GitLab branch through `forge-cli` via a peer
`ForgeCliAdapter` that shells out to `forge-cli issue …` / `forge-cli pr …`
with `--output json` and parses the v1 envelope. Both implement the same
`ProviderAdapter` trait, so call sites in `crates/plan-issue-cli/src/execute.rs`
are provider-neutral.

### Why subprocess to `forge-cli` (not library linkage or in-tree `glab`)

1. Routing through `forge-cli` is the workspace decision for cross-provider
   plumbing — `forge-cli` already owns provider detection, repo normalization,
   `iid` ↔ slug handling, pipeline status, and version probing. Re-implementing
   any of that inside `plan-issue-cli` would duplicate logic and create a
   second source of provider truth.
2. The subprocess + JSON envelope pattern is identical in shape to the
   existing `GhCliAdapter` test surface (PATH-prepended stub binaries via
   `nils-test-support::StubBinDir`), which keeps the test ergonomics
   consistent across both adapters.
3. Process count is bounded — a `record open` is roughly five atomic ops, so
   five extra `forge-cli` processes per run is acceptable for a CLI lifecycle
   command. Today's GitHub path opens a comparable number of `gh` processes.
4. Library linkage would still need the same upstream extensions in
   `forge-cli` (see §3) and would add a new workspace dependency edge.
5. An in-tree `GlabCliAdapter` was rejected because it would duplicate
   `forge-cli`'s GitLab adapter and re-implement pipeline status, MR iid
   lookup, and version probing inside `plan-issue-cli`.

Library linkage stays available as a future fallback if subprocess overhead
ever dominates wall time on a tracking-issue run. Because the trait boundary
already isolates provider I/O, swapping the GitLab branch from subprocess to
in-process is a mechanical change.

## 2. Repo and provider detection

Provider detection happens in `crates/plan-issue-cli/src/provider.rs` and must
recognise both GitHub and GitLab remote forms. Recognised patterns include:

- GitHub: `git@github.com:`, `https://github.com/`, `ssh://git@github.com/`,
  and bare `owner/repo`.
- GitLab: `git@gitlab.<host>:`, `https://gitlab.<host>/`,
  `ssh://git@gitlab.<host>/`, and explicit `gitlab.<host>/group/project`
  slugs.

Resolution rules:

1. If `--repo` carries a host segment (for example
   `gitlab.example.com/group/project`), parse the provider from that host.
2. If `--repo` is bare `owner/repo`, default to `Provider::GitHub` so existing
   GitHub workflows are unchanged.
3. Otherwise read `git remote get-url origin` and detect GitHub vs GitLab from
   the URL pattern. This matches `forge-cli`'s auto-detect behaviour and is
   the default when `--repo` is omitted.
4. Normalize the result to `Repo { provider, slug, host: Option<String> }`.

## 3. Required `forge-cli` extensions (contract reference)

The GitLab branch depends on these `forge-cli` capabilities. They are
listed as the authoritative contract reference; any regression here breaks
`plan-issue-cli` lifecycle commands on GitLab.

- **Issue view with comments.** `forge-cli issue view` must expose the comment
  stream — preferred shape is an optional `comments: Vec<IssueCommentSummary>`
  field on the v1 `IssueViewPayload`, gated behind a `--with-comments` flag.
  `plan-issue-cli` needs this on the hot path for `record audit` so that
  body + comment retrieval is a single subprocess hop.
- **Issue close reason.** GitHub's "completed / not planned" close reason is
  carried by the CLI surface. On GitLab there is no native close-reason
  concept, so the routing layer treats `reason` as a comment prefix on
  GitLab and a `--reason` flag on the GitHub backend via a small `forge-cli`
  extension. The trait's `close_issue` keeps `reason` and optional `comment`
  arguments to preserve GitHub behaviour parity.
- **PR merge summary.** `forge-cli pr view` (or a sibling `pr merge-summary`
  atom) must surface `merge_commit_sha` and a rolled-up `checks` status in
  addition to `state` and `merged_at`. The `record close` strict gate depends
  on both fields.
- **PR comment stream.** A `forge-cli pr comments` atom must return
  `{provider, number, comments: [{body, url, author, created_at}]}`. The
  GitHub backend wraps `gh api --paginate /repos/.../issues/<n>/comments`;
  the GitLab backend wraps
  `glab api --paginate /projects/.../merge_requests/<iid>/notes`. The
  `resolve-approval` flow scans this stream for the approval phrase.

## 4. `ProviderAdapter` contract

### 4.1 Trait shape

The provider boundary is the `ProviderAdapter` trait in
`crates/plan-issue-cli/src/provider.rs`. All lifecycle code paths in
`execute.rs` go through this trait — no direct provider CLI shell-out should
appear outside the adapter implementations.

```rust
// crates/plan-issue-cli/src/provider.rs
pub trait ProviderAdapter {
    fn provider(&self) -> Provider;                              // github | gitlab
    fn issue_body(&self, repo: &Repo, issue: u64) -> Result<String, String>;
    fn issue_evidence(&self, repo: &Repo, issue: u64) -> Result<(String, String), String>;
    fn create_issue(&self, repo: &Repo, title: &str, body_file: &Path, labels: &[String])
        -> Result<(u64, String), String>;
    fn edit_issue_body(&self, repo: &Repo, issue: u64, body_file: &Path) -> Result<(), String>;
    fn comment_issue(&self, repo: &Repo, issue: u64, body_file: &Path) -> Result<String, String>;
    fn edit_issue_labels(
        &self, repo: &Repo, issue: u64,
        add: &[String], remove: &[String],
    ) -> Result<(), String>;
    fn close_issue(
        &self, repo: &Repo, issue: u64,
        reason: CloseReason, comment: Option<&str>,
    ) -> Result<(), String>;
    fn pr_is_merged(&self, repo: &Repo, pr: u64) -> Result<bool, String>;
    fn pr_merge_summary(&self, repo: &Repo, pr: u64) -> Result<PrMergeSummary, String>;
    fn pr_comments(&self, repo: &Repo, pr: u64) -> Result<Vec<Value>, String>;
}

pub struct Repo { pub provider: Provider, pub slug: String, pub host: Option<String> }
pub enum Provider { GitHub, GitLab }

pub fn select_adapter(repo: &Repo, force: bool) -> Box<dyn ProviderAdapter> {
    match repo.provider {
        Provider::GitHub => Box::new(GhCliAdapter::new(force)),
        Provider::GitLab => Box::new(ForgeCliAdapter::new(force)),
    }
}
```

`GhCliAdapter` and `ForgeCliAdapter` are the two production implementations.
The GitHub adapter shells out to `gh` directly (preserving the historical
behaviour). The GitLab adapter shells out to `forge-cli` and parses the v1
JSON envelope.

### 4.2 Repo resolution

```rust
// crates/plan-issue-cli/src/provider.rs (continued)
pub fn resolve_repo(repo_override: Option<&str>) -> Result<Repo, String> {
    // 1. If --repo carries a host (gitlab.<host>/group/project), parse provider.
    // 2. Else if --repo is bare owner/repo, default Provider::GitHub.
    // 3. Else read `git remote get-url origin`; detect github vs gitlab from URL pattern.
    // 4. Normalize to (provider, slug, optional host).
}
```

`forge-cli`'s `provider::detect` already covers step 3. `plan-issue-cli` may
either shell out to `forge-cli repo view --output json` and inherit detection
(at the cost of one extra subprocess per CLI run) or keep the pattern matching
local. Either choice is acceptable as long as the four-step rule above is
preserved and remains consistent with `forge-cli` behaviour.

### 4.3 Error mapping

`forge-cli` errors surface as JSON envelopes:

```json
{"status":"error","schema_version":"cli.forge-cli.issue.create.v1","error":{"code":"…","message":"…"}}
```

`ForgeCliAdapter::run_forge` parses the envelope and, on `status=error`,
returns `Err(format!("…"))` propagating the upstream `code` and `message` so
`plan-issue-cli` error messages remain informative for operators.

### 4.4 Behaviour preservation

Existing GitHub callers must see no behaviour change. The routing layer
preserves this by:

- Keeping `GhCliAdapter` unchanged for the GitHub branch.
- Letting `select_adapter` return `GhCliAdapter` for `Provider::GitHub`,
  exercising the same code path as before the routing work landed.
- Wrapping the existing slug `String` in `Repo { provider: GitHub, slug,
  host: None }`, so repo plumbing changes are limited to a mechanical
  `&repo.slug` substitution at trait call sites.

The cwd auto-detect default (when `--repo` is omitted) matches `forge-cli`,
which keeps existing GitHub-first command lines unchanged.

### 4.5 Validation checkpoints

Run these checkpoints any time a provider operation is added, the trait
shape changes, or a new adapter implementation lands:

1. **Audit completeness.** Every provider call site in
   `crates/plan-issue-cli/src/execute.rs` must go through a `ProviderAdapter`
   method — no direct `gh`, `glab`, or `forge-cli` invocation outside the
   adapter implementations.
2. **Equivalent operation on both adapters.** Each trait method must have a
   working implementation on both `GhCliAdapter` and `ForgeCliAdapter`, with
   parity unit tests against fixtures.
3. **Upstream `forge-cli` capabilities present.** The extensions listed in §3
   must be available in the pinned `forge-cli` version used by the GitLab
   branch. If any are missing, the GitLab path will fail at runtime and the
   gap must be closed in `forge-cli` before the new operation is exposed.

## 5. Adding a third provider (codeberg, gitea, …)

When extending the routing layer to a third provider:

1. Implement the `ProviderAdapter` trait against the new provider's CLI
   (typically a thin shell-out adapter that targets the upstream provider
   binary or `forge-cli` if it gains support for the new provider).
2. Add a new variant to `Provider` and register the adapter in
   `provider::select_adapter()` so `resolve_repo` can route requests to it.
3. Extend `resolve_repo` URL pattern matching to recognise the new provider's
   remote forms (`git@<host>:`, `https://<host>/`, `ssh://git@<host>/`,
   and any host-prefixed `--repo` slug).
4. Add unit tests against fixtures covering each `ProviderAdapter` method on
   the new adapter, mirroring the existing `GhCliAdapter` and
   `ForgeCliAdapter` test layout.
5. Run the §4.5 validation checkpoints. Confirm every trait method works,
   that upstream CLI capabilities listed in §3 have equivalents on the new
   provider, and that no `plan-issue-cli` call site bypasses the trait.

If the new provider has no native equivalent for one of the §3 capabilities
(for example a missing close-reason concept or a missing comment stream
endpoint), document the mapping inside the adapter implementation and prefer
behaviour that degrades gracefully — for example treat a missing reason as a
comment prefix, as the GitLab branch does today.
