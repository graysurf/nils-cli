# Provider-Neutral Plan-Tracking + Local Backend — Source

- Status: ready for plan generation
- Date: 2026-05-31
- Source: design discussion (provider-neutral plan-tracking + local backend),
  captured as the implementation source for this L2 rollout.
- Intended next step: open the tracking issue in `sympoies/nils-cli` via
  `create-plan-tracking-issue`.

## Execution

- Recommended plan: `docs/plans/2026-05-31-plan-tracking-local-provider/plan-tracking-local-provider-plan.md`
- Recommended execution state: `docs/plans/2026-05-31-plan-tracking-local-provider/plan-tracking-local-provider-execution-state.md`
- Status: ready to implement
- Next-task source: this document

## Purpose

The plan-tracking skill family already abstracts providers via the
`ProviderAdapter` trait (`crates/plan-issue-cli/src/github.rs:10`, 11 methods):
GitHub is `GhCliAdapter` (shells `gh`), GitLab is `ForgeCliAdapter` (shells
`forge-cli`). The e2e driver, however, is hardwired to GitHub (`gh` + raw
`gh api repos/...`). This rollout makes the driver provider-neutral and adds a
third `local` provider so the flow can run hermetically with no remote, and so
the issue half can later be lifted into a standalone service.

## The capability split (binding)

The 11 trait methods cleave into two halves, and this line bounds how far
"local" and "service" can go:

- **Issue / timeline half (8 methods) — REAL locally and service-grade.** The
  store is the source of truth: `create_issue`, `issue_body`,
  `issue_evidence`, `list_open_tracker_issues`, `edit_issue_body`,
  `comment_issue`, `edit_issue_labels`, `close_issue`.
- **PR / merge / CI half (3 methods) — SEEDED STUB only.** No real VCS/CI sits
  behind a local store, so `pr_is_merged`, `pr_merge_summary`, `pr_comments`
  return what a test seeded. This half does not grow into a service without a
  real VCS/CI. The realistic service = an issue/plan-tracking service.

## Decisions

- **Layering = broad: `forge-cli Provider::Local`.** The local backend lives in
  forge-cli as a real provider (in-process, file-backed). Consequence: local
  rides the existing forge-cli rail — plan-issue-cli only needs to parameterize
  the hardcoded `--provider gitlab` at `forge_cli_adapter.rs:127`, not a new
  hand-written adapter. The §3 JSON schema becomes forge-cli's on-disk
  contract, asserts become provider-uniform, and the service path (P5) is a
  natural extension (network the local backend).
- **Half-B seeding = driver-writes-JSON (v1).** The e2e driver writes
  `prs/<n>.json` directly into the store (conforming to the documented
  schema); a `seed-pr` convenience command is deferred.
- **Conformance is mandatory.** The local fake must satisfy the same contract
  suite as the real adapters, or local-green is false confidence.

## Out of Scope

- Any fixable source in `graysurf/plan-tracking-testbed` (fixtures only).
- A real VCS/CI behind the local PR half (stays seeded).
- Committing to build the P5 service (eval only).

## References

- Contract & schema spike (full detail, sibling artifact):
  `contract-schema-draft.md`.
- As-built trait + structs: `crates/plan-issue-cli/src/github.rs:10` (trait),
  `:69` (`PrMergeSummary`), `crates/plan-issue-cli/src/commands/plan.rs:9`
  (`CloseReason`).
- GitLab rail + hardcoded provider: `crates/plan-issue-cli/src/forge_cli_adapter.rs:127`.
- Third-provider recipe + capability list:
  `crates/plan-issue-cli/docs/runbooks/provider-routing-runbook.md` §3, §5
  (note the §4.1 trait-shape drift, flagged for reconcile/file).
