# Codex app-server auto-resume Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: active
- Target scope: complete nils-cli issue #1151 through implementation, real
  provider acceptance, PR delivery, merge, and strict closeout.
- Current task: 4.2 deliver, review, merge, and close.
- Next task: commit, rebase onto current `origin/main`, and deliver the PR without
  merging so independent review can run.
- Last updated: 2026-07-12
- Branch: `feat/1151-codex-app-server-auto-resume`
- Source document:
  `docs/plans/2026-07-12-codex-app-server-auto-resume/codex-app-server-auto-resume-discussion-source.md`
- Plan document:
  `docs/plans/2026-07-12-codex-app-server-auto-resume/codex-app-server-auto-resume-plan.md`
- Tracking issue: <https://github.com/sympoies/nils-cli/issues/1151>

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | done | Generate schemas and prove remote-TUI thread ownership | codex-app-server-auto-resume-spike-evidence.md: generated 0.144.1 schema plus real remote-TUI/control-client topology proof | Select control-client or transparent-bridge topology. |
| 1.2 | done | Capture authoritative real usage exhaustion | codex-app-server-auto-resume-spike-evidence.md: live failed + usageLimitExceeded with exhausted same-account snapshot; no reset consumed | Requires exact structured failure and sibling negative control. |
| 2.1 | done | Add private app-server runtime supervision | crates/agent-session/src/codex_app_server.rs plus lifecycle tests: capability-probed private runtime, daemon reconnect, and explicit cleanup | Preserve raw TUI compatibility. |
| 2.2 | done | Normalize app-server turn failures safely | FailureReducer and activity projection tests: exact bound failed + usageLimitExceeded only; raw identifiers projected and content discarded | No text classifier. |
| 2.3 | done | Integrate Codex usage scheduling and continuation | Provider-scoped scheduler/control tests and real canary: authoritative reset recheck and acknowledged exactly-once same-thread continuation | Same account/thread; exactly once. |
| 3.1 | done | Complete regression, privacy, and compatibility coverage | `cargo test -p nils-agent-session`: 214 unit and 69 integration tests passed; docs specify supported and degraded modes | Cover every issue test-first row. |
| 3.2 | done | Run repository validation | `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`: fmt, clippy, docs audits, 283 nextest tests, and doc tests passed | Local-fast plus focused tests. |
| 4.1 | done | Run end-to-end real exhaustion acceptance canary | codex-app-server-auto-resume-canary-evidence.md: real quota failure, sibling isolation, one authorized reset, daemon restart, one visible continuation, and cleanup | Retain content-free evidence and clean runtime. |
| 4.2 | pending | Deliver, review, merge, and close | pending | Strict review and close-ready gates. |

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

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `agent-docs preflight --intent project-dev` | pass | Required nils-cli documents and local-fast finish-line contract resolved. | local preflight |
| Issue and archive deduplication | pass | #1151 is the single open implementation handoff; prior #1144 and agent-console #276 are closed evidence sources. | provider read-back |
| Plan validation | pass | `plan-tooling validate --explain` accepted the complete bundle with zero errors. | local validation |
| Test-first evidence | pass | Meaningful unsupported-Codex red captured before production edits; affected and manual validation recorded. | runtime-kit test-first evidence v2 |
| Focused tests | pass | 214 unit, 69 integration, and doc tests passed. | `cargo test -p nils-agent-session` |
| Local-fast | pass | Formatting, clippy, docs/parity audits, 283 nextest tests, and doc tests passed. | `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` |
| Real exhaustion canary | pass | Exact structured failure, sibling negative control, one reset, daemon reconnect, and exactly one acknowledged visible continuation. | codex-app-server-auto-resume-canary-evidence.md |
| Required CI and specialist review | pending | Delivery gate. | pending |
