# Claude Usage Fallback Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: complete; tracking issue closed
- Target scope: add native Claude usage source selection to `claude-cli`, update
  `sympoies-infra` to consume it, deliver PRs, and deploy the reader.
- Execution window: Sprint 1 (`nils-cli`) -> Sprint 2 (`sympoies-infra`) ->
  Sprint 3 (delivery/deploy), strictly serial.
- Current task: complete; deployed for validation.
- Next task: none.
- Last updated: 2026-07-07
- Branch/commit/PR: sympoies/nils-cli#1047 merged (<https://github.com/sympoies/nils-cli/pull/1047>); graysurf/sympoies-infra#43 merged (<https://github.com/graysurf/sympoies-infra/pull/43>)
- Source document:
  `docs/plans/2026-07-07-claude-usage-fallback/claude-usage-fallback-discussion-source.md`
- Plan document:
  `docs/plans/2026-07-07-claude-usage-fallback/claude-usage-fallback-plan.md`
- Direct source-doc execution waiver: not applicable.
- Tracking issue: <https://github.com/sympoies/nils-cli/issues/1046>
- Source snapshot: recorded in issue #1046.
- Plan snapshot: recorded in issue #1046.
- Initial state snapshot: recorded in issue #1046.

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
| 1.1 | done | Add failing coverage for auto source fallback | test-first evidence: cargo test added before production edit; `$HOME/.local/state/agent-runtime-kit/out/projects/sympoies__nils-cli/20260708-055325-test-first-evidence` | Failing coverage captured before implementing usage command. |
| 1.2 | done | Implement JSON usage command and cache contract | sympoies/nils-cli#1047 @ 9c81a5fa; cargo test -p nils-claude-cli pass | JSON usage command and cache-compatible contract implemented. |
| 1.3 | done | Implement CLI `/usage` fallback parser | sympoies/nils-cli#1047 @ 9c81a5fa; installed claude-cli source=cli smoke pass | PTY /usage fallback parser implemented and covered. |
| 2.1 | done | Switch infra usage reader to `claude-cli usage` JSON | graysurf/sympoies-infra#43 @ a1a64f1; python3 scripts/test-agent-console-usage.py pass | Reader consumes claude-cli usage JSON with degrade coverage. |
| 2.2 | done | Update infra runbook docs and devlog | graysurf/sympoies-infra#43 @ a1a64f1; docs/devlog updated | Runbook/devlog describe nils-cli ownership boundary. |
| 3.1 | done | Deliver nils-cli PR and install/release local binary | sympoies/nils-cli#1047 merged; `$HOME/.local/nils-cli/bin/claude-cli usage --format json --source cli` pass | Local binary installed on sympoies. |
| 3.2 | done | Deliver infra PR and deploy | graysurf/sympoies-infra#43 merged; make deploy STACK=agent-console pass; /api/usage Claude ok=true stale=false | Stack deployed and smoke passed. |

## Session Log

- 2026-07-07: Classified as L2 at user direction (not L3). Created a managed
  `nils-cli` worktree from `main` and assembled this plan bundle with the method
  inventory. Selected `claude-cli usage --format json --source auto` as the
  directly shippable approach; rejected infra-owned PTY parsing, web-cookie
  fallback, Admin API, and JSONL quota estimation for this delivery.
- 2026-07-07: Delivered and merged `sympoies/nils-cli#1047` and
  `graysurf/sympoies-infra#43`. Installed the new `claude-cli` on sympoies,
  restarted `agent-console-usage.service`, deployed the agent-console stack, and
  verified `/api/usage` reports Claude as `ok=true` and `stale=false`.

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `plan-tooling validate --file docs/plans/2026-07-07-claude-usage-fallback/claude-usage-fallback-plan.md --format text --explain` | pass | Plan Format v1 clean; 0 errors. | local |
| `cargo test -p nils-claude-cli` | pass | `claude-cli usage` tests pass. | test-first evidence |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` | pass | nils-cli local-fast suite pass. | test-first evidence |
| `python3 scripts/test-agent-console-usage.py` | pass | infra usage reader tests pass. | test-first evidence |
| `bash -n scripts/*.sh host/agent-console/bin/run-agent-console-usage host/agent-console/bin/agent_console_usage.py && make config STACK=agent-console` | pass | infra syntax and compose config pass. | test-first evidence |
| `make deploy STACK=agent-console` | pass | Stack deployed; smoke PASS including usage panel and tailnet WebSocket attach probe. | sympoies deploy |
