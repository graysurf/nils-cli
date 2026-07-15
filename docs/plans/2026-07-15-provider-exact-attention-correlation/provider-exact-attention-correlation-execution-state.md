# Provider Exact Attention Correlation Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: complete; tracking issue closed
- Target scope: one provider-neutral v1 invariant, runtime-selected Codex
  attention authority with app-server exact resolution, capability-selected
  Claude exact-or-limited Elicitation, and an explicit conservative limitation
  for generic permission dialogs.
- Execution window: Sprint 1 shared boundary -> Sprints 2 and 3 independent /
  parallel provider lanes -> Sprint 4 integration and delivery.
- Current task: none; tracking issue closed
- Next task: none; tracking issue closed
- Last updated: 2026-07-16
- Branches: `feat/provider-exact-attention-correlation`,
  `fix/claude-attention-shadow`, and `docs/provider-attention-closeout`
- Source document:
  `docs/plans/2026-07-15-provider-exact-attention-correlation/provider-exact-attention-correlation-plan.md`
- Implementation source:
  `docs/plans/2026-07-15-provider-exact-attention-correlation/provider-exact-attention-correlation-discussion-source.md`
- Tracking issue: <https://github.com/sympoies/nils-cli/issues/1237>
- Branch/commit/PR: sympoies/nils-cli#1239 merged
  (<https://github.com/sympoies/nils-cli/pull/1239>);
  sympoies/nils-cli#1241 merged
  (<https://github.com/sympoies/nils-cli/pull/1241>);
  sympoies/nils-cli#1242 merged
  (<https://github.com/sympoies/nils-cli/pull/1242>)

## Validation Plan

- Validate the plan bundle and docs-only repository gate before opening the
  tracker.
- Preserve the already-green shared invariant, then capture meaningful
  provider-lane test-first failures before production edits.
- Re-audit the installed Codex typed request-id schema and sanitize Claude
  Elicitation fixtures before selecting exact capability.
- Run focused reducer/proxy/setup/doctor tests plus nils-cli local-fast.
- Require specialist review, provider checks, and pre-lifecycle-boundary live
  polling/SSE/Agent Console acceptance before final closeout.

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | done | Freeze shared invariant and authority-mode contract | Focused reducer and AskUserQuestion baseline passed; contract update in worktree.; Authority-mode contract recorded in crates/agent-session/docs/turn-state-contract.md; three focused activity baselines passed. | No source pairing; provider lanes become independent. |
| 2.1 | done | Verify Codex capability and capture test-first evidence | pending; Codex 0.144.3 generated schema confirms string-or-int64 RequestId and serverRequest/resolved; four compile-valid focused red tests retained in test-first evidence. | Schema and red-test artifacts retained under agent-out; production edits now authorized. |
| 2.2 | done | Implement typed Codex projection and fail-closed reduction | pending; Implementation started from verified red tests.; Typed string/int64 projection, five-method allowlist, exact 2-to-1-to-0 clearing, privacy, idempotence, malformed/queue fail-close, and activity degradation tests pass. | v1 wire contract retained; only opaque projected ids persist. |
| 2.3 | done | Implement Codex runtime source arbitration and capability status | pending; Runtime extra plus tmux env select protocol or hook authority; generic protocol-authority permission reporter suppresses at source; mismatch and projection loss remain unknown until a new generation; doctor reports exact capability and policy. | Codex 0.144.3 supported; versions outside exact audit remain unverified. |
| 3.1 | done | Verify Claude capability and capture branch-specific test-first evidence | pending; Official Claude hooks contract and installed 2.1.210 confirm optional elicitation_id on Elicitation and ElicitationResult; sanitized live canary remains.; Sanitized Claude 2.1.210 MCP canary: form emitted request/result with no elicitation_id; URL emitted no callbacks. Both are conservative terminal outcomes; artifacts retained under agent-out. | No raw message, URL, schema, content, or provider id retained. |
| 3.2 | done | Implement selected Claude exact or conservative branch | pending; Claude setup adds Elicitation/ElicitationResult; same nonempty id maps to exact runtime-scoped request/clear, missing request id latches conservatively, missing result id is ignored; form/URL/privacy/setup tests pass. | AskUserQuestion and generic permission behavior remain unchanged; rollback disables only the new admission/setup entries. |
| 4.1 | done | Publish capability status and run integration acceptance | Contract and evidence docs, doctor/setup migration, live Claude AskUserQuestion clear, live Codex completion, polling, authenticated SSE, staged-binary/tmux PATH parity, and first/subsequent-prompt retitle tests all pass. | Same-id clear before `Stop` is verified where exact is supported; generic no-id permissions remain conservative by design. |
| 4.2 | done | Deliver implementation PRs and close tracker | nils-cli PRs #1239 and #1241 and sympoies-infra PR #116 merged; `agent-session 1.22.5 (v1.22.5-2-g2fc5602d)` is installed and active in `agent-console-serve.service`; sanitized acceptance evidence retained under agent-out. | Completion audit passed; tracker is ready to close after this docs-only state update merges. |

## Session Log

- 2026-07-15: Reassessment confirmed that the original producer, persistence,
  SSE, setup, and consumer work shipped under v1. The remaining issue is exact
  provider request correlation, not a v2 redesign.
- 2026-07-15: The user required the original Codex/Claude shared-design goal to
  remain explicit. The plan converged on one v1 invariant, separate provider
  evidence adapters, and no false exact-clear claim for generic permissions.
- 2026-07-15: Plan archive and open-issue audit found related foundations but no
  duplicate. Archived #1151/#1154 own app-server auto-resume; open #1118 owns
  Codex hook representation convergence.
- 2026-07-15: Pre-merge specialist review rejected uncorrelated Codex
  hook/protocol reconciliation as unimplementable. The plan now selects one
  attention authority per runtime, preserves typed exact ids through semantic
  deduplication, treats request/resolution loss asymmetrically, and makes
  Claude's conservative capability result a valid independent lane outcome.
- 2026-07-15: Opened L2 tracker #1237 from the committed bundle and initialized
  run `20260715T155230Z-issue-1237` at Sprint 1, Task 1.1. The tracker remains
  open for the later implementation session.
- 2026-07-15: API-contract follow-up ruled out treating a duplicate
  `PermissionRequest` hook as progress because it must not advance
  `last_progress_at` or change pending presentation.
- 2026-07-15: Red-team follow-up proved that hook-only and hook-before-protocol
  traces are indistinguishable. Protocol authority now requires proven request
  completeness plus hook source suppression; a hook that bypasses suppression
  degrades the runtime instead of being ignored or paired.
- 2026-07-15: Resumed run `20260715T161951Z-issue-1237` in managed worktree
  `feat/provider-exact-attention-correlation`. The existing exact-clear,
  progress-never-clears, and AskUserQuestion correlation regressions passed
  before the shared authority contract was edited.
- 2026-07-15: Task 1.1 completed. The provider-neutral v1 correlation and
  one-authority-per-runtime rules are now explicit; Codex capability evidence
  and failing adapter fixtures are the active scope.
- 2026-07-15: Tasks 2.1 through 3.2 completed. Codex 0.144.3 uses the app-server
  protocol as one immutable exact attention authority; Claude uses the same v1
  contract with exact same-id Elicitation when available and a conservative
  latch otherwise. Mixed-source or projection failure degrades the runtime
  until a new generation instead of guessing.
- 2026-07-15: The sanitized Claude 2.1.210 MCP canary observed form
  `Elicitation` and `ElicitationResult` without `elicitation_id`; the deployed
  capability must therefore remain conservative on this installed version.
  The URL branch emitted no callbacks and remains unverified-conservative.
- 2026-07-15: Test-first evidence is complete. Four provider-correlation tests
  failed before production edits and now pass; the affected crate, local-fast,
  docs-only, and plan gates are green. Pre-merge specialist review is active.
- 2026-07-15: First-wave specialist review found request-id reuse, wrong-turn
  scope, MCP mode classification, raw-id privacy, projection lock contention,
  transport/exact capability coupling, rollback compatibility, and coverage
  gaps. The implementation now uses per-occurrence opaque tokens, rejects
  wrong-turn requests, maps MCP modes explicitly, retains no raw request id,
  separates transport from exact capability, source-guards the installed hook
  command, and persists a generation-scoped unhealthy marker without waiting
  indefinitely on the activity lock. The expanded crate suite and Clippy pass;
  reviewer follow-up remains active.
- 2026-07-15: Follow-up API, maintainability, and red-team review found public
  revision reset, unbound-turn scope, marker serialization/corruption,
  unverified installed source suppression, and proxy transport-loss paths. The
  second fix wave gives the marker a stable monotonic public state, serializes
  degradation with activity/auto-resume through the session-record lock, fails
  closed on invalid markers, binds the first non-null exact turn id, requires
  the guarded installed command before protocol authority, and degrades on
  proxy EOF/read/write/malformed-data failure. Final follow-up and full gates
  remain active.
- 2026-07-15: Retitle integration was reassessed at the user's request. Codex
  and Claude already trigger first-prompt and subsequent-prompt auto-retitles
  through the independent `provider-prompt.v1` attach channel, while completion
  fallback already consumes authoritative v1 turn state. Folding prompt text
  into the metadata-only activity pipeline would violate the privacy and
  failure-isolation boundary, so no cross-repo feature edit is warranted. The
  deployed acceptance must instead prove both prompt retitles still work after
  the activity changes.
- 2026-07-15: Final red-team lock review found that a one-shot authority breach
  could time out behind the session-record lock before poisoning the runtime.
  The fail-close path now writes a runtime-scoped pending marker first, with a
  stable timestamp, and a dedicated health fence orders that write against
  activity commits and the durable auto-resume submission claim. Parseable
  markers must themselves contain a runtime-owned `unknown` state, and Codex
  protocol authority is rejected when any second direct unguarded permission
  reporter exists. Held-lock, record-update stability, invalid-state, and mixed
  guarded/unguarded regressions pass. The final full gates and API-contract,
  maintainability, and red-team testing follow-ups all passed with no blocking
  findings; delivery and deployed acceptance remain active.
- 2026-07-15: The first final-HEAD GitHub coverage run exposed a macOS-only
  test portability failure before coverage measurement: the runner's temporary
  directory made the configured app-server test socket exceed the Unix path
  budget. The three runtime-allocation tests now create their private runtime
  roots directly under `/tmp`; focused authority/source-guard/transport tests
  pass and the provider-visible failed check remains the retained red evidence.
- 2026-07-16: PR #1239 merged the shared provider-neutral contract and exact
  Codex/conditional-Claude correlation. Live Claude acceptance then exposed a
  duplicate AskUserQuestion permission shadow and a long-lived tmux PATH that
  could select an older helper.
- 2026-07-16: Follow-up nils-cli PR #1241 and coordinated sympoies-infra PR
  #116 retired the duplicate managed Claude notifier, suppressed the
  AskUserQuestion shadow, and pinned each spawned/resumed tmux session to the
  staged daemon PATH. All Linux, macOS, coverage, CodeQL, focused, and local
  validation gates passed.
- 2026-07-16: The merged binary was installed and the service restarted. A
  fresh Claude session transitioned `working -> needs_input(clarification) ->
  working -> waiting(completed)` without a duplicate latch; a fresh Codex
  session completed as `provider_hook/authoritative`. Polling and
  authenticated SSE passed, and both tmux sessions selected the installed
  helper path.
- 2026-07-16: Retitle was retained on the independent metadata-safe
  `provider-prompt.v1` channel. Focused Agent Console tests passed for the
  immediate first prompt, each distinct later prompt, replay suppression, and
  an initial summary still in flight. Activity-to-retitle coupling was
  deliberately not added because it would duplicate one prompt across two
  event sources.

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `agent-docs session activate` for `project-dev,task-tools` | pass | Required repository and external-fact policies activated. | local preflight |
| `plan-archive search` for turn-state/activity/app-server plans | pass | Found app-server foundation; no exact-attention duplicate. | local catalog |
| Open nils-cli issue audit | pass | #1118 is related configuration work, not this scope. | GitHub issue list |
| Installed version audit | pass | Claude 2.1.210, Codex 0.144.3, agent-session 1.22.4. | local runtime |
| `plan-tooling validate` | pass | Repaired eight-task graph validates with zero errors. | local |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` | pass | Repaired three-file bundle passes all docs checks. | local |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` | pass | Repository correctly selected docs-only mode and passed. | local |
| Focused activity reducer and AskUserQuestion baselines | pass | Exact ids clear independently, progress never clears attention, and existing Claude exact correlation remains green. | local worktree |
| Four provider-correlation red-to-green regressions | pass | Codex typed projection and independent 2-to-1-to-0 clearing, semantic-id preservation, and Claude exact-or-conservative normalization pass. | `20260716-003843-provider-exact-attention-test-first/test-first-evidence.json` |
| `cargo test -p nils-agent-session` | pass | After first-wave review fixes, 400 unit tests and 94 integration tests passed. | local worktree |
| `cargo clippy -p nils-agent-session --all-targets --all-features -- -D warnings` | pass | Expanded production and test paths compile with zero warnings. | local worktree |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` | pass | Formatting, Clippy with denied warnings, docs gates, 487 nextest tests, and doctests passed. | local worktree |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` | pass | Strict documentation and CLI-contract checks passed. | local worktree |
| Final `cargo test -p nils-agent-session` | pass | 403 unit and 94 integration tests passed after all marker, health-fence, source-guard, and proxy-loss review fixes. | local worktree |
| Final `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` | pass | Docs gates, formatting, denied-warning Clippy, 497 nextest tests, and doctests passed. | local worktree |
| Final specialist follow-up | pass | API-contract, maintainability, and red-team testing reviewers found no blocking issues after the health-fence, marker, source-guard, and proxy-loss fixes. | PR #1239 review evidence |
| macOS socket-path CI regression | red-to-green | GitHub coverage failed in `configured_app_server_runtime_selects_protocol_attention_authority` because its temporary Unix socket path exceeded the platform budget; all three runtime-allocation tests pass with short private `/tmp` roots. | GitHub Actions run `29440660587`, job `87438487132`; local focused rerun |
| Claude 2.1.210 MCP Elicitation canary | pass | Form request/result omitted ids and selected conservative behavior; URL remained unverified-conservative. | `20260716-010014-claude-elicitation-canary/result.json` |
| `plan-tooling validate --file ... --explain` | pass | The eight-task plan bundle validates with zero errors. | local worktree |
| PR #1239 GitHub required checks | pass | Linux, macOS, coverage, CodeQL, and cargo-deny passed before merge. | GitHub Actions run `29441064625` |
| PR #1241 follow-up validation | pass | 499/499 local-fast tests, 95/95 integration tests, and all updated-head GitHub checks passed. | GitHub Actions run `29445657482` |
| sympoies-infra helper-path validation | pass | Focused fail-closed override regressions and post-review `make validate` passed. | serenvia/sympoies-infra#116 |
| Live deployment acceptance | pass | Installed service binary and tmux helper PATH agree; Claude exact clarification clears; Codex completes authoritatively; polling and authenticated SSE snapshots pass. | `20260716-024029-provider-exact-attention-acceptance/README.md` |
| Agent Console retitle acceptance | pass | First prompt, each distinct later prompt, replay suppression, and in-flight initial summary tests passed 3/3. | `packages/ui/test/dashboardHooks.test.ts` focused run |

## Handoff

- Tracking issue <https://github.com/sympoies/nils-cli/issues/1237> is closed;
  terminal execution state is synchronized. No closeout or merge action
  remains.
