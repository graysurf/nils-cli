# Advisory Session Coordination Implementation Handoff

## Status

- Date: 2026-07-20
- Status: approved for L2 implementation, review, PR delivery, and merge;
  release/runtime deployment requires a final exact preview and explicit
  maintainer approval.
- Source: maintainer clarification, nils-cli issue #1318, current nils-cli
  `agent-session` implementation, and current agent-runtime-kit hook sources.
- Intended next step: execute the linked L2 plan through merged code, then stop
  at the deployment consent boundary with an exact preview.

## Purpose

Change agent-session coordination from mandatory mutation admission into an
optional awareness service. Managed sessions should automatically learn when
another managed session may overlap their repository, worktree, provider
reference, or task scope, while ordinary iTerm-launched agents remain usable
without agent-session metadata or manual claim bookkeeping.

## Confirmed Facts

- `session-coordination-guard.py` currently blocks managed mutation when an
  authenticated work-context claim is absent, expired, uncovered, conflicted,
  or temporarily unavailable.
- `checkout-lease-guard.py` independently blocks a foreign active checkout,
  dirty unowned checkout, or missing lease identity, so changing only the
  work-context guard cannot produce advisory behavior.
- `agent-session work-context claim` requires agents to discover and populate a
  private JSON document plus session, capability, revision, idempotency, and
  lifecycle details that the launcher already knows.
- A fresh guarded session can therefore be unable to construct the file needed
  to acquire the claim required for its first mutation.
- The agent-session launcher and broker already know the session incarnation,
  working directory, repository context, lifecycle, and heartbeat needed for
  automatic presence.
- Plain Codex or Claude processes launched outside agent-session may not have
  any agent-session identity, and the maintainer does not require them to
  participate.

## Maintainer Intent

- Coordination exists to prevent accidental collisions through timely
  reminders, not to make agent work contingent on perfect orchestration state.
- Advisory behavior is the default for agent-session-managed Codex and Claude
  sessions.
- Unmanaged sessions bypass agent-session coordination; collision resolution
  can occur through ordinary Git and provider workflows.
- Strong enforcement may remain as an explicit opt-in mode for workflows that
  intentionally want claim/admission semantics.
- A managed session must always have a bounded escape hatch when coordination
  is unavailable, stale, or intentionally overridden.
- Mechanical context setup should be launcher/CLI-owned. Agents should not need
  to discover private schemas or manually supply IDs, capability paths,
  revisions, idempotency keys, or renewals.

## Decisions

- Add session coordination modes `advisory`, `enforce`, and `off`, with
  `advisory` as the default for new managed sessions.
- Treat the broker-backed managed-session lifecycle as automatic presence;
  presence registration/refresh/removal follows start, resume, heartbeat,
  stop, and delete rather than a separate mandatory claim ceremony.
- Keep the raw authenticated claim/admit/complete API for compatibility and
  explicit `enforce` workflows.
- Add high-level self-targeting work-context commands that infer session
  identity and repository/worktree context, own idempotency/CAS details, and
  expose simple status/set/clear operations.
- In advisory mode, evaluate overlap and emit privacy-safe, deduplicated
  warnings, but never deny a mutation solely because presence, claim,
  coordination, or checkout-lease state is missing, stale, conflicting, or
  unavailable.
- In `off` mode, agent-session coordination hooks are silent and non-blocking.
- In `enforce` mode, retain the existing claim coverage, conflict, operation
  lease, and physical checkout writer protections.
- Unmanaged launches bypass both agent-session semantic coordination and its
  exclusive checkout-lease enforcement.
- Preserve unrelated security, consent, secret, Git delivery, project-intent,
  and finish-line validation hooks.
- Add a session-bounded override/acknowledgement surface so a managed agent can
  proceed after an advisory without editing runtime configuration.

## Warning Model

- Same physical worktree: strong warning with privacy-safe peer/session
  summary and suggested coordination action.
- Same issue, PR, provider reference, or overlapping declared path: strong
  warning.
- Same repository but different worktree and no stronger overlap: informational
  warning.
- Missing/old CLI, broker failure, or malformed coordination state: degraded
  warning once per bounded deduplication window, then continue.
- Repeated equivalent warnings are deduplicated; warning text never exposes
  capability material, raw local paths, prompts, logs, or peer message bodies.

## Scope

- nils-cli `agent-session` command model, session record/public JSON contract,
  broker lifecycle, work-context high-level UX, compatibility behavior, tests,
  docs, and generated completion assets where required.
- agent-runtime-kit session-coordination and checkout-lease hooks, shared
  policy/templates, managed/unmanaged routing, advisory warning behavior,
  enforce/off compatibility, and privacy/acceptance tests.
- Two repository PRs, independent specialist review, required CI, merge,
  tracking issue checkpoints, deployment preview, approved release/runtime
  sync, and fresh-session acceptance.

## Non-Scope

