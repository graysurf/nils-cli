# Plan: Advisory-by-Default Agent Session Coordination

## Overview

Replace mandatory session collision admission with automatic, lifecycle-bound
presence and privacy-safe warnings. Keep strong claims and checkout writer
leases available through explicit enforce mode, add a silent off mode and a
bounded advisory escape hatch, and make ordinary work-context operations infer
all mechanical session/repository metadata.

## Read First

- Primary source: `docs/plans/2026-07-20-advisory-session-coordination/advisory-session-coordination-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Open questions carried into execution: none

## Scope

- In scope: nils-cli coordination modes, automatic presence, self-targeting
  work-context UX, additive JSON/docs/completion contracts, runtime-kit
  session/checkout hook routing and warnings, compatibility tests, two PRs,
  review/merge, deployment preview, approved release/runtime sync, and live
  fresh-session acceptance.
- Out of scope: mandatory participation by unmanaged agents, replacement of
  Git/provider conflict resolution, or weakening unrelated safety hooks.

## Assumptions

1. Broker/session lifecycle is the authoritative source of managed-session
   liveness; no separate claim is required to prove presence.
2. Repository/worktree/provider/task hints can be projected without prompts,
   transcripts, logs, terminal bytes, or capability material.
3. Existing raw claim/admit/complete clients require compatibility and retain
   their authenticated semantics in enforce mode.

## Sprint 1: nils-cli Coordination Contract

**Goal**: Make advisory coordination and automatic managed-session presence a
stable nils-cli contract while preserving explicit enforcement compatibility.

**Demo/Validation**:

- Commands: focused nils-agent-session tests and JSON/help/completion checks.
- Verify: a default managed session reports advisory mode and presence without
  a raw claim; enforce/off modes and old records remain compatible.

### Task 1.1: Lock mode, presence, and compatibility behavior with failing tests

- **Location**:
  - `crates/agent-session/src/`
  - `crates/agent-session/tests/`
- **Description**: Add behavioral fixtures for advisory default, explicit
  enforce/off, old session/claim records, automatic lifecycle presence,
  overlap classification, and privacy-safe public projection; capture
  meaningful pre-edit failures.
- **Dependencies**:
  - none
- **Complexity**: 6
- **Acceptance criteria**:
  - New tests fail for the missing mode/presence/advisory contract before
    production edits.
  - Existing raw work-context behavior remains covered.
- **Validation**:
  - `cargo test -p nils-agent-session coordination`

### Task 1.2: Implement automatic presence and coordination modes

- **Location**:
  - `crates/agent-session/src/lib.rs`
  - `crates/agent-session/src/session_record.rs`
  - `crates/agent-session/src/coordination/`
- **Description**: Add additive session mode state and environment projection,
  derive managed-session presence from live broker/session records, classify
  privacy-safe repository/worktree/context overlap, and bind lifecycle cleanup
  to existing broker semantics.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 9
- **Acceptance criteria**:
  - New managed sessions default to advisory; callers can explicitly choose
    enforce or off.
  - Presence needs no raw claim and disappears when its broker/session is no
    longer live.
  - Public output excludes raw paths, capability data, and private claim data.
- **Validation**:
  - `cargo test -p nils-agent-session coordination`

### Task 1.3: Add self-targeting work-context and advisory UX

- **Location**:
  - `crates/agent-session/src/main.rs`
  - `crates/agent-session/src/coordination/`
  - `crates/agent-session/docs/`
  - `completions/`
- **Description**: Add high-level status/set/clear and advisory
  acknowledge/override operations that infer the current session,
  capability/state paths, repository/worktree scope, revision, and
  idempotency. Retain raw authenticated primitives for advanced/enforce use and
  document the mode/JSON/help contract.
- **Dependencies**:
  - Task 1.2
- **Complexity**: 8
- **Acceptance criteria**:
  - Normal managed use does not require a private JSON file or manually copied
    IDs/revisions.
  - Unmanaged calls return an explicit unavailable/not-managed result without
    claiming participation or blocking work.
  - Help, JSON contracts, docs, and generated completions describe all modes
    and escape hatches.
- **Validation**:
  - `cargo test -p nils-agent-session`
  - `zsh -n completions/zsh/_agent-session`
  - `bash -n completions/bash/agent-session`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`

## Sprint 2: agent-runtime-kit Advisory Hooks

