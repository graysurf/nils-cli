# Codex Optional Usage Windows Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: complete; tracking issue closed
- Target scope: optional Codex usage windows in nils-cli, nils-cli release and
  local install, sympoies-infra reader delivery/deploy, and live Agent Console
  recovery smoke.
- Execution window: Sprint 1 (nils-cli behavior and tests) -> Sprint 2 (nils-cli
  PR, release, install) -> Sprint 3 (reader PR, deploy, live smoke), serial.
- Current task: none; tracking issue closed
- Next task: none; tracking issue closed
- Last updated: 2026-07-12
- Branch/commit/PR: sympoies/nils-cli#1165 merged (<https://github.com/sympoies/nils-cli/pull/1165>); sympoies/nils-cli#1174 merged (<https://github.com/sympoies/nils-cli/pull/1174>)
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
| 2.1 | done | Deliver and merge the nils-cli PR | #1165 squash-merged as `4024423e`; 11 checks passed, review threads/tasks were zero, and six specialist lenses approved after repairs. | Dynamic and legacy contracts merged with no tracker close. |
| 2.2 | done | Release and install nils-cli | #1168 merged as `f4fd48cf`; `v1.21.23` release run `29208620090` and tap run `29209447625` passed; brew and local release installs report 1.21.23. | Installed live JSON, table, and prompt show Weekly without stale 5h. |
| 3.1 | done | Update and deliver the sympoies-infra reader | graysurf/sympoies-infra#77 squash-merged as `2aaa541c`; 99 reader tests, 140 related tests, and `make validate` passed. | Dynamic windows are authoritative; legacy summary, variable duration, and PII boundaries remain covered. |
| 3.2 | done | Deploy and run live end-to-end smoke | Deploy run `29210284309` passed; reader/edge expose three fresh Weekly-only accounts with zero sensitive keys; full smoke and rendered-browser acceptance passed. | Agent Console shows live Weekly meters for gamania, poies, and sym with no 5h card or Codex `—`. |

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
- 2026-07-13: #1165 passed all required checks and specialist follow-ups, then
  squash-merged. Release PR #1168 also passed Linux, macOS, coverage, cargo-deny,
  and CodeQL gates; `v1.21.23` published four platform assets and updated the
  Homebrew tap. The host brew install and 48-binary local release build both
  completed at 1.21.23 while preserving the existing brew pin.
- 2026-07-13: The sympoies-infra reader was delivered test-first in #77 after
  testing, API, and security reviewers found and verified fixes for one-window
  symmetry, variable non-weekly durations, semantic label filtering, and plan
  allowlisting. The main deploy workflow applied the reader and passed its
  stack smoke.
- 2026-07-13: Independent live readback showed three fresh Codex provider rows,
  each containing only a Weekly window and no sensitive keys. The shell table
  used `-` for Non-weekly, the prompt rendered only `W:83%`, and the browser
  Usage dialog rendered Weekly meters for gamania, poies, and sym without a 5h
  card or Codex `—`.

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
| nils-cli PR #1165 required checks and specialist review | pass | 11 checks passed; all threads/tasks resolved; testing, maintainability, API, security, performance, and red-team follow-ups approved. | GitHub PR |
| Release PR #1168, `v1.21.23`, and Homebrew tap | pass | Release PR gates, strict tagged-commit CI, four platform builds, GitHub Release, tap update, brew upgrade, and local release install passed. | GitHub Actions / local install |
| sympoies-infra #77 validation and review | pass | Test-first red captured; 99 reader tests, 140 related tests, `make validate`, and testing/API/security follow-ups passed. | GitHub PR / test-first evidence |
| `scripts/smoke-agent-console.sh` | pass | Host helpers, reader parity, edge, layout, cross-origin gates, loopback bind, tailnet TLS, and WebSocket attach passed. | live sympoies host |
| Live reader, edge, and rendered Usage dialog | pass | Three fresh Weekly-only Codex entries; no 5h window, no sensitive keys, and live meters rendered instead of `—`. | live API / browser acceptance |

## Handoff

- Tracking issue <https://github.com/sympoies/nils-cli/issues/1162> is closed;
  terminal execution state is synchronized. No closeout or merge action remains.