- Replacing Git conflict resolution, provider branch protection, or formal L3
  dispatch ownership.
- Making unmanaged iTerm agents discoverable or claim-aware.
- Weakening user-consent, secret, signing, delivery-route, project-intent, or
  validation gates unrelated to session collision awareness.
- Reading prompts, transcripts, terminal buffers, raw host/user identity, or
  capability material to infer work context.

## Requirements

1. New managed sessions default to advisory coordination and automatically
   publish lifecycle-bound presence.
2. Advisory overlap or coordination failure never blocks Bash, Write, Edit,
   apply_patch, provider mutation, or the first useful task mutation.
3. Unmanaged Codex and Claude sessions remain fully usable without
   agent-session metadata.
4. Explicit enforce mode preserves existing authenticated claim/admission and
   checkout writer protection.
5. Explicit off mode removes agent-session coordination warnings without
   disabling unrelated safety hooks.
6. High-level context UX infers self identity and repository/worktree data and
   never requires a hand-authored private JSON file for normal use.
7. Warnings distinguish worktree, provider/task/path, repository-only, and
   degraded-state overlap without leaking private runtime data.
8. Warning repetition is bounded and agents have a session-scoped
   acknowledgement/override path.
9. Existing raw work-context clients and stored records remain readable and
   enforce-mode compatible.
10. Cross-product acceptance proves managed advisory, managed enforce/off,
    unmanaged launch, broker degradation, fresh-session first mutation, and
    overlapping-session behavior.

## Acceptance Criteria

- A fresh managed Codex or Claude session can mutate immediately with no claim;
  any peer overlap is warning-only in the default mode.
- Two managed sessions in the same physical worktree receive a strong warning
  and both remain able to proceed.
- Two managed sessions in distinct worktrees of the same repository receive an
  informational warning without false same-worktree wording.
- A direct iTerm-launched Codex or Claude session is not blocked by missing
  agent-session identity or coordination state.
- `agent-session work-context status|set|clear` can operate on the current
  session without manual session IDs, capability paths, private JSON files,
  revisions, or idempotency values.
- `--coordination-mode enforce` retains definite-conflict, uncovered-scope, and
  pending-operation blocking behavior; `off` remains silent.
- A bounded advisory acknowledgement/override prevents warning spam while
  preserving future overlap visibility.
- Invalid capability data and raw claim mutation remain protected; advisory
  defaults do not turn private control APIs into unauthenticated APIs.
- nils-cli and agent-runtime-kit local validation, provider checks, privacy
  canaries, specialist review, and cross-product acceptance pass.
- Deployment occurs only after the maintainer approves an exact release/runtime
  sync preview, then installed managed/unmanaged fresh-session smoke passes.

## Validation Plan

- Capture meaningful test-first red evidence in each repository before
  production edits.
- Run focused nils-agent-session unit/integration/JSON/completion tests and the
  nils-cli `--local-fast` gate.
- Run runtime-kit focused hook/routing/privacy tests and repository validation
  against the nils-cli source worktree.
- Run testing, security, API/compatibility, and maintainability specialist
  reviews before merge.
- Require provider checks, review-thread sweep, and task sweep to pass for both
  PRs.
- Before deployment, present the exact nils-cli version/base/command, runtime
  sync target/digest/config preview, rollback, and live acceptance commands.
- After explicit approval, release/install, sync runtime surfaces, and test
  managed Codex, managed Claude, and unmanaged iTerm-style launches.

## Risks and Guardrails

- Checkout coordination is shared with mutation isolation; mode routing must
  not accidentally weaken enforce mode or unrelated Git delivery rules.
- Existing serialized session/claim records require additive defaults and
  compatibility fixtures.
- A warning hook must remain deterministic and fast; broker failure degrades to
  a bounded warning rather than mutation latency or denial.
- Provider-visible evidence must use privacy-safe session summaries and omit
  raw machine-local paths.
- Release and installed-home/runtime sync are explicit consent boundaries even
  after both implementation PRs merge.

## Execution

Recommended plan: docs/plans/2026-07-20-advisory-session-coordination/advisory-session-coordination-plan.md

Recommended execution state: docs/plans/2026-07-20-advisory-session-coordination/advisory-session-coordination-execution-state.md

- Status: execute now through merged nils-cli and agent-runtime-kit PRs; pause
  at the deployment preview consent boundary.
- Next-task source: Sprint 1, Task 1.1 in the recommended plan.
- Retention intent: transient L2 plan source; archive with the completed bundle.

## Read-First References

- `crates/agent-session/src/main.rs`
- `crates/agent-session/src/lib.rs`
- `crates/agent-session/src/coordination/`
- `crates/agent-session/docs/specs/session-coordination-v1.md`
- `agent-runtime-kit/core/hooks/shared/session-coordination-guard.py`
- `agent-runtime-kit/core/hooks/shared/checkout-lease-guard.py`
- `agent-runtime-kit/core/policies/session-coordination.md`
