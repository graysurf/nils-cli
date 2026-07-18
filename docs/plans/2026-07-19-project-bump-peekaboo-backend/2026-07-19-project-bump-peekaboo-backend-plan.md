# Plan: Add the project-local Peekaboo backend bump skill

## Overview

Add `project-bump-peekaboo-backend` to prepare a reviewable, exact-tag update
of the locked Peekaboo backend. The skill will orchestrate a released nils-cli
dry-run/apply primitive, retain the prior trusted release as rollback material,
and fail closed before changing the lock when any supply-chain, compatibility,
or macOS trust check is incomplete.

## Read First

- Primary source:
  `docs/plans/2026-07-19-project-bump-peekaboo-backend/2026-07-19-project-bump-peekaboo-backend-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Open questions carried into execution: none

## Scope

- In scope: a reusable nils-cli lock-update primitive, the project-local skill
  and tests, exact-tag inspection, deterministic dry-run/apply, rollback
  retention, public-safe evidence, and macOS trust/capability validation.
- Out of scope: floating/minimum-version resolution, Sparkle self-update, live
  candidate installation, TCC/trust-store changes, deployment, release
  publication, or private launcher changes.

## Assumptions

1. Upstream continues to publish separate CLI and app assets addressable by an
   explicit immutable tag and commit.
2. macOS-only trust checks remain required before apply, while pure planning
   and fixture validation remain cross-platform.
3. A notarization exception is never inferred from an older locked release.

## Sprint 1: Freeze the update contract with meaningful red tests

**Goal**: Define the exact-tag, no-mutation, trust, and rollback contract before
production behavior is added.

**Demo/Validation**:

- Command(s): focused Rust and shell tests selected during Task 1.1.
- Verify: each new test fails for the expected missing behavior, not setup,
  compilation, network, or fixture errors.

### Task 1.1: Map affected contracts and add failing cross-platform tests

- **Location**:
  - `crates/macos-agent/src/lock.rs`
  - `crates/macos-agent/src/backend/mod.rs`
  - `crates/macos-agent/tests/`
  - `.agents/skills/project-bump-peekaboo-backend/tests/test_project_bump_peekaboo_backend.sh`
- **Description**: Classify affected tests/fixtures, then add deterministic
  failing tests for explicit-tag input, dry-run non-mutation, rejected floating
  versions, rollback retention, idempotent apply, and failure-before-write.
- **Dependencies**:
  - none
- **Complexity**: 5
- **Acceptance criteria**:
  - Test-first evidence dispositions every materially affected test/fixture.
  - New tests fail on the old implementation for the intended contract.
  - Linux-capable tests use local fixtures and make no live provider,
    credential, TCC, trust-store, or macOS GUI mutation.
- **Validation**:
  - Focused red commands and `test-first-evidence check --phase pre-edit`.

### Task 1.2: Specify the planner and apply JSON contracts

- **Location**:
  - `crates/macos-agent/README.md`
  - `crates/macos-agent/src/cli.rs`
  - `crates/macos-agent/tests/`
- **Description**: Define the released nils-cli command surface consumed by the
  skill, including explicit inputs, versioned JSON, typed failures, evidence
  fields, apply guards, and exit codes.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 4
- **Acceptance criteria**:
  - The command requires an exact tag and rejects `latest`, ranges, and `>=`.
  - Dry-run and apply share one validated plan schema; apply binds to the exact
    reviewed plan and candidate artifacts.
  - JSON exposes no credentials, private host inventory, or unnecessary local
    absolute paths.
- **Validation**:
  - CLI help/JSON contract tests and fixture-backed documentation examples.

## Sprint 2: Implement the deterministic nils-cli primitive

**Goal**: Produce a candidate update only after complete release, artifact,
compatibility, and trust verification.

**Demo/Validation**:

- Command(s): focused `macos-agent` unit/integration tests on Linux and macOS.
- Verify: dry-run is read-only; apply produces only the expected lock,
  compatibility assertion, fixture, test, and documentation changes.

### Task 2.1: Build exact-tag release and artifact planning

- **Location**:
  - `crates/macos-agent/src/`
  - `crates/macos-agent/tests/fixtures/peekaboo-lock-update/release.json`
- **Description**: Resolve the explicit tag to immutable metadata, inspect
  isolated downloads, verify safe archives/hashes, inspect executable/app
  metadata, and render the candidate trust tuple without editing the repo.
- **Dependencies**:
  - Task 1.2
- **Complexity**: 7
- **Acceptance criteria**:
  - The plan records tag, commit, release time, license, asset URLs/hashes,
    executable hashes, architecture, bridge build, bundle id, signing identity
    and team, notarization result, and capability probes.
  - Archive traversal and symlink protections remain enforced.
  - Missing, ambiguous, mutable, or mismatched input yields a typed failure and
    no repository diff.
- **Validation**:
  - Deterministic/malformed fixtures and macOS `lipo`/`codesign`/`spctl` tests.

### Task 2.2: Apply an approved plan and retain exact rollback material

- **Location**:
  - `crates/macos-agent/peekaboo-lock.json`
  - `crates/macos-agent/src/lock.rs`
  - `crates/macos-agent/src/backend/mod.rs`
  - `crates/macos-agent/tests/`
  - `BINARY_DEPENDENCIES.md`
  - `crates/macos-agent/README.md`
- **Description**: Apply only a still-current reviewed plan, move the previous
  trusted current release into `rollback_releases`, and update exact
  version/build assertions, fixtures, tests, and docs through a reference
  inventory.
- **Dependencies**:
  - Task 2.1
- **Complexity**: 7
- **Acceptance criteria**:
  - The prior current release becomes the newest exact rollback, and the last
    viable rollback is never deleted automatically.
  - Reapplying the same approved plan is idempotent.
  - The v3.9.3 CLI waiver is not copied forward; a new waiver requires separate
    exact-tuple approval.
  - A stale plan, changed artifact, failed trust check, or failed capability
    probe leaves the repository unchanged.
- **Validation**:
  - Apply/idempotency/stale-plan, rollback, and focused backend/lock tests.

## Sprint 3: Add the project-local orchestration skill

**Goal**: Expose the primitive through a small workflow that prepares a normal
reviewed PR without broadening runtime authority.

**Demo/Validation**:

- Command(s): skill-owned shell tests and repository skill audits.
- Verify: the skill plans first, calls the canonical primitive, presents
  public-safe evidence, and routes delivery through repository policy.

### Task 3.1: Scaffold and implement `project-bump-peekaboo-backend`

- **Location**:
  - `.agents/skills/project-bump-peekaboo-backend/SKILL.md`
  - `.agents/skills/project-bump-peekaboo-backend/scripts/project-bump-peekaboo-backend.sh`
  - `.agents/skills/project-bump-peekaboo-backend/tests/test_project_bump_peekaboo_backend.sh`
  - `.agents/skills/project-bump-peekaboo-backend/references/UPDATE_CONTRACT.md`
- **Description**: Add the skill and thin script to validate an explicit tag,
  invoke plan/apply, present dry-run evidence, and prepare the scoped diff and
  validation handoff.
- **Dependencies**:
  - Task 2.2
- **Complexity**: 5
- **Acceptance criteria**:
  - Canonical source lives under `.agents/skills`; the existing Claude bridge
    exposes it without a duplicate source.
  - The skill does not implement another updater, scrape floating releases, run
    Sparkle, install the app, alter TCC/trust stores, or deploy.
  - Script tests cover exact-tag rejection, plan-first ordering, failed-plan
    no-apply, safe argument forwarding, and public-safe reporting.
- **Validation**:
  - Skill tests, shell syntax, and project skill-name/governance audits.

### Task 3.2: Document inputs, evidence, and rollback expectations

- **Location**:
  - `.agents/skills/project-bump-peekaboo-backend/SKILL.md`
  - `.agents/skills/project-bump-peekaboo-backend/references/UPDATE_CONTRACT.md`
  - `crates/macos-agent/README.md`
- **Description**: Document the exact input, dry-run review, waiver approval,
  macOS validation, rollback guarantee, and PR versus release/deploy boundary.
- **Dependencies**:
  - Task 3.1
- **Complexity**: 3
- **Acceptance criteria**:
  - Operators can identify the tuple, validation, residual risk, and rollback
    without reading implementation code.
  - Public issue/PR text contains no credentials, private host details, or
    machine-local paths.
- **Validation**:
  - Documentation references and skill dry-run output review.

## Sprint 4: Validate and deliver through the L2 gates

**Goal**: Prove the behavior on cross-platform fixtures and macOS, then deliver
one independently reviewed PR.

**Demo/Validation**:

- Command(s): focused suites, docs/skill audits, local-fast checks, and a macOS
  no-apply run against the current explicit tag.
- Verify: ledger and review gates are complete before merge; release/deploy
  remain separate.

### Task 4.1: Run cross-platform and macOS validation

- **Location**:
  - repository validation output
  - test-first evidence record
- **Description**: Run deterministic Linux planner/apply coverage, macOS
  trust/capability acceptance without live install, and the nils-cli local-fast
  gate.
- **Dependencies**:
  - Task 3.2
- **Complexity**: 5
- **Acceptance criteria**:
  - Focused and affected suites pass on supported platforms.
  - macOS validates architecture, signing, notarization policy, bridge build,
    and capability execution in no-apply mode.
  - Acceptance changes no TCC/trust-store state, credential, or live managed
    installation.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` plus the
    implementation's focused macOS command.

