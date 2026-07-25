# Plan: `semantic-commit default-branch` Local-Only Delivery

## Overview

Replace the exceptional `semantic-commit local-default` workflow with the
self-validating `semantic-commit default-branch` contract across nils-cli and
agent-runtime-kit. The work is delivered as signed commits on both local
`main` branches, then built, installed, and deployed locally without any
GitHub, remote-push, release, or package-publication activity. Main Agent Mode
owns orchestration and acceptance while serial managed workers own production
and test implementation in isolated worktrees.

## Read First

- Primary source:
  `docs/plans/2026-07-25-semantic-commit-default-branch/semantic-commit-default-branch-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Open questions carried into execution: none

## Scope

- In scope: the breaking CLI rename, independent default-branch proof, typed
  shared commit arguments, new preview and final receipt schemas, renamed
  `forge-cli` adoption option, Rust and Python hook admission, active policy and
  documentation, generated completions and runtime surfaces, Main Agent
  bootstrap/readiness repair and proactive abnormal-state diagnostics, local
  validation, signed local-main commits, local build/install/deploy, and
  fresh-session permission acceptance.
- Out of scope: any `local-default` alias or compatibility shim, movement of
  commit authoring into `git-cli` or `forge-cli`, normal managed-worktree/PR
  redesign, GitHub artifacts or Actions, remote push, public release, package
  publication, and edits to `.github/workflows/**`.

## Assumptions

1. The currently installed pre-change `semantic-commit local-default` remains
   available until both repositories have their signed bootstrap commits.
2. Cached local Git state is sufficient for default-branch and upstream
   validation; the new command performs no network access.
3. One implementation worker at a time is sufficient for this L2 plan; the
   runtime-kit assignment follows accepted nils-cli implementation evidence.
4. Repository-owned local validation replaces unavailable GitHub Actions
   evidence without weakening the required test surface.

## Sprint 1: Nils CLI Contracts

**Goal**: Implement the complete breaking command, receipt, consumer, hook, and
completion contract, then repair the Main Agent startup defects discovered
during this run in a separate isolated nils-cli worktree.

**Demo/Validation**:

- Commands: focused package tests, completion parity checks, and
  `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`.
- Verify: only `default-branch` is exposed, old syntax is rejected, the command
  proves default-branch identity itself, and preview/final receipt contracts
  cannot be confused.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 1.1: Establish meaningful red coverage

- **Location**:
  - `crates/semantic-commit/tests/`
  - `crates/nils-common/src/`
  - `crates/forge-cli/tests/`
  - `crates/agent-hook/tests/`
- **Description**: Add focused tests that fail against the old command for the
  canonical rename, removed flags, authoritative default-branch checks,
  remote-backed and remote-free states, separate preview/final schemas, forge
  adoption, hook classification, and completion removal.
- **Dependencies**:
  - none
- **Complexity**: 5
- **Acceptance criteria**:
  - The selected tests fail before production edits for the expected contract
    reasons and the worker records command, expected result, and observed
    failure.
  - Negative tests prove `local-default` is not accepted as an alias.
- **Validation**:
  - `cargo test -p nils-semantic-commit`
  - `cargo test -p nils-common`
  - `cargo test -p nils-forge-cli`
  - `cargo test -p nils-agent-hook`

### Task 1.2: Implement the new atomic command and receipt boundary

- **Location**:
  - `crates/semantic-commit/src/`
  - `crates/nils-common/src/`
  - `crates/forge-cli/src/`
  - `crates/agent-hook/src/`
- **Description**: Replace the old source surface with the cohesive
  `default_branch` implementation, share typed message construction, resolve
  authoritative local default-branch state, enforce aligned remote-backed or
  remote-free preconditions, emit separate preview and final schemas, rename
  the forge receipt option, and admit only the exact new hook form.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 9
- **Acceptance criteria**:
  - Exactly one forced-signed commit is created from the expected head and all
    postconditions are revalidated before an atomic outside-repository receipt.
  - No network-capable Git or provider command is invoked.
  - Old command, flag, option, and receipt surfaces fail before mutation.
  - `git-cli`, `semantic-commit`, and `forge-cli` retain their approved
    specialist boundaries.
- **Validation**:
  - `cargo test -p nils-semantic-commit`
  - `cargo test -p nils-common`
  - `cargo test -p nils-forge-cli`
  - `cargo test -p nils-agent-hook`

### Task 1.3: Align docs and generated completions

- **Location**:
  - `crates/semantic-commit/README.md`
  - `crates/forge-cli/README.md`
  - `completions/`
  - `docs/`
- **Description**: Update active documentation and regenerate bash/zsh
  completion assets so the new command and forge option are canonical and the
  old terminology remains only in negative migration tests, this plan bundle,
  and immutable history.
- **Dependencies**:
  - Task 1.2
- **Complexity**: 3
- **Acceptance criteria**:
  - Generated completions expose `default-branch` and
    `--default-branch-receipt` without the old names.
  - Active docs describe the no-network local-completion boundary consistently.
- **Validation**:
  - `zsh -f tests/zsh/completion.test.zsh`
  - `bash scripts/ci/completion-freshness-audit.sh --strict`
  - `bash scripts/ci/completion-flag-parity-audit.sh --strict`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`

### Task 1.4: Repair Main Agent startup diagnosis and recovery

- **Location**:
  - `crates/agent-session/src/main_agent.rs`
  - `crates/agent-session/tests/integration/`
  - `crates/agent-session/docs/runbooks/main-agent-orchestration.md`
  - `crates/agent-session/docs/specs/main-agent-orchestration-v1.md`
- **Description**: Reproduce and fix the assignment worktree-path bootstrap
  failure, provide a typed pre-claim blocker path, end readiness waits early
  when an authoritative provider turn terminates without a checkpoint, and
  provide a macro-first/primitive-recovery command hierarchy. Add bounded
  high-level supervision and safe reassignment while keeping independently
  callable typed diagnosis, guarded submit recovery, and pre-claim cancellation
  primitives so a Main Agent can recover when a macro itself fails.
- **Dependencies**:
  - Task 1.3
- **Complexity**: 8
- **Acceptance criteria**:
  - An assignment may retain its absolute managed-worktree path as durable
    routing metadata while bootstrap derives the HMAC checkout fingerprint from
    the authenticated worker `cwd`.
  - Bootstrap failure before claim acquisition becomes a typed durable blocker
    visible to the Main Agent rather than a silent `starting` assignment.
  - `worker start --await-ready` returns before its outer timeout when the
    authoritative worker turn has terminated without an authenticated
    checkpoint.
  - Privacy-safe diagnosis distinguishes active progress, startup/dialog
    failure, pre-claim failure, uncertain mutation, and safe reassignment.
  - `main-agent worker supervise` composes assignment, activity, claim,
    operation, and worktree-progress inspection into one repeatable bounded
    command with typed classification and next action.
  - `main-agent worker reassign` composes safe pre-claim cancellation, guarded
    retirement, and fresh assignment startup without reusing the failed
    worktree or prompt.
  - `main-agent worker diagnose`, guarded submit recovery, and
    `main-agent worker cancel` remain independently callable primitives so a
    Main Agent can recover when a high-level macro stops mid-flow.
  - A guarded per-assignment cancel transition can terminalize a failed
    pre-claim worker and then use the ordinary retire/delete proof without
    cancelling unrelated active workers or leaving a dangling assignment.
  - No recovery path resends the prompt, sends an unbounded/manual Enter,
    accepts trust/auth/update/permission prompts, or treats pane text as
    authorization or completion evidence.
- **Validation**:
  - `cargo test -p nils-agent-session main_agent`
  - `cargo test -p nils-agent-session coordination`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`

## Sprint 2: Runtime-Kit Admission

**Goal**: Update the source hook, policy, rendered surfaces, and tests in an
isolated agent-runtime-kit worktree without deploying them yet.

**Demo/Validation**:

- Commands: `bash tests/hooks/run.sh` and `bash scripts/ci/all.sh`.
- Verify: Python hook admission matches the Rust classifier, active policies
  name only the new command, and generated surfaces match their source.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 2.1: Replace runtime hook and policy references

- **Location**:
  - `docs/plans/2026-07-25-semantic-commit-default-branch/semantic-commit-default-branch-discussion-source.md`
  - `docs/plans/2026-07-25-semantic-commit-default-branch/semantic-commit-default-branch-execution-state.md`
- **Description**: In the `graysurf/agent-runtime-kit` assignment, add red hook
  tests and then update `core/hooks/shared`, active Git-delivery and intent
  policy, hook guidance, templates, and goldens to admit only
  `semantic-commit default-branch` with authoritative cached default identity.
  The listed plan files are the parent repository's durable coordination
  surface for this cross-repository lane.
- **Dependencies**:
  - Task 1.2
- **Complexity**: 7
- **Acceptance criteria**:
  - Python and Rust hook behavior agree on accepted and rejected command forms.
  - No active runtime-kit source or generated surface offers an old alias or
    old receipt option.
  - Generated outputs are produced by the repository renderer rather than
    hand-edited as source.
- **Validation**:
  - `bash tests/hooks/run.sh`
  - `bash scripts/ci/all.sh`

### Task 2.2: Make Main Agent abnormal-state handling proactive

- **Location**:
  - `docs/plans/2026-07-25-semantic-commit-default-branch/semantic-commit-default-branch-discussion-source.md`
  - `docs/plans/2026-07-25-semantic-commit-default-branch/semantic-commit-default-branch-execution-state.md`
- **Description**: In the `graysurf/agent-runtime-kit` assignment, update the
  Main Agent Mode skill and protocol so each bounded wait timeout or state
  contradiction triggers privacy-safe status, activity, claim, and worktree
  progress checks through the high-level supervisor. Require macro-first
  operation and typed primitive fallback when a macro fails. Permit one bounded
  provider-screen diagnostic only when metadata cannot classify an abnormal
  state, never as acceptance evidence. Add explicit routing for update, MCP
  initialization, trust, authentication/permission, prompt-submit, and
  pre-claim failures.
- **Dependencies**:
  - Task 1.4
  - Task 2.1
- **Complexity**: 6
- **Acceptance criteria**:
  - A healthy worker with recent progress continues without pane inspection.
  - A completed/waiting worker without a checkpoint is diagnosed immediately
    and cannot consume the whole outer workflow silently.
  - Bounded glance output may classify a blocker but cannot authorize work,
    prove completion, or override claim/checkpoint evidence.
  - Safe reassignment requires no active/uncertain operation, no claim, a clean
    retained worktree, a distinct new worktree, and a recorded reason.
  - Failed pre-claim workers are cancelled and retired individually after
    diagnosis; group force cleanup and direct session deletion are not used.
  - Update, MCP, trust, auth, permission, and submit/Enter cases have explicit
    deterministic actions and tests; unsafe decisions remain user- or
    owner-authorized.
  - The protocol uses high-level `start`, `supervise`, `reassign`, and `retire`
    operations by default, then falls back to the matching individual
    primitives from the last typed safe state instead of repeating the macro or
    stopping passively.
- **Validation**:
  - `bash tests/runtime-smoke/run.sh --mode deterministic --domain conversation`
  - `bash tests/hooks/run.sh`
  - `bash scripts/ci/all.sh`

## Sprint 3: Integrated Acceptance and Local Deployment

**Goal**: Review both diffs, run full local parity, create both bootstrap
commits before cutover, install and deploy locally, and prove permissions from a
fresh managed session.

**Demo/Validation**:

- Commands: full nils-cli local parity and coverage, runtime-kit full gate,
  repository install/sync scripts, doctors, and a fresh managed Codex
  acceptance matrix.
- Verify: both local `main` branches are signed and unpushed, installed/runtime
  surfaces agree, and hooks admit the intended exception without weakening
  ordinary default-branch protection.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 3.1: Independently review and validate both candidates

- **Location**:
  - `docs/plans/2026-07-25-semantic-commit-default-branch/semantic-commit-default-branch-plan.md`
  - `docs/plans/2026-07-25-semantic-commit-default-branch/semantic-commit-default-branch-execution-state.md`
- **Description**: Main Agent inspects each complete diff, runs testing,
  maintainability, security, performance, and API-contract review where
  applicable, returns findings to the owning worker, and reruns focused plus
  full local validation on revised heads.
- **Dependencies**:
  - Task 1.3
  - Task 1.4
  - Task 2.2
- **Complexity**: 6
- **Acceptance criteria**:
  - No unresolved blocking specialist finding remains.
  - Required focused, full, coverage, documentation, completion, render, and
    hook gates pass locally with exact evidence recorded.
- **Validation**:
  - `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh`
  - `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage`
  - `bash scripts/ci/all.sh`

### Task 3.2: Commit both local main branches before cutover

- **Location**:
  - `scripts/install-local-release-binaries.sh`
  - `docs/plans/2026-07-25-semantic-commit-default-branch/semantic-commit-default-branch-execution-state.md`
- **Description**: Integrate accepted worker commits locally, then use the
  pre-change installed `semantic-commit local-default` exactly once per
  repository to create signed local-main bootstrap commits and private
  receipts. Confirm both repositories remain unpushed before staging and
  installing the replacement binary set.
- **Dependencies**:
  - Task 3.1
- **Complexity**: 7
- **Acceptance criteria**:
  - Each repository has one signed local `main` delivery commit with its
    private outside-repository receipt.
  - Neither repository performs a fetch, push, provider mutation, or workflow
    action.
  - Recoverable backups and staged binary smoke evidence exist before live
    installation.
- **Validation**:
  - `git status --short --branch`
  - `git log -1 --show-signature --format=fuller`
  - `./scripts/install-local-release-binaries.sh`

### Task 3.3: Deploy and prove fresh-session hook permissions

- **Location**:
  - `docs/plans/2026-07-25-semantic-commit-default-branch/semantic-commit-default-branch-execution-state.md`
- **Description**: Preview and apply runtime-kit synchronization with
  `--no-pull`, run Codex and Claude doctors, then launch a new managed Codex
  session in a disposable repository/worktree test fixture. Require
  provider-observed acceptance and record a privacy-safe matrix proving the new
  dry-run is admitted, old syntax is unknown, ordinary default-branch commit is
  blocked, and normal feature-worktree commit remains allowed.
- **Dependencies**:
  - Task 3.2
- **Complexity**: 7
- **Acceptance criteria**:
  - Local runtime surfaces and policy bundle are deployed from the committed
    runtime-kit source without pull.
  - Doctors pass for installed Codex and Claude products.
  - A fresh managed session completes all four permission assertions without
    trust, authentication, setup, or permission dialogs.
  - Old binary backups remain retained until acceptance succeeds.
- **Validation**:
  - `agent-hook doctor --product codex`
  - `agent-hook doctor --product claude`
  - `agent-runtime doctor --source-root "$HOME/Project/graysurf/agent-runtime-kit" --product codex`
  - `agent-runtime doctor --source-root "$HOME/Project/graysurf/agent-runtime-kit" --product claude`
  - Fresh `agent-session` provider-hook activity and acceptance evidence

## Testing Strategy

- Unit: parser, typed argument sharing, default resolution, receipt parsing,
  hook classification, and exact command/option removal.
- Integration: disposable repositories for aligned, missing, ambiguous,
  behind, diverged, ahead, remote-free, dry-run, receipt adoption, completion,
  and no-network behavior.
- E2E/manual: signed local-main bootstrap commits, isolated binary smoke,
  local install and runtime sync, doctors, and a fresh managed-session
  permission matrix.

## Risks & gotchas

- Installing or deploying before both bootstrap commits can lock out the only
  currently admitted local-main commit route.
- A partial rename can leave contradictory binary, receipt, hook, completion,
  or policy contracts.
- Preview output must never become forge delivery evidence.
- Cross-repository validation must use exact accepted heads; mixed old/new
  installed surfaces are not acceptable.
- Full local parity is required because GitHub Actions is intentionally absent.

## Rollback plan

- Preserve private copies and digests of live binaries before installation and
  keep runtime sync transaction recovery material until fresh-session
  acceptance passes.
- If installation or deployment acceptance fails, restore prior binaries and
  use the runtime sync transaction's rollback path; do not reset or amend either
  signed local source commit automatically.
- Record the failed gate and safe state in the execution ledger before any
  resumed attempt.
