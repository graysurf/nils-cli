# plan-issue `record close` Non-Required Check Gate Fix — Source

| Field | Value |
| --- | --- |
| Status | Ready for implementation |
| Date | 2026-05-25 |
| Source | sympoies/nils-cli#502 (`plan-issue close treats non-required checks as failed`) |
| Intended next step | Implement Sprint 1 in this plan and land alongside the issue's closing PR |

## Purpose

`plan-issue record close --linked-pr ...` currently treats any failed item in
GitHub's `statusCheckRollup` as a hard close blocker, even when the linked PR
is already merged and the PR's required-check rollup is `success` (or has
`required_count = 0`). This contradicts the contract documented in
[issue-backed-plan-record-contract-v2](../../../crates/plan-issue-cli/docs/specs/issue-backed-plan-record-contract-v2.md):
`linked-pr-not-merged` should only fire when the PR is genuinely unmerged or
required checks fail, not when a non-required workflow happens to be red.

The downstream symptom observed in `graysurf/agent-runtime-kit#69` closeout:

- `record-close-gate-failed`
- `linked-pr-not-merged`
- `linked PRs: graysurf/agent-runtime-kit#103 (checks=Fail)`

…even though `forge-cli pr checks 103` reported `state=success`,
`required_count=0` and the only red item was an unrelated `scripts/ci/all.sh`
workflow.

## Confirmed facts

- The close-gate blocker string `linked PRs: <ref> (checks=Fail)` and the code
  `linked-pr-not-merged` come from
  `crates/plan-issue-cli/src/lifecycle_record.rs:1763-1780`. The gate currently
  rejects any `CheckStatus` that is neither `Pass` nor `None`. [F1]
- `CheckStatus` and `LinkedPrEvidence` are defined in
  `crates/plan-issue-cli/src/lifecycle_record.rs:1066-1080` and carry only a
  single tri-state field; they do not preserve required-vs-non-required
  classification. [F2]
- `record close` resolves linked-PR evidence in
  `crates/plan-issue-cli/src/execute.rs:1162-1204`. It calls
  `adapter.pr_merge_summary(...)` and then flattens `summary.checks` (a single
  string state) into `CheckStatus` at lines 1178-1182. [F3]
- The GitHub adapter (`crates/plan-issue-cli/src/github.rs:64,348-383,477-518`)
  fetches `state,mergeCommit,statusCheckRollup` and runs `rollup_status` to
  compute a single aggregate state; it does not surface required-only status
  or any breakdown. [F4]
- The GitLab adapter (`crates/plan-issue-cli/src/forge_cli_adapter.rs:305-349`)
  has the same shape: it forwards `forge-cli pr view`'s rolled-up `state` and
  drops per-check detail with a TODO comment acknowledging the gap. [F5]
- `forge-cli` already exposes the required-vs-non-required distinction:
  `crates/forge-cli/src/ops/pr_checks.rs` returns
  `PrChecksPayload { required_count, failed, checks: Vec<CheckItem { required, .. }> }`
  (`required_count` at line 126, `required: bool` at line 95), and
  `crates/forge-cli/src/ops/required_check_gate.rs:46-98` implements the
  correct "only block on required failures" pattern via
  `ensure_required_checks_green` + `classify`. plan-issue does not call this
  path today. [F6]
- Fixture closeout tests live at
  `crates/plan-issue-cli/tests/integration/live_record_ops.rs:648,718` and
  read PR snapshots through `read_fixture_pr_snapshot`
  (`crates/plan-issue-cli/src/execute.rs:488-534`), which also only reads
  `statusCheckRollup.state` — so any fix has to update fixtures and that
  parsing path too. [F7]
- The contract spec already names the relevant failure codes:
  `linked-pr-missing`, `linked-pr-not-merged`, and `linked-pr-checks-failed`
  (`crates/plan-issue-cli/docs/specs/issue-backed-plan-record-contract-v2.md:318`).
  Today, non-required failures collapse into `linked-pr-not-merged` instead
  of being either ignored or surfaced as a distinct code. [F8]

## Decisions

1. **Gate on required-check status, not aggregate status.** When the provider
   exposes required-check data, `record close` only blocks on a required-check
   failure or on an unmerged PR. Non-required failures are recorded as
   informational evidence in the closeout comment but do not fail the gate.
