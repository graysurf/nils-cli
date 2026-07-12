# Codex Optional Usage Windows Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: active; nils-cli implementation validated, PR delivery in progress
- Target scope: optional Codex usage windows in nils-cli, nils-cli release and
  local install, sympoies-infra reader delivery/deploy, and live Agent Console
  recovery smoke.
- Execution window: Sprint 1 (nils-cli behavior and tests) -> Sprint 2 (nils-cli
  PR, release, install) -> Sprint 3 (reader PR, deploy, live smoke), serial.
- Current task: Task 2.1 - deliver and merge the nils-cli PR.
- Next task: Task 2.2 - release and install nils-cli.
- Last updated: 2026-07-12
- Branch/commit/PR: `feat/codex-optional-usage-windows`; implementation commit and PR pending.
- Source document: `docs/plans/2026-07-12-codex-optional-usage-windows/2026-07-12-codex-optional-usage-windows-plan.md`
- Implementation source: `docs/plans/2026-07-12-codex-optional-usage-windows/2026-07-12-codex-optional-usage-windows-discussion-source.md`
- Direct source-doc execution waiver: not applicable.
- Tracking issue: <https://github.com/sympoies/nils-cli/issues/1162>

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
| 1.1 | done | Lock optional-window behavior with failing tests | Test-first evidence captured meaningful pre-edit failure for weekly-only JSON; focused regression tests now pass. | Cover weekly-only, non-weekly-only, empty, malformed, two-window, cache, and PII cases. |
| 1.2 | done | Generalize rate-limit parsing and derived output | Optional primary/secondary parser, dynamic windows, summary projection, text/table output, and writeback implemented; live API returns one Weekly window per account. | Dynamic windows are authoritative; summary remains compatible. |
| 1.3 | done | Refresh caches, prompt output, and diagnostics safely | Weekly-only cache/prompt refresh removes stale 5h data; identity PII redaction added; local-fast passed. | Remove obsolete windows and PII without regressing legacy caches. |
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
- 2026-07-12: Weekly-only parsing, dynamic output, cache replacement, prompt
  rendering, writeback, and PII redaction were implemented test-first. The
  required local-fast gate passed, and a safe live query returned one current
  Weekly window for each of the three configured accounts with zero sensitive
  keys in the diagnostic output.

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `plan-archive search` for rate-limit and usage-reader plans | pass | No matching archived plan or open tracker was found. | local |
| `plan-tooling validate --file docs/plans/2026-07-12-codex-optional-usage-windows/2026-07-12-codex-optional-usage-windows-plan.md --format text --explain` | pass | Bundle validation passed with zero errors. | local |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` | pass | Docs placement, hygiene, markdown lint, plan-bundle, CLI contract, and forge fixture gates passed. | local |
| `cargo test -p nils-codex-cli --test integration -- --skip parity_oracle` | pass | All 290 affected codex-cli integration tests passed. | local |
| `cargo test -p nils-agent-session --test integration cli::serve_usage_returns_partial_provider_results_from_helpers -- --exact` | pass | Agent-session accepted a weekly-only Codex helper result. | local |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` | pass | Formatting, clippy, docs, package tests, parity checks, and doctests passed for nils-agent-session and nils-codex-cli. | local |
| Live `target/debug/codex-cli diag rate-limits --all --format json` safe projection | pass | Three accounts succeeded with one Weekly window each; sensitive-key count was zero. | local live API |
