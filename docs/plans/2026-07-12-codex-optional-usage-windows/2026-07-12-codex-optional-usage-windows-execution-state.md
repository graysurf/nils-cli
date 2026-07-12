# Codex Optional Usage Windows Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: active; bundle prepared, tracker pending
- Target scope: optional Codex usage windows in nils-cli, nils-cli release and
  local install, sympoies-infra reader delivery/deploy, and live Agent Console
  recovery smoke.
- Execution window: Sprint 1 (nils-cli behavior and tests) -> Sprint 2 (nils-cli
  PR, release, install) -> Sprint 3 (reader PR, deploy, live smoke), serial.
- Current task: Task 1.1 - lock optional-window behavior with failing tests.
- Next task: Task 1.2 - generalize rate-limit parsing and derived output.
- Last updated: 2026-07-12
- Branch/commit/PR: `feat/codex-optional-usage-windows`; no PR yet.
- Source document: `docs/plans/2026-07-12-codex-optional-usage-windows/2026-07-12-codex-optional-usage-windows-plan.md`
- Implementation source: `docs/plans/2026-07-12-codex-optional-usage-windows/2026-07-12-codex-optional-usage-windows-discussion-source.md`
- Direct source-doc execution waiver: not applicable.
- Tracking issue: pending

## Validation Plan

- Validate the bundle and docs-only checks before opening the tracker.
- Capture meaningful test-first red evidence before Rust production edits.
- Run focused codex-cli and agent-session tests, then nils-cli local-fast.
- Require provider checks and specialist review before the nils-cli merge.
- Verify release assets, tap update, and installed binary against live API.
- Run sympoies-infra usage-reader and repository validation before its PR.
- Deploy through the repo-owned host path and run safe live API/UI smoke.

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | pending | Lock optional-window behavior with failing tests | pending | Cover weekly-only, non-weekly-only, empty, malformed, two-window, cache, and PII cases. |
| 1.2 | pending | Generalize rate-limit parsing and derived output | pending | Dynamic windows are authoritative; summary remains compatible. |
| 1.3 | pending | Refresh caches, prompt output, and diagnostics safely | pending | Remove obsolete windows and PII without regressing legacy caches. |
| 2.1 | pending | Deliver and merge the nils-cli PR | pending | Full L2 review and provider gates. |
| 2.2 | pending | Release and install nils-cli | pending | Tag, release, tap, local install, live API smoke. |
| 3.1 | pending | Update and deliver the sympoies-infra reader | pending | Prefer windows; retain summary fallback. |
| 3.2 | pending | Deploy and run live end-to-end smoke | pending | Prompt/table/reader/UI all current and non-stale. |

## Session Log

- 2026-07-12: Live API diagnosis confirmed that all configured accounts return
  a 604800-second primary window with `secondary_window: null`; nils-cli 1.21.22
  rejected the response, shell surfaces served stale cache, and Agent Console
  rendered empty Codex usage. The user approved L2 execution through release,
  host deploy, and live smoke. A clean managed worktree was created from fresh
  `origin/main`; the unrelated dirty shared checkout remains untouched.

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `plan-archive search` for rate-limit and usage-reader plans | pass | No matching archived plan or open tracker was found. | local |
| `plan-tooling validate --file docs/plans/2026-07-12-codex-optional-usage-windows/2026-07-12-codex-optional-usage-windows-plan.md --format text --explain` | pass | Bundle validation passed with zero errors. | local |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` | pass | Docs placement, hygiene, markdown lint, plan-bundle, CLI contract, and forge fixture gates passed. | local |
