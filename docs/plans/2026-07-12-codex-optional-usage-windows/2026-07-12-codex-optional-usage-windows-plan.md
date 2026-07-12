# Plan: Codex Optional Usage Windows and Agent Console Recovery

## Overview

Restore Codex usage reporting when the upstream service independently omits or
nulls either rate-limit window. Make `windows` the authoritative dynamic JSON
contract while retaining the v1 summary projection, update prompt/table caches
without inventing a five-hour window, release and install nils-cli, then update
and deploy the live Agent Console reader through `sympoies-infra`.

## Read First

- Primary source: `docs/plans/2026-07-12-codex-optional-usage-windows/2026-07-12-codex-optional-usage-windows-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Open questions carried into execution: none

## Scope

- In scope: optional primary/secondary Codex windows; dynamic JSON/text/cache
  output; PII-safe diagnostics; `agent-session /usage` compatibility; nils-cli
  PR, release, and local install; `sympoies-infra` reader compatibility, PR,
  deploy, and live smoke.
- Out of scope: changing OpenAI entitlements, fabricating removed windows,
  redesigning the Agent Console UI, or changing Claude usage behavior.

## Assumptions

1. A present well-formed window is live data even when the sibling window is
   absent or null.
2. The five-hour window may return, so two-window behavior remains supported.
3. The current Agent Console UI can render any non-empty list of normalized
   windows without production component changes.

## Sprint 1: nils-cli Contract and Implementation

**Goal**: Make every codex-cli usage surface accurately represent zero, one,
or two upstream windows without stale fabrication or PII leakage.

**Demo/Validation**:

- Commands: focused codex-cli tests and
  `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
- Verify: weekly-only payloads exit zero, emit one live window, and refresh
  prompt/table/cache state without a five-hour value.

### Task 1.1: Lock optional-window behavior with failing tests

- **Location**:
  - `crates/codex-cli/tests/integration/`
  - `crates/codex-cli/src/rate_limits/`
- **Description**: Add fixtures for two windows, weekly-only, non-weekly-only,
  no windows, malformed windows, cache refresh, text/table output, JSON output,
  and PII redaction; capture meaningful pre-edit failures.
- **Dependencies**:
  - none
- **Complexity**: 4
- **Acceptance criteria**:
  - Weekly-only behavior fails against the pre-change implementation for the
    expected invalid-payload or stale-cache reason.
  - Existing two-window behavior remains covered.
- **Validation**:
  - `cargo test -p nils-codex-cli rate_limits`

### Task 1.2: Generalize rate-limit parsing and derived output

- **Location**:
  - `crates/codex-cli/src/rate_limits/render.rs`
  - `crates/codex-cli/src/rate_limits/mod.rs`
  - `crates/codex-cli/src/rate_limits/writeback.rs`
- **Description**: Represent primary and secondary independently, derive the
  dynamic window list first, preserve summary compatibility with nullable
  non-weekly/weekly projections, and render only windows actually returned.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 7
- **Acceptance criteria**:
  - Either valid window succeeds independently.
  - Missing/null windows are omitted, malformed present windows remain errors,
    and an entirely empty response degrades as no active window.
  - Two-window output remains behavior-compatible.
- **Validation**:
  - `cargo test -p nils-codex-cli --test integration rate_limits`

### Task 1.3: Refresh caches, prompt output, and diagnostics safely

- **Location**:
  - `crates/codex-cli/src/rate_limits/cache.rs`
  - `crates/codex-cli/src/prompt_segment/`
  - `crates/codex-cli/docs/specs/codex-cli-diag-rate-limits-and-auth-json-contract-v1.md`
- **Description**: Make cached non-weekly data optional, remove obsolete
  five-hour values on weekly-only refresh, keep legacy cache reads compatible,
  and exclude account/email/user identifiers from `raw_usage`.
- **Dependencies**:
  - Task 1.2
- **Complexity**: 6
- **Acceptance criteria**:
  - Weekly-only prompt output is fresh and contains only weekly usage.
  - JSON never emits access tokens, email, user IDs, or account IDs.
  - The v1 contract documents the dynamic `windows` list and nullable summary
    projection.
