# Codex app-server auto-resume Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: complete; tracking issue closed
- Target scope: complete nils-cli issue #1151 through implementation, real
  provider acceptance, PR delivery, merge, and strict closeout.
- Current task: complete.
- Next task: none.
- Last updated: 2026-07-12
- Branch: `feat/1151-codex-app-server-auto-resume`
- Source document:
  `docs/plans/2026-07-12-codex-app-server-auto-resume/codex-app-server-auto-resume-discussion-source.md`
- Plan document:
  `docs/plans/2026-07-12-codex-app-server-auto-resume/codex-app-server-auto-resume-plan.md`
- Tracking issue: <https://github.com/sympoies/nils-cli/issues/1151>
- Branch/commit/PR: sympoies/nils-cli#1154 merged
  (<https://github.com/sympoies/nils-cli/pull/1154>)

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | done | Generate schemas and prove remote-TUI thread ownership | codex-app-server-auto-resume-spike-evidence.md: generated 0.144.1 schema, passive-control negative result, and exact-TUI transparent-bridge selection | Selected transparent bridge for TUI lifecycle plus a bound control connection for usage/submission. |
| 1.2 | done | Capture authoritative real usage exhaustion | codex-app-server-auto-resume-spike-evidence.md: live failed + usageLimitExceeded with exhausted same-account snapshot; no reset consumed | Requires exact structured failure and sibling negative control. |
| 2.1 | done | Add private app-server runtime supervision | crates/agent-session/src/codex_app_server.rs plus lifecycle tests: capability-probed private runtime, daemon reconnect, and explicit cleanup | Preserve raw TUI compatibility. |
| 2.2 | done | Normalize app-server turn failures safely | FailureReducer and activity projection tests: exact bound failed + usageLimitExceeded only; raw identifiers projected and content discarded | No text classifier. |
| 2.3 | done | Integrate Codex usage scheduling and continuation | Provider-scoped scheduler/control tests and real canary: authoritative reset recheck and acknowledged exactly-once same-thread continuation | Same account/thread; exactly once. |
| 3.1 | done | Complete regression, privacy, and compatibility coverage | `cargo test -p nils-agent-session --all-targets`: 239 unit and 69 integration tests passed; docs specify supported and degraded modes | Cover every issue test-first row. |
| 3.2 | done | Run repository validation | `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`: workspace fmt, clippy, docs audits, 5,946 nextest tests, and doc tests passed; completion freshness checked 62 snapshots with zero failures | Local-fast plus focused tests. |
| 4.1 | done | Run end-to-end real exhaustion acceptance canary | codex-app-server-auto-resume-canary-evidence.md: final transparent-bridge run on `sym` captured real quota failure, sibling isolation, one authorized reset, daemon restart, one visible continuation, and cleanup | Future quota/reset validation must use `sym`, never `gamania`. |
| 4.2 | done | Deliver, review, merge, and close | PR #1154 squash-merged as `8b0c5491`; 18 review threads resolved; all Linux, macOS, coverage, CodeQL, and specialist review gates passed; #1151 closed | Strict review and close-ready gates. |

## Session Log

- 2026-07-12: User requested full execution of nils-cli issue #1151 with all
  feature acceptance passing and durable final evidence.
- 2026-07-12: Classified as L2 because it is one tightly coupled runtime
  migration with one delivery PR and high-risk external acceptance.
- 2026-07-12: Created a clean managed worktree from current `origin/main`; the
  pre-existing dirty primary checkout remains untouched.
- 2026-07-12: Implemented the capability-gated private app-server runtime,
  live structured reducer, provider-scoped scheduler, acknowledged submission,
  reconnect wake, and explicit cleanup while retaining raw Codex fallback.
- 2026-07-12: Completed the real installed-Codex quota/reset canary. The target
  resumed exactly once after daemon restart; its same-account sibling remained
  unarmed; isolated auth, runtime state, tmux sessions, and sockets were removed.
- 2026-07-12: Review and a live passive-control smoke proved that app-server
  notifications are connection-local for TUI-owned turns. Replaced the passive
  monitor with a private transparent WebSocket bridge and reran the real
  exhaustion/reset/restart canary using only `sym`; one reset credit was used.
- 2026-07-12: Final CI exposed stale generated Bash/Zsh completion snapshots
  for the internal bridge subcommand. Regenerated both assets and passed the
  completion suite, 62-snapshot freshness audit, and full workspace gate.

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `agent-docs preflight --intent project-dev` | pass | Required nils-cli documents and local-fast finish-line contract resolved. | local preflight |
| Issue and archive deduplication | pass | #1151 is the single open implementation handoff; prior #1144 and agent-console #276 are closed evidence sources. | provider read-back |
| Plan validation | pass | `plan-tooling validate --explain` accepted the complete bundle with zero errors. | local validation |
| Test-first evidence | pass | Meaningful unsupported-Codex red captured before production edits; affected and manual validation recorded. | runtime-kit test-first evidence v2 |
| Focused tests | pass | 239 unit, 69 integration, and doc tests passed after the final bridge repair. | `cargo test -p nils-agent-session --all-targets` |
| Local-fast | pass | Workspace formatting, clippy, docs/parity audits, 5,946 nextest tests, and doc tests passed. | `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` |
| Completion assets | pass | Bash/Zsh syntax, Zsh completion behavior, export smoke, and 62-snapshot freshness audit passed with zero failures. | completion standard validation commands |
| Real exhaustion canary | pass | Exact structured failure, sibling negative control, one reset, daemon reconnect, and exactly one acknowledged visible continuation. | codex-app-server-auto-resume-canary-evidence.md |
| Required CI and specialist review | pass | PR #1154 merged after all Linux, macOS, coverage, CodeQL, cargo-deny, JUnit, and specialist review gates passed. | provider read-back |
