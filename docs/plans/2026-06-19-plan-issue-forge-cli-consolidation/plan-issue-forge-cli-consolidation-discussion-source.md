# plan-issue → forge-cli Provider Consolidation Implementation Handoff

- Status: decisions settled; ready for plan tracking.
- Date: 2026-06-19
- Source: operator review triggered by a bot-identity routing effort. A
  GitHub App bot (`graysurf-agent-bot`) should author the *intermediate*
  lifecycle comments of issue-backed plan trackers while the user
  (`graysurf`) opens and closes the tracker. Shell-level shims to achieve
  this proved fragile, surfacing the deeper question of whether plan-issue
  should own a provider client at all.
- Intended next step: open an L2 plan-tracking issue from this bundle. This is
  a source artifact, not the implementation itself.

## Execution

- Recommended plan:
  `docs/plans/2026-06-19-plan-issue-forge-cli-consolidation/plan-issue-forge-cli-consolidation-plan.md`
- Recommended execution state:
  `docs/plans/2026-06-19-plan-issue-forge-cli-consolidation/plan-issue-forge-cli-consolidation-execution-state.md`
- Status: decisions settled; plan tracking is the next step.
- Next-task source: this document.

## Purpose

Decide and scope whether plan-issue should stop using `gh`/`glab` directly and
route every provider operation through `forge-cli`, so that `forge-cli` becomes
the single provider gateway and the single identity chokepoint. This document
answers the three operator questions, records the settled decisions, and
specifies the implementation boundaries, sequence, risks, and validation bar so
a later plan can execute without re-litigating the design.

## Confirmed Facts

Grounded in two read-only architectural reviews of
`/Users/terry/Project/sympoies/nils-cli` (`main`) completed this session.

- plan-issue performs every provider mutation through exactly five methods on
  one `ProviderAdapter` trait: `create_issue` and `close_issue` (AUTHORING —
  open/close the issue), and `comment_issue`, `edit_issue_body`,
  `edit_issue_labels` (POSTING). Reads are `issue_body`, `issue_evidence`,
  `list_open_tracker_issues`, `pr_is_merged`, `pr_merge_summary`, `pr_comments`
  (`crates/plan-issue/src/github.rs:10-66`).
- Adapter selection is uniform and runtime-driven by provider, not per
  subcommand: `provider::select_adapter` returns `GhCliAdapter` for GitHub
  (spawns `gh`) and `ForgeCliAdapter` for GitLab/Local (spawns `forge-cli`)
  (`crates/plan-issue/src/provider.rs:172-178`). ~35 call sites in
  `execute.rs` are already trait-based and provider-agnostic.