2. **Distinguish `linked-pr-not-merged` from `linked-pr-checks-failed`.** The
   contract spec already lists `linked-pr-checks-failed` as a distinct failure
   code. Wire it up so an unmerged PR and a required-check failure no longer
   share a blocker code.
3. **Reuse forge-cli's required-only path; do not re-implement.** plan-issue
   consumes `forge-cli pr checks --required-only` (or the equivalent library
   call) when available, instead of re-parsing GitHub's `statusCheckRollup`.
   The existing parser in `crates/forge-cli/src/ops/pr_checks.rs` and the
   `required_check_gate` classification stay the source of truth.
4. **Add explicit override only as a defense-in-depth knob.**
   `--allow-non-required-check-failure` is added to `RecordCloseArgs` for the
   case where the provider cannot resolve required-check state (e.g. a
   degraded `gh`/`glab` call). When set, the closeout comment records the
   override decision and the observed non-required failures.
5. **Provider-agnostic shape.** Both GitHub (`src/github.rs`) and GitLab
   (`src/forge_cli_adapter.rs`) adapters return a richer linked-PR summary
   that carries `required_count`, `required_failed`, and `required_state`.
   The lifecycle record consumes this without provider-specific branching.
6. **Out of scope.** No changes to `forge-cli` itself; it already does the
   right thing. No changes to `pr_merge_summary`'s other callers; we add a
   parallel call rather than mutate the existing signature.

## Scope

- Edit `crates/plan-issue-cli/src/lifecycle_record.rs` (`CheckStatus` /
  `LinkedPrEvidence` shape; `evaluate_strict_closeout_gate` logic) so the
  close gate distinguishes required-vs-non-required failures.
- Edit `crates/plan-issue-cli/src/github.rs` and
  `crates/plan-issue-cli/src/forge_cli_adapter.rs` so adapters report
  required-check details (count, state, failed list) in addition to the
  aggregate rollup.
- Edit `crates/plan-issue-cli/src/execute.rs` (`record close` live + fixture
  paths) so linked-PR evidence carries the new required-check fields and the
  override flag is honored.
- Add `--allow-non-required-check-failure` to `RecordCloseArgs` in
  `crates/plan-issue-cli/src/commands/record.rs`.
- Add fixtures and integration tests in
  `crates/plan-issue-cli/tests/fixtures/lifecycle/` and
  `crates/plan-issue-cli/tests/integration/live_record_ops.rs` covering:
  PR merged + zero required + non-required fail; PR merged + required pass +
  non-required fail; PR merged + required fail (must still block).

## Non-scope

- No changes to `forge-cli` `pr_checks` / `required_check_gate` semantics.
- No new `provider-cli` abstraction beyond the new adapter return fields.
- No changes to `pr_merge_summary` semantics for other callers.
- No backfill of historical closeout comments.
- No changes to the v2 contract spec's failure-code names; we just wire
  `linked-pr-checks-failed` through.

## Requirements

- R1. `record close --linked-pr <merged PR with required success + non-required fail>`
  succeeds on both fixture and live paths.
- R2. `record close --linked-pr <merged PR with required failure>` still fails
  with the new `linked-pr-checks-failed` code (not `linked-pr-not-merged`).
- R3. `record close --linked-pr <unmerged PR>` still fails with
  `linked-pr-not-merged` regardless of check state.
- R4. `--allow-non-required-check-failure` documented in `--help`, and when
  used in the override scenario writes `record-close-allow-non-required:` plus
  the observed failures into the closeout comment evidence block.
- R5. Existing `live_record_ops.rs` integration tests continue to pass without
  modification beyond fixture additions and any new assertions explicitly
  exercising the new behavior.

## Acceptance criteria

- AC-1. `cargo test -p plan-issue-cli` is green.
- AC-2. New integration test covering the three fixture scenarios above is
  added and green.
- AC-3. `plan-issue --help record close` documents the new flag; release notes
  reference issue #502.
- AC-4. Re-running the original `graysurf/agent-runtime-kit#69` closeout
  scenario against a freshly built binary no longer reports
  `record-close-gate-failed` for a non-required workflow failure.

