# Plan-Issue CLI Provider Abstraction — Design Note (Sprint 1)

Companion to `plan-issue-cli-provider-abstraction-plan.md`. This note lands the
Sprint 1 deliverables:

- §1 Audit: every `gh` shell-out in `plan-issue-cli` mapped to its abstract
  operation and `forge-cli` equivalent (Task 1.1).
- §2 Routing strategy: subprocess-to-`forge-cli` vs. library linkage (Task 1.2,
  Q1).
- §3 Resolutions for Q2–Q5 (Task 1.3).
- §4 Contract sketch the routing layer must satisfy before Sprint 2 code
  changes start.

## 1. Audit of current provider surface (Task 1.1)

All provider calls in `plan-issue-cli` route through the `GitHubAdapter` trait
in `crates/plan-issue-cli/src/github.rs:11-61`. The only implementation today
is `GhCliAdapter` (lines 79–435), which shells out to `gh` for every method.
This means the trait already defines a clean cut line for provider routing —
no refactor of the call sites is needed; we only need a second adapter
implementation (or an internal dispatch).

### 1.1 Trait → abstract operation map

| Trait method (file: `github.rs`) | Abstract operation | `gh` invocation | Stdout shape parsed |
| --- | --- | --- | --- |
| `issue_body(repo, issue) -> String` (132) | Read issue body | `gh issue view <n> --repo <r> --json body` | `{body}` JSON |
| `issue_evidence(repo, issue) -> (body, comments_json)` (151) | Read body + comment stream | `gh issue view <n> --repo <r> --json body,comments` | `{body, comments[]}` JSON |
| `create_issue(repo, title, body_file, labels) -> (n, url)` (178) | Open new issue | `gh issue create --repo <r> --title <t> --body-file <f> [--label <l>]*` | URL on stdout; parse trailing `/<n>` for number |
| `edit_issue_body(repo, issue, body_file)` (213) | Replace issue body | `gh issue edit <n> --repo <r> --body-file <f>` | empty |
| `comment_issue(repo, issue, body_file) -> url` (228) | Append comment, return its URL | `gh issue comment <n> --repo <r> --body-file <f>` | comment URL (`...#issuecomment-<m>`) |
| `edit_issue_labels(repo, issue, add[], remove[])` (249) | Add/remove labels | `gh issue edit <n> --repo <r> [--add-label CSV] [--remove-label CSV]` | empty |
| `close_issue(repo, issue, reason, comment?)` (293) | Close with reason + optional comment | `gh issue close <n> --repo <r> --reason "completed\|not planned" [--comment <c>]` | empty |
| `pr_is_merged(repo, pr) -> bool` (326) | Merge predicate | `gh pr view <n> --repo <r> --json state,mergedAt` | `state` ∈ MERGED ∨ `mergedAt` non-null |
| `pr_merge_summary(repo, pr) -> PrMergeSummary` (348) | Merge state + commit SHA + check rollup | `gh pr view <n> --repo <r> --json state,mergeCommit,statusCheckRollup` | `{state, mergeCommit.oid, statusCheckRollup}` |
| `pr_comments(repo, pr) -> Vec<Value>` (385) | Read PR's issue-comment stream | `gh api --paginate repos/<r>/issues/<n>/comments` | concatenated JSON arrays |

Call sites: 35 invocations across `crates/plan-issue-cli/src/execute.rs`
(record open/post/audit/close, link-pr, dispatch plan/sprint lifecycles, and
resolve-approval). Every site goes through `GhCliAdapter::new(force)`; no
direct `gh` shell-out exists outside `github.rs`.

Repo lookup (`resolve_repo` at lines 520–546) currently hardcodes GitHub
remote-URL patterns (`git@github.com:`, `https://github.com/`, etc.) and falls
back to `is_owner_repo` which rejects values containing `:` or `://`. This is
a second touchpoint: provider detection from the git remote must learn the
GitLab patterns too.

### 1.2 `forge-cli` equivalence and gaps

