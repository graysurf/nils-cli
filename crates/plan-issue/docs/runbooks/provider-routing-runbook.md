# Plan-Issue CLI provider routing runbook

This is the long-term runbook for the `plan-issue` provider routing layer.
It was promoted from the Sprint 1 design note after the multi-provider routing
work (Sprints 1–4) shipped through sympoies/nils-cli#498, #499, #503, and #505.
For the historical sprint deliberation, see the original design note in the
archived bundle
`agent-plan-archive:plans/github.com/sympoies/nils-cli/2026-05-25-plan-issue-cli-provider-abstraction/`;
this runbook is the durable reference for contributors who maintain or extend
the routing layer.

## 1. Routing strategy

`plan-issue` routes EVERY provider — GitHub, GitLab, and the in-process Local
backend — through `forge-cli` via a single `ForgeCliAdapter` that shells out to
`forge-cli issue …` / `forge-cli pr …` with `--format json` and parses the v1
envelope. The adapter emits `--provider github|gitlab|local` so one
implementation serves all backends. It implements the `ProviderAdapter` trait
(defined in `crates/plan-issue/src/adapter.rs`), so the call sites in
`crates/plan-issue/src/execute.rs` are provider-neutral.

GitHub was originally kept on an in-tree `GhCliAdapter` (a direct `gh` client)
to preserve a zero-behaviour-change cut when the abstraction landed (#498). The
plan-issue → forge-cli consolidation
(`docs/plans/2026-06-19-plan-issue-forge-cli-consolidation`) flipped the GitHub
arm onto `ForgeCliAdapter` and deleted `github.rs`, so `forge-cli` is now the
single provider gateway and identity chokepoint. Identity is the inherited
ambient token, exactly as `GhCliAdapter` behaved (`forge-cli` passes the parent
environment to the spawned `gh`/`glab` child verbatim).

### Why subprocess to `forge-cli` (not library linkage or in-tree clients)

1. Routing through `forge-cli` is the workspace decision for cross-provider
   plumbing — `forge-cli` already owns provider detection, repo normalization,
   `iid` ↔ slug handling, pipeline status, and version probing. Re-implementing
   any of that inside `plan-issue` would duplicate logic and create a
   second source of provider truth.
2. The subprocess + JSON envelope pattern keeps the test ergonomics consistent:
   integration tests PATH-prepend a `forge-cli` stub (via
   `nils-test-support::StubBinDir`) that emits v1 envelopes.
3. Process count is bounded — a `record open` is roughly five atomic ops, so
   five `forge-cli` processes per run is acceptable for a CLI lifecycle command.
4. Library linkage would still need the same upstream capabilities in
   `forge-cli` (see §3) and would add a new workspace dependency edge.
5. A single forge-cli rail also unifies the markdown / local-path egress guards
   in one place (forge-cli's write-op validations) rather than duplicating them
   per in-tree client.

Library linkage stays available as a future fallback if subprocess overhead
ever dominates wall time on a tracking-issue run. Because the trait boundary
already isolates provider I/O, swapping the GitLab branch from subprocess to
in-process is a mechanical change.

## 2. Repo and provider detection

Provider detection happens in `crates/plan-issue/src/provider.rs` and must
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
`plan-issue` lifecycle commands on GitLab.

- **Issue view with comments.** `forge-cli issue view` must expose the comment
  stream — preferred shape is an optional `comments: Vec<IssueCommentSummary>`
  field on the v1 `IssueViewPayload`, gated behind a `--with-comments` flag.
  `plan-issue` needs this on the hot path for `record audit` so that
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

The provider boundary is the `ProviderAdapter` trait, **defined** in
`crates/plan-issue/src/adapter.rs` and re-exported from
`crates/plan-issue/src/provider.rs` (`pub use crate::adapter::ProviderAdapter`).
The `Provider` / `Repo` routing types plus `select_adapter` / `resolve_repo`
live in `provider.rs`. All lifecycle code paths in `execute.rs` go through the
trait — no direct provider CLI shell-out should appear outside the adapter
implementation. The trait takes `repo: &str` (the slug); call sites pass
`&repo.slug` (see §4.4). There is no `provider()` method on the trait.

```rust
// crates/plan-issue/src/adapter.rs — the provider boundary trait
pub trait ProviderAdapter {
    fn issue_body(&self, repo: &str, issue: u64) -> Result<String, String>;
    fn issue_evidence(&self, repo: &str, issue: u64) -> Result<(String, String), String>;
    fn list_open_tracker_issues(&self, repo: &str, labels: &[String]) -> Result<Vec<u64>, String>;
    fn create_issue(&self, repo: &str, title: &str, body_file: &Path, labels: &[String])
        -> Result<(u64, String), String>;
    fn edit_issue_body(&self, repo: &str, issue: u64, body_file: &Path) -> Result<(), String>;
    fn comment_issue(&self, repo: &str, issue: u64, body_file: &Path) -> Result<String, String>;
    fn edit_issue_labels(
        &self, repo: &str, issue: u64,
        add_labels: &[String], remove_labels: &[String],
    ) -> Result<(), String>;
    fn close_issue(
        &self, repo: &str, issue: u64,
        reason: CloseReason, close_comment: Option<&str>,
    ) -> Result<(), String>;
    fn pr_is_merged(&self, repo: &str, pr: u64) -> Result<bool, String>;
    fn pr_merge_summary(&self, repo: &str, pr: u64) -> Result<PrMergeSummary, String>;
    fn pr_comments(&self, repo: &str, pr: u64) -> Result<Vec<Value>, String>;
}

// crates/plan-issue/src/provider.rs — routing types and adapter selection
pub struct Repo { pub provider: Provider, pub slug: String, pub host: Option<String> }
pub enum Provider { GitHub, GitLab, Local }

pub fn select_adapter(repo: &Repo, force: bool) -> Box<dyn ProviderAdapter> {
    match repo.provider {
        Provider::GitHub => Box::new(crate::forge_cli_adapter::ForgeCliAdapter::new_github(force)),
        Provider::GitLab => Box::new(crate::forge_cli_adapter::ForgeCliAdapter::new(force)),
        Provider::Local => Box::new(crate::forge_cli_adapter::ForgeCliAdapter::new_local(force)),
    }
}
```

`ForgeCliAdapter` is the single production implementation; it emits
`--provider github|gitlab|local` so one adapter serves all backends. It shells
out to `forge-cli` and parses the v1 JSON envelope. The retired `GhCliAdapter`
(a direct `gh` client) was deleted by the consolidation; `forge-cli` itself
still wraps `gh`/`glab`, so the GitHub behaviour is preserved through the
forge-cli rail.

### 4.2 Repo resolution

```rust
// crates/plan-issue/src/provider.rs (continued)
pub fn resolve_repo(repo_override: Option<&str>) -> Result<Repo, String> {
    // 1. If --repo carries a host (gitlab.<host>/group/project), parse provider.
    // 2. Else if --repo is bare owner/repo, default Provider::GitHub.
    // 3. Else read `git remote get-url origin`; detect github vs gitlab from URL pattern.
    // 4. Normalize to (provider, slug, optional host).
}
```

`forge-cli`'s `provider::detect` already covers step 3. `plan-issue` may
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
`plan-issue` error messages remain informative for operators.

### 4.4 Behaviour preservation

The plan-issue → forge-cli consolidation
(`docs/plans/2026-06-19-plan-issue-forge-cli-consolidation`) flipped the GitHub
arm from the in-tree `GhCliAdapter` onto `ForgeCliAdapter` with three parity
fixes that landed in the same release so GitHub behaviour is preserved:

- `forge-cli issue close --reason completed|"not planned"` (GitHub arm) +
  `ForgeCliAdapter::close_issue` passing the reason through, so the
  completed/not-planned distinction survives the flip (GitLab/Local ignore it).
- `ForgeCliAdapter::pr_merge_summary` calls `forge-cli pr checks --required-only`
  and reads the real gating `state` / `required_count` / non-required failures,
  so a failing required check still blocks the `record close` merge gate.
- The escaped-control markdown guard re-homed into forge-cli's write ops
  (`no_escaped_control_markdown`), alongside the existing local-path guard, so
  both egress guards survive on the GitHub write path. Note: forge-cli's guards
  have no plan-issue `--force` bypass — `--force` no longer suppresses them.

Identity is unchanged: `forge-cli` passes the parent environment to the spawned
`gh` child verbatim, so the inherited ambient token governs the call exactly as
`GhCliAdapter` did. The cwd auto-detect default (when `--repo` is omitted)
matches `forge-cli`, keeping existing GitHub-first command lines unchanged.

### 4.5 Validation checkpoints

Run these checkpoints any time a provider operation is added, the trait
shape changes, or a new adapter implementation lands:

1. **Audit completeness.** Every provider call site in
   `crates/plan-issue/src/execute.rs` must go through a `ProviderAdapter`
   method — no direct `gh`, `glab`, or `forge-cli` invocation outside the
   adapter implementations.
2. **Equivalent operation per provider.** Each trait method must work for every
   provider the adapter emits (`github`, `gitlab`, `local`), with parity unit
   tests (`forge_cli_adapter.rs` `ScriptedRunner` tests) asserting the per-
   provider argv shape, plus the integration tests' PATH-prepended `forge-cli`
   stub.
3. **Upstream `forge-cli` capabilities present.** The capabilities listed in §3
   must be available in the pinned `forge-cli` version. Because plan-issue and
   forge-cli ship in one nils-cli release, a new plan-issue must not be paired
   with an older installed forge-cli (e.g. one without `issue close --reason`).

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
4. Add unit tests against fixtures covering each `ProviderAdapter` method for
   the new provider, mirroring the existing `ForgeCliAdapter` `ScriptedRunner`
   test layout.
5. Run the §4.5 validation checkpoints. Confirm every trait method works,
   that upstream CLI capabilities listed in §3 have equivalents on the new
   provider, and that no `plan-issue` call site bypasses the trait.

If the new provider has no native equivalent for one of the §3 capabilities
(for example a missing close-reason concept or a missing comment stream
endpoint), document the mapping inside the adapter implementation and prefer
behaviour that degrades gracefully — for example treat a missing reason as a
comment prefix, as the GitLab branch does today.
