# Claude Usage Fallback Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: planned; tracking issue pending.
- Target scope: add native Claude usage source selection to `claude-cli`, update
  `sympoies-infra` to consume it, deliver PRs, and deploy the reader.
- Execution window: Sprint 1 (`nils-cli`) -> Sprint 2 (`sympoies-infra`) ->
  Sprint 3 (delivery/deploy), strictly serial.
- Current task: create the L2 tracking issue and capture test-first evidence.
- Next task: implement Sprint 1 in `nils-cli`.
- Last updated: 2026-07-07
- Branch/commit/PR: pending.
- Source document:
  `docs/plans/2026-07-07-claude-usage-fallback/claude-usage-fallback-discussion-source.md`
- Plan document:
  `docs/plans/2026-07-07-claude-usage-fallback/claude-usage-fallback-plan.md`
- Direct source-doc execution waiver: not applicable.
- Tracking issue: <https://github.com/sympoies/nils-cli/issues/1046>
- Source snapshot: pending.
- Plan snapshot: pending.
- Initial state snapshot: pending.

## Validation Plan

- Bundle creation: `plan-tooling validate --file
  docs/plans/2026-07-07-claude-usage-fallback/claude-usage-fallback-plan.md
  --format text --explain`.
- nils-cli test-first: failing integration test for `claude-cli usage --format
  json --source auto` CLI fallback before production edits.
- nils-cli final: `cargo test -p nils-claude-cli`; `bash
  scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`.
- sympoies-infra final: `python3 scripts/test-agent-console-usage.py`; shell
  syntax checks; `make config STACK=agent-console`.
- Deploy smoke: install new `claude-cli`, restart `agent-console-usage.service`,
  call loopback `/usage`, run `scripts/smoke-agent-console.sh`.

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | pending | Add failing coverage for auto source fallback | pending | Current code lacks `claude-cli usage`; the first test should fail before production edits. |
| 1.2 | pending | Implement JSON usage command and cache contract | pending | Secret-free service JSON; keep prompt-segment compatible. |
| 1.3 | pending | Implement CLI `/usage` fallback parser | pending | Fake `claude` integration test; bounded subprocess; cache only on success. |
| 2.1 | pending | Switch infra usage reader to `claude-cli usage` JSON | pending | Python tests cover success/degrade paths. |
| 2.2 | pending | Update infra runbook docs and devlog | pending | Ownership boundary: `claude-cli` owns source selection. |
| 3.1 | pending | Deliver nils-cli PR and install/release local binary | pending | `claude-cli usage --format json --source auto` available on sympoies. |
| 3.2 | pending | Deliver infra PR and deploy | pending | Service restarted; `/api/usage` smoke green. |

## Session Log

- 2026-07-07: Classified as L2 at user direction (not L3). Created a managed
  `nils-cli` worktree from `main` and assembled this plan bundle with the method
  inventory. Selected `claude-cli usage --format json --source auto` as the
  directly shippable approach; rejected infra-owned PTY parsing, web-cookie
  fallback, Admin API, and JSONL quota estimation for this delivery.

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `plan-tooling validate --file docs/plans/2026-07-07-claude-usage-fallback/claude-usage-fallback-plan.md --format text --explain` | pass | Plan Format v1 clean; 0 errors. | local |