| Operation | `forge-cli` atom | Schema | Equivalence | Gap |
| --- | --- | --- | --- | --- |
| Read issue body | `issue view <n>` | `cli.forge-cli.issue.view.v1` | `IssueViewPayload.body` | — |
| Read body + comments | `issue view <n>` | `cli.forge-cli.issue.view.v1` | body only | **G1**: payload lacks `comments[]`. Need either `--with-comments` flag or a new `issue comments` atom |
| Open issue | `issue create --title --body-file [--label]*` | `cli.forge-cli.issue.create.v1` | full | — |
| Replace body | `issue edit <n> --body \| --body-file` | `cli.forge-cli.issue.edit.v1` | full | — |
| Append comment | `issue comment <n> --body-file` | `cli.forge-cli.issue.comment.v1` | full (payload `.url`) | — |
| Add/remove labels | `issue edit <n> --add-label … --remove-label …` | `cli.forge-cli.issue.edit.v1` | full (`IssueEditArgs.add_label/remove_label` at `cli.rs:832-835`) | — (downstream sandbox F-8 was about the older `--label` shorthand; the canonical flags exist) |
| Close issue | `issue close <n>` | `cli.forge-cli.issue.close.v1` | id-only close | **G2**: no `--reason` (completed/not-planned), no `--comment`. GitHub semantics drop; GitLab has no native reason concept either. Need either CLI flag additions or call `issue comment` + `issue close` as two ops |
| PR merge predicate | `pr view <n>` | `cli.forge-cli.pr.view.v1` | `state` ∈ `merged` ∨ `merged_at` ⇒ bool | — |
| PR merge summary | `pr view <n>` | `cli.forge-cli.pr.view.v1` | state + `merged_at`; **no `merge_sha`, no checks rollup** | **G3**: payload lacks `merge_commit_sha` and rolled-up check status. Need schema extension or a sibling `pr merge-summary` atom (potentially composable from `pr view` + `pr checks`) |
| PR comment stream | (none) | — | not exposed | **G4**: no `forge-cli pr comments` atom. Used only by `resolve-approval`, which scans for an approval phrase across the PR's issue-style comment stream |

### 1.3 Repo / provider detection

`resolve_repo` in `github.rs:520-583` must learn GitLab remote forms
(`git@gitlab.<host>:`, `https://gitlab.<host>/`, `ssh://git@gitlab.<host>/`).
`forge-cli` already does this in `crates/forge-cli/src/provider.rs::detect`
and `git_remote_url`. If we route through `forge-cli`, that detection is free.
If we link as a library, we should reuse the `forge-cli::provider` module
directly.

## 2. Routing strategy (Task 1.2, Q1)

### 2.1 Options

**Option A — Subprocess to `forge-cli`.** A new `ForgeCliAdapter` (peer of
`GhCliAdapter`) shells out to `forge-cli issue view --output json …` per
operation and parses the v1 envelope. Provider detection happens inside
`forge-cli`.

**Option B — Library linkage.** Add `nils-forge-cli` as a workspace dep on
`nils-plan-issue-cli`, call `forge_cli::ops::issue_view::run_with` etc.
directly. Reuse `forge_cli::provider::detect` for routing.

**Option C — In-tree GitLab adapter.** Add `GlabCliAdapter` (peer of
`GhCliAdapter`) that shells to `glab`. Internal trait dispatch picks the
adapter based on provider detection.

### 2.2 Comparison

| Axis | A: subprocess→forge-cli | B: library link | C: in-tree glab |
| --- | --- | --- | --- |
| Process count per `record open` (≈ 5 atomic ops) | 5× forge-cli + 5× `gh`/`glab` underneath | 5× `gh`/`glab` only | 5× `glab`/`gh` |
| Cargo dep cost | none new | adds `nils-forge-cli` lib | none new |
| Version skew | runtime `forge-cli` binary version vs. plan-issue expectations | compile-time pin | none — owns the glab call sites |
| Test ergonomics | stub forge-cli via PATH (existing pattern) | inject `BackendRunner` mock | stub `glab` via PATH |
| Parity with today's `gh` shell-out style | yes (identical pattern, just different binary) | departure (in-process) | yes (sibling adapter) |
| Surface gaps (G1–G4) handling | gaps surface as forge-cli upgrades | same, plus needs lib-level API | gaps are local — write any missing glab calls directly |
| `forge-cli` runtime PATH dependency | yes — explicit `FORGE_CLI_BIN` override needed | no | no |
| Closes F-3 fastest | medium (depends on G1–G4 timing in forge-cli) | medium (same gaps) | fastest (no cross-crate negotiation) |
| Aligns with sandbox plan's stated Decision 2 ("Route through forge-cli") | yes | yes | no |