- **Validation**:
  - `cargo test -p nils-codex-cli`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`

## Sprint 2: Delivery, Release, and Local Install

**Goal**: Merge the nils-cli fix through the tracking review gate and install a
released binary containing it.

**Demo/Validation**:

- Commands: required GitHub checks, release skill, and installed binary smoke.
- Verify: release assets and Homebrew tap succeed; installed `codex-cli`
  handles the live weekly-only API payload.

### Task 2.1: Deliver and merge the nils-cli PR

- **Location**:
  - `sympoies/nils-cli` provider PR
- **Description**: Commit through `semantic-commit`, deliver without merging,
  run testing/maintainability/API-contract specialist review, resolve all
  findings and checks, checkpoint the tracker, then squash merge.
- **Dependencies**:
  - Task 1.3
- **Complexity**: 5
- **Acceptance criteria**:
  - Required checks, review outcome, review-thread sweep, and task sweep pass.
  - The PR is squash-merged to `main`.
- **Validation**:
  - `forge-cli pr deliver --no-merge` and `forge-cli pr merge`

### Task 2.2: Release and install nils-cli

- **Location**:
  - workspace version surfaces
  - GitHub release and Homebrew tap
  - `$HOME/.local/nils-cli/bin`
- **Description**: Bump the next patch version, tag and publish the release,
  update the tap, and install the released binaries locally.
- **Dependencies**:
  - Task 2.1
- **Complexity**: 5
- **Acceptance criteria**:
  - Release workflow and tap update pass.
  - Installed `codex-cli --version` reports the new version.
  - Live `diag rate-limits --all --format json` succeeds.
- **Validation**:
  - project release skill output and installed binary smoke.

## Sprint 3: Agent Console Reader and Live Recovery

**Goal**: Consume the dynamic nils-cli contract in the live reader and prove
the prompt, table, host reader, and Agent Console display current weekly data.

**Demo/Validation**:

- Commands: `sympoies-infra` usage-reader tests, repo validation, PR delivery,
  deploy/install, loopback/API smoke, and rendered Agent Console inspection.
- Verify: every active Codex account exposes one current weekly window, no
  removed five-hour window, no stale marker, and no PII.

### Task 3.1: Update and deliver the sympoies-infra reader

- **Location**:
  - `host/agent-console/bin/agent_console_usage.py`
  - `scripts/test-agent-console-usage.py`
  - `host/agent-console/README.md`
- **Description**: Prefer `result.windows`, keep summary fallback for older
  nils-cli releases, omit absent windows instead of synthesizing them, then
  deliver and merge the reader PR.
- **Dependencies**:
  - Task 2.2
- **Complexity**: 5
- **Acceptance criteria**:
  - Weekly-only and legacy two-window fixtures both normalize correctly.
  - No raw usage PII crosses the host reader boundary.
  - Repository validation and PR checks pass.
- **Validation**:
  - `python3 scripts/test-agent-console-usage.py`
  - repo-defined `make validate`

### Task 3.2: Deploy and run live end-to-end smoke

- **Location**:
  - sympoies host `agent-console-usage.service`
  - live Agent Console edge/UI
- **Description**: Install the merged host reader, restart the service through
  the repo-owned deploy/install path, and verify live API and rendered UI state.
- **Dependencies**:
  - Task 3.1
- **Complexity**: 4
- **Acceptance criteria**:
  - Host usage endpoint reports current weekly windows for Codex.
  - Shell prompt and all-account table no longer display stale five-hour data.
  - Agent Console shows a live weekly meter instead of `—`.
  - Existing service and stack smoke checks remain green.
- **Validation**:
  - `scripts/smoke-agent-console.sh`
  - safe projected `/usage` API query
  - live rendered Agent Console inspection

## Testing Strategy

- Unit: parser, window derivation, cache compatibility, redaction, and reader
  normalization.
- Integration: codex-cli network fixtures and all/prompt outputs; agent-session
  one-window helper contract; infra reader helper invocation.
- E2E/manual: installed released binary against the live API, host reader
  service, shell prompt/table, and Agent Console rendered weekly-only state.

## Risks & gotchas

- The five-hour window can return during implementation; deterministic fixtures
  prove both shapes while the live smoke records whichever valid shape exists.
- The shared checkout contains unrelated uncommitted work; all implementation
  uses managed clean worktrees and does not overwrite it.
- The diagnostic payload previously exposed PII; validation must project only
  whitelisted fields and never retain raw live payloads.
- Release and deploy gates are external and must remain green before closeout.

## Rollback plan

- Revert the nils-cli PR and release a follow-up patch if optional-window
  parsing regresses two-window behavior.
- Revert the `sympoies-infra` reader PR and reinstall the prior reader if the
  host contract regresses; the Agent Console edge/UI require no rollback.