### Task 4.2: Deliver, review, merge, and close the tracker

- **Location**:
  - tracking issue and linked PR
- **Description**: Deliver one feature PR, run testing, maintainability, and
  supply-chain review, resolve findings, merge after the issue review
  checkpoint, then complete strict closeout and archive handoff.
- **Dependencies**:
  - Task 4.1
- **Complexity**: 4
- **Acceptance criteria**:
  - Test-first, validation, native review, thread/task, and checked-ledger gates
    pass for the reviewed provider head.
  - Merge creates no release, deploy, upstream mutation, or live installation.
- **Validation**:
  - PR delivery/review, `tracking close-ready --expect-visible`, provider
    read-back, and archive dry-run.

## Testing Strategy

- Unit: tag parsing, plan binding, lock transformation, rollback retention,
  notarization policy, archive safety, and typed errors.
- Integration: fixture-backed planning, dry-run/apply idempotency, stale-plan
  rejection, CLI JSON/help, and skill wrapper ordering.
- E2E/manual: macOS no-apply validation of the current release's architecture,
  signing/notarization posture, bridge build, and capability surface.

## Risks & gotchas

- Asset names, signing teams, architectures, or bundle structure may change;
  every difference is review evidence, not an auto-trusted value.
- CLI and app notarization can differ; v3.9.3's waiver must not become a
  version-family exception.
- Apply must bind to reviewed immutable commit/hashes instead of refetching
  unbound inputs.
- Fixture tests cannot establish macOS trust or TCC/GUI readiness.

## Rollback plan

- Before merge, revert branch changes; active lock and installed runtime remain
  untouched.
- After a future bump, use the prior exact release in `rollback_releases`; do
  not synthesize rollback from mutable receipts or unreviewed downloads.
- Reverting the skill/primitive must not delete the last viable rollback or
  modify installed apps, TCC, credentials, or trust stores.