### 2.3 Recommendation: Option A (subprocess to `forge-cli`)

Reasoning:

1. The source doc's Decision 2 already commits to routing through `forge-cli`.
   Option A is the lowest-friction realisation of that decision.
2. Process count is bounded: a `record open` is ~5 atomic ops. 5 extra
   processes per run is fine for a CLI lifecycle command (cf. today's `gh`
   path also opens 5+ `gh` processes — the floor is the same).
3. Subprocess+JSON envelope keeps the test surface identical to today's
   `GhCliAdapter` (PATH-prepended stub binaries). Reusing
   `nils-test-support::StubBinDir` patterns saves test setup churn.
4. `forge-cli` already handles `Provider::detect` + repo normalization, so the
   GitLab remote URL patterns and `iid` ↔ slug indirection come free.
5. Gaps G1–G4 are small enough to land as `forge-cli` extensions inside
   Sprint 2/3. Library linkage (B) would need the same extensions and adds
   a new dep edge.
6. Option C duplicates `forge-cli`'s GitLab adapter inside `plan-issue-cli`.
   We would re-implement pipeline status, MR iid lookup, version probing —
   all of which `forge-cli` already does. Rejected.

Library linkage (B) remains a fallback if Sprint 2 measurements show
subprocess overhead dominates wall time for a tracking-issue run. The trait
boundary stays the same, so swapping later is mechanical.

### 2.4 Required `forge-cli` extensions

Sprint 2 (before GitLab `record open` ships):

- **G1** — Add `comments` to `issue view` output (preferred: extend the
  `IssueViewPayload` with an optional `comments: Vec<IssueCommentSummary>`
  field gated behind `--with-comments`). Justification: `issue_evidence`
  is on the hot path for `record audit` and shouldn't require two subprocess
  hops.

Sprint 3 (before `record close`):

- **G2** — Either (i) add `--reason {completed,not-planned}` and `--comment`
  to `forge-cli issue close` on the GitHub backend, or (ii) drop the reason
  argument from `plan-issue-cli` (the reason is only consumed by GitHub's
  closing-state UI; it has no programmatic effect). Recommendation: (ii) —
  treat `reason` as a comment prefix on GitLab and a `--reason` flag on
  GitHub via a small `forge-cli` extension; drop the special UI hint
  parameter from the trait if it cannot be carried.

Sprint 3 (before `record close` strict gating):

- **G3** — Extend `pr view` to optionally include `merge_commit_sha` and a
  `checks` summary, or add a new `pr merge-summary` atom that composes
  `pr view` + `pr checks` on both backends.

Sprint 3 (before `resolve-approval` GitLab):

- **G4** — Add `forge-cli pr comments` atom that returns
  `{provider, number, comments: [{body, url, author, created_at}]}`. GitHub
  backend wraps `gh api --paginate /repos/.../issues/<n>/comments`; GitLab
  backend wraps `glab api --paginate /projects/.../merge_requests/<iid>/notes`.

None of G1–G4 are required for Sprint 2 Task 2.1 ("Land the routing layer")
or Task 2.2 ("Implement GitLab branch for `record open`") — `record open`
only uses `create_issue`, `comment_issue`, `edit_issue_body`, and
`edit_issue_labels`, all of which have direct forge-cli equivalents today.

## 3. Q2–Q5 resolution (Task 1.3)

### Q2: Label catalogue fallback on GitLab

**Decided**: graceful pass-through. `plan-issue` should not own label
catalogue management; that belongs to `forge-cli label ensure` / `audit`.
When `record open --label …` is invoked:

1. plan-issue passes the labels through to `forge-cli issue create --label …`
   as-is.
2. If the GitLab project lacks the label, `forge-cli issue create` will fail
   (GitLab rejects unknown labels). plan-issue surfaces that error verbatim.
3. Operators run `forge-cli label ensure` against `manifests/forge-labels.yaml`
   ahead of time — this is the workflow already exercised in the sandbox
   (P2 pass).

No plan-issue-side fallback logic. Document this in the dispatch SKILL
prereqs alongside the label catalogue requirement.

### Q3: Provider discriminator on lifecycle payload

**Decided**: add `provider: "github"|"gitlab"` to the v1 record payloads.
GitLab uses per-project `iid` which collides with GitHub `number` across
hosts; the discriminator avoids ambiguity when the same `plan-issue-record:v2`
comment is read by a downstream consumer that doesn't already know the host.