## Validation plan

1. `cargo test -p plan-issue-cli` (worktree).
2. `cargo build --release -p plan-issue-cli` and copy binary into
   `~/.local/nils-cli/plan-issue`.
3. Re-run the original closeout shape against a disposable issue using the
   fixture path (`--fixture` directory built from #103 snapshot data) and
   verify the gate passes without overriding.
4. Run `plan-issue record close --dry-run` against a live merged PR with a
   non-required failed workflow (any nils-cli PR with a deliberately-failing
   non-required job) and confirm `ok=true`.

## Findings table

| ID | Source | Disposition |
| --- | --- | --- |
| F-1 | sympoies/nils-cli#502 + lifecycle_record.rs:1763 | In scope — gate-logic rewrite |
| F-2 | execute.rs:1178-1182 | In scope — `CheckStatus` shape change |
| F-3 | github.rs + forge_cli_adapter.rs | In scope — adapter returns required-check detail |
| F-4 | commands/record.rs:238-286 | In scope — `--allow-non-required-check-failure` flag |
| F-5 | tests/fixtures/lifecycle/ | In scope — new fixtures + tests |
| F-6 | forge-cli pr_checks / required_check_gate | Reused as-is — out of scope |

## Risks and guardrails

- **R-1**: Adapter shape change ripples into other callers of
  `pr_merge_summary`. Mitigation: add a parallel method or return the
  required-check fields as `Option<_>` so existing callers ignore them.
- **R-2**: GitLab `forge-cli pr checks` may not surface a true `required_count`
  on every project (GitLab has no first-class required-check concept; it maps
  to pipeline jobs). Mitigation: when the adapter cannot resolve a meaningful
  `required_count`, fall back to the existing aggregate `state` semantics and
  document the limitation; this matches existing GitLab gating behavior.
- **R-3**: Override flag risks erosion of the close gate's intent. Mitigation:
  always emit a discoverable trace in the closeout comment when the override
  is exercised and require an explicit non-empty reason string (rejected with
  `record-close-override-reason-missing` otherwise).

## Execution

- Recommended plan:
  `docs/plans/plan-issue-close-non-required-checks/plan-issue-close-non-required-checks-plan.md`
- Recommended execution state:
  `docs/plans/plan-issue-close-non-required-checks/plan-issue-close-non-required-checks-execution-state.md`
- Status: ready
- Next-task source: this plan's Sprint 1

## Retention intent

Promote after merge. The required-vs-non-required distinction is the canonical
contract for any future plan-issue lifecycle gate that consumes provider check
state, so the structured evidence shape introduced here should outlive this
PR.

## Read-first references

- `crates/plan-issue-cli/src/lifecycle_record.rs:1066-1080` (`CheckStatus`,
  `LinkedPrEvidence`)
- `crates/plan-issue-cli/src/lifecycle_record.rs:1755-1782` (close gate's
  linked-PR branch)
- `crates/plan-issue-cli/src/execute.rs:1160-1204` (live linked-PR resolution)
- `crates/plan-issue-cli/src/execute.rs:488-534` (fixture linked-PR snapshot
  parser)
- `crates/plan-issue-cli/src/github.rs:64,348-383,477-518`
  (`PrMergeSummary`, `pr_merge_summary`, `rollup_status`)
- `crates/plan-issue-cli/src/forge_cli_adapter.rs:305-349` (GitLab adapter
  `pr_merge_summary`)
- `crates/plan-issue-cli/src/commands/record.rs:238-286` (`RecordCloseArgs`)
- `crates/plan-issue-cli/tests/integration/live_record_ops.rs:648,718` (close
  gate fixture tests)
- `crates/plan-issue-cli/tests/fixtures/lifecycle/agent-runtime-kit-closeout/prs/sympoies__agent-runtime-kit__1.json`
  (existing PR fixture shape)
- `crates/plan-issue-cli/docs/specs/issue-backed-plan-record-contract-v2.md:318`
  (failure-code contract)
- `crates/forge-cli/src/ops/pr_checks.rs:95,123-126`,
  `crates/forge-cli/src/ops/required_check_gate.rs:46-98` (source of truth
  for required-only classification)

## Source type

`discussion-to-implementation-doc`