**Goal**: Route managed mutation through warning-only coordination by default,
while keeping enforce/off behavior explicit and leaving unmanaged sessions
usable.

**Demo/Validation**:

- Commands: focused shared-hook tests, cross-product source-linked tests, and
  runtime-kit repository validation.
- Verify: advisory conflicts and broker failure warn/allow; enforce blocks;
  off/unmanaged bypass; privacy canaries pass.

### Task 2.1: Lock hook mode routing with failing tests

- **Location**:
  - `tests/test_session_coordination_guard.py`
  - `tests/test_checkout_lease_guard.py`
  - product hook rendering/contract tests
- **Description**: Add managed advisory/enforce/off and unmanaged fixtures for
  Bash, edit/apply_patch, provider mutation, PostTool/Failure/Stop, broker
  degradation, same-worktree/repository overlap, warning deduplication, and
  privacy canaries; capture meaningful pre-edit failures.
- **Dependencies**:
  - Task 1.3
- **Complexity**: 7
- **Acceptance criteria**:
  - Default-mode tests fail on current blocking behavior for the expected
    denial reason.
  - Enforce-mode regression fixtures preserve current hard-block cases.
- **Validation**:
  - focused Python hook tests through the repo-owned test entrypoints

### Task 2.2: Implement advisory, enforce, off, and unmanaged routing

- **Location**:
  - `core/hooks/shared/session-coordination-guard.py`
  - `core/hooks/shared/checkout-lease-guard.py`
  - shared hook helpers/templates
- **Description**: Use the new nils-cli advisory evaluation in managed default
  mode, emit bounded/deduplicated privacy-safe warnings and always allow,
  preserve operation leases and exclusive checkout enforcement only in
  enforce mode, make off silent, and bypass session coordination for incomplete
  unmanaged metadata.
- **Dependencies**:
  - Task 2.1
- **Complexity**: 9
- **Acceptance criteria**:
  - Missing claim, peer overlap, unavailable broker, or checkout lease conflict
    cannot block advisory/off/unmanaged work.
  - Enforce mode still rejects missing/uncovered/conflicting claims and foreign
    checkout ownership with authenticated recovery guidance.
  - Unrelated hook gates remain registered and behavior-compatible.
- **Validation**:
  - focused shared-hook and product-render tests

### Task 2.3: Update policy and cross-product acceptance

- **Location**:
  - `core/policies/session-coordination.md`
  - runtime hook specs/templates/goldens
  - runtime-kit validation scripts
- **Description**: Replace mandatory-by-default policy language with the
  advisory contract, document enforce/off and escape hatches, refresh product
  surfaces, and prove fresh managed/unmanaged sessions against the nils-cli
  source worktree.
- **Dependencies**:
  - Task 2.2
- **Complexity**: 7
- **Acceptance criteria**:
  - Codex and Claude renders share the same mode semantics.
  - Source-linked acceptance proves first mutation, overlap warning,
    acknowledgement, enforce denial, off silence, unmanaged bypass, and
    privacy boundaries.
- **Validation**:
  - runtime-kit repo-defined focused and full validation with
    `RUNTIME_COORD_EVIDENCE_DIR` bound

## Sprint 3: Review, PR Delivery, and Merge

**Goal**: Deliver both repositories through independent review and provider
gates while keeping #1318 synchronized as the L2 tracker.

**Demo/Validation**:

- Commands: semantic commits, `forge-cli pr deliver --no-merge`, specialist
  reviews, thread/task sweeps, required checks, and `forge-cli pr merge`.
- Verify: both merged heads contain the validated cross-product contract and
  #1318 records current task/validation/review truth.

### Task 3.1: Deliver and merge nils-cli

- **Location**:
  - `sympoies/nils-cli` provider PR
- **Description**: Commit the plan, contract, implementation, tests, docs, and
  completion assets; deliver without merge; resolve specialist findings and
  provider checks; checkpoint #1318; squash merge.
- **Dependencies**:
  - Task 1.3
- **Complexity**: 6
- **Acceptance criteria**:
  - Test-first/docs evidence, local-fast, review outcome, threads, tasks, and
    required checks pass.
  - The linked nils-cli PR is squash-merged to main.
- **Validation**:
  - `forge-cli pr deliver --no-merge` and `forge-cli pr merge`

### Task 3.2: Deliver and merge agent-runtime-kit