- `github.rs` is a historical artifact, not a deliberate feature/perf choice.
  plan-issue was hard-wired to `gh` from its first orchestration commit
  (#224, 2026-02-25); the provider abstraction and `ForgeCliAdapter` arrived
  later (#498, 2026-05-25). GitHub was deliberately left on `GhCliAdapter` to
  preserve zero behaviour change at the time
  (`crates/plan-issue/src/provider.rs:3-4`; runbook
  `crates/plan-issue/docs/runbooks/provider-routing-runbook.md:144-147,179-192`).
- `forge-cli` (`crates/forge-cli`, `Cargo.toml:6` "Provider-neutral forge CLI
  (gh / glab wrapper)") is itself a `gh`/`glab` subprocess wrapper, not an API
  client (no `octocrab`/`reqwest`). Every backend call funnels through
  `ProcessRunner` (`crates/forge-cli/src/backend.rs:161-272`).
- `forge-cli` passes the parent process environment to the spawned backend
  verbatim — no `env_clear`, no token injection
  (`crates/forge-cli/src/backend.rs:199-204`). An injected `GH_TOKEN` from a
  wrapper governs the `gh` child exactly as it does for plan-issue's current
  `GhCliAdapter`. This is what makes verb-based identity routing work after
  consolidation.
- Each `ProviderAdapter` write method maps to exactly one `forge-cli`
  subprocess (`crates/plan-issue/src/forge_cli_adapter.rs:230-321`). No
  plan-issue subcommand batches multiple provider mutations into a single
  forge-cli call: `record open` reuses one adapter across `create_issue` +
  N×`comment_issue` + `edit_issue_body` (`execute.rs:846,967,974,1006,1011,1029`);
  `record close` across `comment_issue` + `edit_issue_body` + `close_issue`
  (+ optional `edit_issue_labels`) (`execute.rs:1837,1842,1857,1860,1869`).
  `ForgeCliAdapter::close_issue`-with-comment decomposes into separate
  `issue comment` + `issue close` subprocesses
  (`forge_cli_adapter.rs:339-348`). So a verb-based router governs each op
  independently and reliably.
- Identity today (both adapters) is the inherited ambient token. Neither
  adapter reads any token env var; the only env they consult is `FORGE_CLI_BIN`
  (the forge-cli binary override) (`forge_cli_adapter.rs:45-49`). plan-issue
  does not perform any project-v2 board mutation — the `added_to_project_v2`
  timeline event observed on the tracker is external automation.

## Question 1 — forge-cli Capability Inventory and Gaps

### Full command surface (from `forge-cli --help`)

| Group | Subcommands |
| --- | --- |
| `pr` | create, view, list, edit, comment, comments, ready, review-threads, tasks, merge, close, checks, wait-checks, deliver |
| `issue` | create, view, list, edit, comment, close, reopen |
| `activity` | personal activity across forge repositories |
| `label` | repository label catalog audit + ensure |
| `inbox` | status, list, next |
| `search` | full-text / reverse-reference search over issues and PRs |
| `repo` | view (slug, default branch, merge methods) |
| `auth` | status (verify gh / glab auth) |
| `completion` | shell-completion scripts |

forge-cli already exposes a strict superset of what plan-issue needs. Every
plan-issue trait method has a direct forge-cli command on GitHub:
`issue create` (`--title --body-file --label --assignee`,
`ops/issue_create.rs:124-152`), `issue comment` (`ops/issue_comment.rs:98-105`),
`issue edit` (body + `--add-label/--remove-label`, `ops/issue_edit.rs:108-147`),
`issue close` (`ops/issue_close.rs:82-88`), `issue view --with-comments`
(`ops/issue_view.rs:115-118,229-230`), `issue list` (`ops/issue_list.rs`),
`pr view` (state/merged_at/merge_commit_sha, `ops/pr_view.rs:202-236`),
`pr checks --required-only --format json`
(`ops/pr_checks.rs:274-287,303-317,565-629`; `cli.rs:558-571`), and
`pr comments` (`ops/pr_comments.rs:113-129,208`).

### Gaps (what is insufficient today)

1. **`issue close --reason` is missing (forge-cli gap).**
   `GhCliAdapter::close_issue` passes `--reason completed|"not planned"`
   (`github.rs:548-560`). forge-cli's `issue close` hard-codes
   `["issue","close",<id>]` for all providers with no `--reason`
   (`ops/issue_close.rs:82-88`), and `ForgeCliAdapter::close_issue` intentionally
   drops the reason (`forge_cli_adapter.rs:323-349`, comment 336-337). Routing
   GitHub through forge-cli today loses the `not planned` vs `completed`
   distinction. Severity: medium (metadata only; both still close the issue).
   Fix is small/clean: one CLI flag + one argv append on the GitHub arm.

2. **Required-vs-non-required check classification is hard-coded — ADAPTER gap,
   not a forge-cli gap.** forge-cli already produces the data:
   `pr checks --required-only --format json` returns `required_count`, gating
   `state`, and per-check `required` classification
   (`ops/pr_checks.rs:281-287,303-317,440-504`). But
   `ForgeCliAdapter::pr_merge_summary` calls plain `pr checks` and hard-codes
   `required_state=success, required_count=0, non_required_failures=[]`
   (`forge_cli_adapter.rs:413-421`). Left as-is, a GitHub PR with a failing
   *required* check would be reported as "none required" and could pass the
   `record close` merge gate. Severity: HIGH — merge-gate correctness; this is
   the must-fix-before-flip item. Fix is adapter-only (pass `--required-only`,
   read real fields); no forge-cli change.

3. **Markdown escaped-control guard is not re-homed (partial gap).**
   `github.rs::guard_provider_payload` does two checks: local-path leak
   (`validate_no_local_paths`) and escaped-control markdown
   (`validate_markdown_payload`) (`github.rs:189-197`). The local-path guard IS
   enforced by forge-cli on the write ops plan-issue uses
   (`ops/issue_create.rs:69,71`, `ops/issue_comment.rs:68`,
   `ops/issue_edit.rs:64-74`, engine `validations.rs:331-342`) — no regression.
   The escaped-control markdown guard lives only in `github.rs:191` and is not
   wired into any forge-cli op. Severity: low (cosmetic corruption guard; already
   absent on GitLab/Local). Fix is small/additive: re-home it into forge-cli
   write ops (preferred for the chokepoint) or keep it in the adapter.

`#557` is CLOSED and was a GitLab rendering bug ("Required: unknown"), not a
forge-cli capability tracker — it is not a blocker. No open issue tracks the
`--reason` extension, the markdown guard, or the GitHub-adapter retirement.

## Question 2 — Should the Identity Shim Move Into nils-cli?

### Decision: No — keep identity as a thin shell wrapper around forge-cli

The shim today is three shell constructs in `local-scripts` (`.private`,
`_lib/shared/env/30-forge-bot.zsh` + `bin/forge-cli-router`):

- `forge-cli()` function — governs forge-cli typed in an interactive/agent zsh
  shell.
- `forge-cli-router` (`FORGE_CLI_BIN`) — governs forge-cli spawned as a
  subprocess by a binary (e.g. plan-issue's `ForgeCliAdapter`), because a
  binary's child inherits env + PATH but not shell functions.
- `plan-issue()` function — a heuristic that guesses plan-issue subcommands and
  injects the bot token for plan-issue's `gh`-backed path; it misses
  `start-plan`/`close-plan` and relies on CLI-word guessing.

After consolidation (Question 3), plan-issue spawns `forge-cli` for every
provider op, so the verb-based `forge-cli-router` governs them reliably and the
`plan-issue()` heuristic is deleted. The remaining question is whether the
*token-selection logic itself* should become first-class inside the
`forge-cli`/plan-issue binaries.

Reasons to keep it in the shell:

- Token **minting** requires the GitHub App private key + installation map,
  which live in the secrets/shell layer (`github-app-cli` +
  `~/.config/secrets`). Moving minting into `forge-cli` couples a
  provider-neutral wrapper to App credentials and re-implements
  `github-app-cli`. Poor separation.
- Token **selection** (which identity per op) becomes verb-based and reliable
  once plan-issue routes through forge-cli — the router already does it
  (`create`/`close` → principal, `comment`/`edit` → bot). No in-binary logic
  needed.
- `forge-cli` is deliberately provider-neutral (`Cargo.toml:6`). Identity/App
  concepts dilute that.
- The bot identity is only needed in agent shells where the wrapper loads. No
  current non-shell (CI/cron/headless) context needs it.

Revisit only if a non-shell context later needs the bot identity; even then the
narrow move is a documented token-selection env in forge-cli, never App-key
minting.

## Question 3 — Can plan-issue Use Only forge-cli (no gh/glab)?

### Verdict: Yes — clean, conditional on the Question 1 fixes landing first

forge-cli supports every operation plan-issue needs on GitHub, passes the
ambient token through verbatim, and runs one subprocess per provider op so
verb-based identity routing is reliable. `github.rs` is an unfinished-migration
legacy path, so retiring it finishes the routing work begun in #498. The only
blockers are the three Question 1 items, and the merge-gate fix (gap 2) MUST
land before flipping the GitHub arm.

## Decisions

- Consolidate: flip `provider::select_adapter` so GitHub uses `ForgeCliAdapter`,
  then retire `crates/plan-issue/src/github.rs`. plan-issue stops calling `gh`
  directly.
- Identity stays a shell wrapper around forge-cli (Question 2). Delete the
  `plan-issue()` heuristic and `_planissue_runs_as_graysurf` from local-scripts;
  keep the `forge-cli()` function (interactive) and `forge-cli-router`
  (`FORGE_CLI_BIN`, for plan-issue's subprocess), which become the single live
  seam.
- The merge-gate fix (gap 2) is required before the flip; close-reason (gap 1)
  and the markdown guard (gap 3) ship in the same release.
- This is L2-scale: it changes shared `forge-cli` + plan-issue behaviour and
  requires a release plus a downstream version pin.
- Identity routing is **uniform** (locked): `issue create` / `issue close` →
  graysurf everywhere, including ad-hoc `issue-follow-up` issues;
  `comment` / `edit` → bot. The `forge-cli()` function and the
  `forge-cli-router` carry identical verb rules — there is no plan-issue vs
  ad-hoc distinction and no per-call override. PR `create` / `merge` /
  `deliver` stay graysurf (squash-author policy, unchanged).

## Scope

- forge-cli: add `issue close --reason` (GitHub arm); re-home the
  escaped-control markdown guard into forge-cli write ops.
- plan-issue adapter: pass `--reason` through on GitHub; switch
  `pr_merge_summary` to `pr checks --required-only` reading real fields.
- plan-issue routing: flip the GitHub arm of `select_adapter`; move the
  `ProviderAdapter` trait + `PrMergeSummary`/`CloseReason` types out of
  `github.rs`; delete the `gh` impl and its tests.
- local-scripts: delete the `plan-issue()` heuristic; align/keep the
  `forge-cli()` function and `forge-cli-router`.

## Non-Scope

- No GitHub App token minting inside nils-cli.
- No change to GitLab/Local routing (already on `ForgeCliAdapter`).
- No change to the squash-author policy (PR create/merge/deliver stay graysurf).
- No project-v2 board behaviour (plan-issue never touches it).
- No change to `forge-cli`'s broader surface (`activity`, `label`, `search`,
  `inbox`).

## Implementation Boundaries

- The `ProviderAdapter` trait and its call sites in `execute.rs` stay stable;
  the consolidation changes which impl is selected, not the trait surface.
- GitLab/Local keep the close-reason degrade (comment prefix) and the
  zero-required path (GitLab has no required-check concept).
- Runtime coupling: the new plan-issue and the new forge-cli MUST ship in the
  same nils-cli release. A new plan-issue passing `--reason` to an old installed
  forge-cli would error. nils-cli releases all crates at one version, so a
  single release upgrades both binaries atomically — the changes must land and
  release together.

## Requirements

- `forge-cli issue close` accepts an optional `--reason completed|"not planned"`
  on GitHub and continues to ignore it on GitLab/Local.
- `ForgeCliAdapter::close_issue` passes the reason on GitHub; `pr_merge_summary`
  reports true required-check state/count on GitHub.
- The escaped-control markdown guard is enforced for plan-issue's GitHub writes
  after the flip (via forge-cli or retained in the adapter).
- After the flip, plan-issue makes zero direct `gh` calls; `select_adapter`
  returns `ForgeCliAdapter` for all providers.
- The local-scripts `plan-issue()` heuristic is removed; the `forge-cli-router`
  governs plan-issue's identity by verb.

## Acceptance Criteria

- A GitHub `record close` closes with the correct reason and gates correctly on
  a failing required check (no false pass).
- A GitHub `record open`/`record close` run authors `create`/`close` under the
  principal identity and intermediate comments under the bot identity, driven by
  the forge-cli verb (verified by stub-asserted injected token per call).
- `github.rs` is removed with no remaining references; `execute.rs` call sites
  unchanged.
- Local-path and escaped-control guards still reject offending payloads on the
  GitHub write path.
- GitLab/Local behaviour is unchanged.

## Validation Plan

- Finish-line gate: `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
  (changed-scope `cargo fmt`, `clippy -D warnings`, package tests).
- Extend the existing adapter test harnesses: `ScriptedRunner`
  (`forge_cli_adapter.rs` tests) and the `GhRunner`/`StubBinDir` `gh` stubs to
  assert the per-op injected identity (authoring vs posting) and the new
  `--reason`/`--required-only` argv.
- This is a testable production behaviour change: capture failing-test evidence
  first (`test-first-evidence`), since the forge-cli test-first gate requires it
  for feature PRs.
- Full parity before release: `bash scripts/ci/nils-cli-checks-entrypoint.sh`.

## Risks And Guardrails

- Merge-gate correctness (gap 2) is the highest risk: do not flip the GitHub arm
  until `pr_merge_summary` reads real required-check data. Guardrail: land the
  adapter fix and its test in the same change as the flip.
- Close-reason loss (gap 1) until `--reason` lands: guardrail — ship the flag in
  the same release as the flip.
- Markdown guard regression (gap 3): guardrail — re-home the guard before the
  flip, with a parity test.
- Runtime version skew: guardrail — single atomic release; do not merge the flip
  to an environment that pins an older forge-cli.
- Trait/type relocation: `ProviderAdapter`/`PrMergeSummary`/`CloseReason` are
  defined in `github.rs`; move them before deleting the file or the build
  breaks.

## Blast Radius

- plan-issue: `provider.rs` (1-line flip + relocate trait/types),
  `forge_cli_adapter.rs` (2 methods), delete `github.rs` (~1340 lines incl.
  ~25 tests); `execute.rs` call sites unchanged.
- forge-cli: `ops/issue_close.rs` + `cli.rs` (one `--reason` flag); optionally
  `ops/issue_create.rs`/`issue_comment.rs`/`issue_edit.rs` + `validations.rs`
  for the markdown guard.
- Downstream: nils-cli release (Y+1), agent-runtime-kit version pin, local-scripts
  shim simplification synced to both machines.

## Recommended Delivery Sequence

1. forge-cli: add `issue close --reason` (GitHub) + re-home markdown guard.
2. plan-issue adapter: `--reason` passthrough + `pr_merge_summary --required-only`
   with parity tests.
3. Flip `select_adapter` GitHub arm to `ForgeCliAdapter`.
4. Retire `github.rs` (relocate trait/types first; delete `gh` impl + tests).
5. nils-cli release Y+1; bump the agent-runtime-kit version pin.
6. local-scripts: delete the `plan-issue()` heuristic; align the `forge-cli()`
   function and `forge-cli-router` to the locked uniform rules (`issue
   create`/`close` → graysurf, `comment`/`edit` → bot); sync both machines.

Steps 1–4 land together in one release (runtime coupling). Steps 5–6 follow.

## Read-First References

- `crates/plan-issue/src/provider.rs` — `select_adapter`, the flip point and the
  trait/type relocation source.
- `crates/plan-issue/src/github.rs` — the adapter to retire (and the
  guard/required-check logic to preserve).
- `crates/plan-issue/src/forge_cli_adapter.rs` — the adapter to extend.
- `crates/forge-cli/src/ops/issue_close.rs`, `cli.rs` — the `--reason` change.
- `crates/plan-issue/docs/runbooks/provider-routing-runbook.md` — the routing
  design and the documented "zero behaviour change" intent for GitHub.
- `DEVELOPMENT.md`, `AGENT_DOCS.toml` — validation bar.