This is **schema-additive** (consumers that ignore the field still work) so
it does not require a v2 bump. Encode as an optional field for backwards
compatibility with already-written comment bodies.

### Q4: `create-dispatch-lane-pr` GitLab port

**Decided**: defer. Out of this plan's scope (`plan` doc, Scope §). Once the
plan-issue routing lands, `create-dispatch-lane-pr`'s code path can call
`forge-cli pr create` directly — at which point the skill is one-line
provider-neutral and only the SKILL.md surface needs updating. Sprint 4
Task 4.2 (SKILL sweep) will handle the SKILL surface; a follow-up issue
should cover any code change if one is still needed.

### Q5: Auto-detect provider from cwd remote

**Decided**: yes, default to cwd auto-detect when `--repo` is omitted (matches
forge-cli's behaviour). When `--repo <slug>` is explicit, parse the host from
the slug if it includes one (`gitlab.example.com/group/project`); otherwise
assume GitHub for bare `owner/repo` (matches existing GitHub-first
expectation). This keeps existing GitHub workflows unchanged (R5).

## 4. Contract sketch

### 4.1 Trait shape

Keep the existing `GitHubAdapter` trait name as `ProviderAdapter` (rename for
honesty) or introduce a sibling. Recommendation: rename + retain the trait
shape; the methods are already provider-neutral except for the `close_issue`
reason argument (see G2 above).

```rust
// crates/plan-issue-cli/src/provider.rs (new)
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

Note: the existing `GhCliAdapter` stays for the GitHub branch (minimum
churn, R5). The new `ForgeCliAdapter` shells out to `forge-cli issue …`
/ `forge-cli pr …` with `--output json` and parses v1 envelopes. Both
implement `ProviderAdapter`.

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

`forge-cli`'s `provider::detect` does step 3 already. The cleanest move is to
shell out to `forge-cli repo view --output json` from this function and
inherit detection — at the cost of one extra subprocess per CLI run.
Alternative: copy the pattern matching locally to avoid the dependency. **Defer
this decision to Sprint 2 Task 2.1** when we know whether the routing layer
already needs forge-cli on PATH for any other reason (it will).

### 4.3 Error mapping

`forge-cli` errors surface as JSON envelopes:

```json
{"status":"error","schema_version":"cli.forge-cli.issue.create.v1","error":{"code":"…","message":"…"}}
```

`ForgeCliAdapter::run_forge` parses the envelope, returns `Err(format!("…"))`
for `status=error`, propagating the upstream `code`/`message` so plan-issue
error messages stay informative.

### 4.4 Behaviour preservation

R5 (existing GitHub callers see no behaviour change) is satisfied because:

- The GitHub branch keeps the existing `GhCliAdapter` unchanged.
- `select_adapter` picks `GhCliAdapter` for `Provider::GitHub` — the same
  code path that's exercised today.
- Repo resolution still produces the existing `String` slug for GitHub,
  wrapped in the new `Repo { provider: GitHub, slug, host: None }` struct.
- The trait method signatures change `repo: &str` to `repo: &Repo`, which is
  a tiny `&repo.slug` adjustment at each call site (a mechanical sed).

### 4.5 Validation checkpoints for Sprint 2 entry

Before any code changes in Sprint 2 Task 2.1:

1. **Reviewer agrees with §1 audit table** (Acceptance criterion: every `gh`
   invocation listed; equivalent forge-cli atom or marked gap).
2. **Reviewer agrees with §2.3 recommendation** (Option A subprocess) — if
   they prefer B or C, this design note is wrong, not the production code.
3. **G1–G4 owners assigned** (do they ship inside this plan as forge-cli
   companion PRs, or as a separate forge-cli issue bundle?).

## 5. Open items carried into Sprint 2+

- Owner of G1–G4 forge-cli extensions (this plan vs. companion plan).
- Whether to land an `agent-runtime-testing` companion PR alongside Sprint 2
  Task 2.3 (sandbox revalidation) so the sandbox source doc updates ship in
  the same review cycle.
- Whether `MockGitHubAdapter` (test fixture in `execute.rs:3964`) is renamed
  to `MockProviderAdapter` in Sprint 2 Task 2.1 or kept as the GitHub-only
  fixture with a sibling `MockForgeCliAdapter`. Recommendation: rename.