- **Location**:
  - `graysurf/agent-runtime-kit` provider PR
- **Description**: Commit hook, policy, render, and acceptance changes; deliver
  without merge; resolve specialist findings and provider checks; checkpoint
  #1318; squash merge.
- **Dependencies**:
  - Task 2.3
  - Task 3.1
- **Complexity**: 6
- **Acceptance criteria**:
  - Runtime validation, cross-product acceptance, review outcome, threads,
    tasks, and required checks pass.
  - The linked runtime-kit PR is squash-merged to main.
- **Validation**:
  - `forge-cli pr deliver --no-merge` and `forge-cli pr merge`

## Sprint 4: Deployment Consent and Live Acceptance

**Goal**: Present exact deployment inputs, deploy only after approval, and
close #1318 only after installed managed/unmanaged behavior is proven.

**Demo/Validation**:

- Commands: exact nils-cli release-and-deploy preview, runtime surface sync
  preview, approved execution, installed binaries, and fresh-session smoke.
- Verify: installed defaults are advisory, enforce/off remain explicit,
  unmanaged iTerm-style agents work, and overlapping managed sessions warn
  without blocking.

### Task 4.1: Prepare and obtain approval for the deployment preview

- **Location**:
  - merged nils-cli and runtime-kit main heads
  - release/runtime sync tooling
- **Description**: Compute the exact next patch version, approved base/head,
  release command, runtime-kit source commit, sync target, preview digest/config,
  rollback, and live test matrix; report all implemented outcomes and wait for
  explicit maintainer approval.
- **Dependencies**:
  - Task 3.2
- **Complexity**: 4
- **Acceptance criteria**:
  - No release, installed-home mutation, or live runtime sync occurs before
    exact preview approval.
  - Preview binds every command and immutable version/commit/digest input.
- **Validation**:
  - repo-owned release/runtime preview outputs

### Task 4.2: Release, sync, and prove fresh sessions

- **Location**:
  - nils-cli GitHub release/Homebrew/local install
  - installed Codex and Claude runtime surfaces
- **Description**: After approval, execute the exact release and runtime sync,
  verify installed versions/config, then start disposable managed Codex,
  managed Claude, overlapping managed, enforce/off, and unmanaged iTerm-style
  sessions for end-to-end acceptance.
- **Dependencies**:
  - Task 4.1
- **Complexity**: 7
- **Acceptance criteria**:
  - Released/installed binaries and synced hooks match the approved preview.
  - Default managed sessions warn/allow, enforce blocks expected conflicts,
    off is silent, and unmanaged sessions work without claims.
  - #1318 passes strict close-ready, read-back audit, and closes.
- **Validation**:
  - release/runtime sync receipts and privacy-safe fresh-session smoke

## Testing Strategy

- Unit: mode parsing/defaults, additive serialization, self identity inference,
  overlap severity, warning deduplication, override expiry, and legacy records.
- Integration: broker lifecycle presence, work-context high-level commands,
  raw enforce compatibility, generated CLI/help/completion contracts, and hook
  event routing.
- Cross-product: runtime-kit hooks invoke the nils-cli source worktree for
  advisory/enforce/off/unmanaged and broker-degraded scenarios.
- Live/manual: released binary and installed Codex/Claude hooks in disposable
  sessions, including same-worktree overlap and direct iTerm-style launch.

## Risks & gotchas

- Mode defaults cross a stored-record boundary; serde/JSON defaults must keep
  old records readable without silently weakening an explicitly enforcing
  record.
- Advisory checkout behavior cannot leave a stale exclusive lease that later
  misrepresents ownership; enforce-only lease mutation needs dedicated tests.
- Hook latency and warning spam can make advisory coordination unusable;
  bounded probing and deterministic deduplication are acceptance requirements.
- Cross-repository implementation must use separate managed worktrees and PRs;
  #1318 remains the single durable tracker.
- Deployment mutates installed runtime surfaces and remains separately gated by
  an exact preview and explicit approval.

## Rollback plan

- Revert the runtime-kit PR to restore the previous hook semantics without
  deleting stored claims; sync only after an explicitly approved rollback
  preview.
- Revert the nils-cli PR and release a follow-up patch if additive session
  records or work-context commands regress compatibility.
- Before a fixed rollback release is available, set managed sessions to
  explicit `off` through the supported mode surface; do not disable unrelated
  safety hooks.

