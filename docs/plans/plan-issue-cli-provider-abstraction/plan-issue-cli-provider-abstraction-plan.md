# Plan: Plan-Issue CLI Provider Abstraction

## Overview

Make `plan-issue-cli` provider-aware so the issue-backed plan-tracking and
dispatch lifecycles work on GitLab as well as GitHub. Downstream sandbox
validation against `gitlab.com` confirmed every Tier D / Tier E skill
fails today because `plan-issue` shells out to `gh issue create` regardless of
provider.

This plan is design-first: Sprint 1 produces a contract sketch and decides the
routing strategy (subprocess-to-forge-cli vs. library linkage) before any code
change.

## Read First

- Primary source:
  `docs/plans/plan-issue-cli-provider-abstraction/plan-issue-cli-provider-abstraction-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Open questions carried into execution:
  - Subprocess-to-forge-cli vs. library linkage (Q1; decide Sprint 1)
  - GitLab label catalogue fallback when `manifests/forge-labels.yaml` is absent (Q2)
  - Provider discriminator on lifecycle payload (Q3)
  - `create-dispatch-lane-pr` GitLab port (Q4; defer or absorb)
  - Auto-detect provider from cwd remote (Q5)
- Downstream sandbox findings live in `graysury/nils-cli-gitlab-sandbox`
  (GitLab) under
  `docs/plans/gitlab-skill-validation/gitlab-skill-validation-discussion-source.md`.
- Companion tracking issue for `forge-cli` fixes: sympoies/nils-cli#483.

## Scope

- In scope:
  - Provider routing layer inside `plan-issue-cli` (no new crate).
  - GitLab branch covering `record open / post / audit / close`, `link-pr`, and the dispatch lifecycle subcommands.
  - Cross-provider tests (GitHub + GitLab) using stub backends.
  - Light SKILL.md sweep for stale "gh" references in dispatch/issue skills.
- Out of scope:
  - `create-dispatch-lane-pr` GitLab port (separate skill; only touch if Sprint 3 design pulls it in).
  - Schema version bumps for `plan-issue-cli.*` envelopes.
  - Refactoring `plan-tooling` or `forge-cli`.

## Sprint 1: Design + contract sketch

**Goal**: Decide routing strategy and produce a contract sketch reviewers can
sign off on before any production code change.

**Demo/Validation**:

- Commands:
  - `cargo check -p nils-plan-issue-cli` (sanity)
- Verify: design note + contract sketch land as committed markdown; open
  questions Q1–Q5 either resolved or explicitly punted to Sprint 2+.

### Task 1.1: Audit current `plan-issue-cli` provider surface

- **Location**:
  - `crates/plan-issue-cli/src/github.rs`
  - `crates/plan-issue-cli/src/lib.rs`
  - `crates/plan-issue-cli/src/commands/`
- **Description**: Catalogue every place `plan-issue-cli` invokes `gh` or
  assumes GitHub semantics. Produce a table mapping each call site to the
  conceptual operation (open issue, post comment, edit body, set label, close
  issue, lookup repo, etc.). Land the table inside the discussion-source doc
  or a sibling design note.
- **Dependencies**:
  - none
- **Complexity**: 3
- **Acceptance criteria**:
  - Every `gh` invocation in `plan-issue-cli` is recorded with a one-line
    description of its abstract operation.
  - For each operation, the equivalent `forge-cli` subcommand is noted (or
    "no direct match" with a sketch of how to compose existing atoms).
- **Validation**:
  - Reviewer agrees the catalogue is complete (no further unlisted call
    sites).

### Task 1.2: Decide routing strategy (Q1)

- **Location**:
  - `docs/plans/plan-issue-cli-provider-abstraction/` (design note)
- **Description**: Resolve Q1 from the source doc. Compare subprocess-to-
  `forge-cli` vs. library linkage on: process count per record-open run,
  Cargo dep cost, version-skew handling, test ergonomics, and parity with the
  current `gh` shell-out pattern. Record the decision plus reasoning.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 4
- **Acceptance criteria**:
  - Decision is committed to the design note with explicit tradeoffs.
  - Implementation contract for the routing layer is sketched (trait
    signatures or function shapes).
- **Validation**:
  - Design note reviewed by an owner familiar with both crates.

### Task 1.3: Resolve or punt Q2–Q5

- **Location**:
  - `docs/plans/plan-issue-cli-provider-abstraction/` (design note)
- **Description**: For each remaining open question, either land a decision
  in the design note or explicitly defer to Sprint N+ with a placeholder.
  Do not start coding until Q1 is decided.
- **Dependencies**:
  - Task 1.2
- **Complexity**: 2
- **Acceptance criteria**:
  - Each of Q2–Q5 has a resolution status (decided / deferred / dropped) in
    the design note.
- **Validation**:
  - Design note reviewed.

## Sprint 2: GitLab `record open` end-to-end

**Goal**: `plan-issue record open --profile {tracking,dispatch}` works on a
GitLab repo with the same lifecycle contract as the GitHub path.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-plan-issue-cli` (cross-provider)
  - Live: `plan-issue --repo graysury/nils-cli-gitlab-sandbox record open --profile tracking --bundle docs/plans/p8-smoke`
- Verify: GitLab issue is opened, lifecycle comments are posted, dashboard
  body matches the GitHub-side shape.

### Task 2.1: Land the routing layer

- **Location**:
  - `crates/plan-issue-cli/src/`
- **Description**: Implement the Sprint 1 decision (subprocess or library)
  as a provider trait or function group. Keep the GitHub branch backed by the
  current `github.rs` code; add an empty GitLab branch that returns a typed
  `not_implemented` error for now.
- **Dependencies**:
  - Task 1.2
- **Complexity**: 6
- **Acceptance criteria**:
  - All existing `plan-issue-cli` tests still pass.
  - New GitLab branch returns `provider_not_implemented` (typed) when
    called.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli`

### Task 2.2: Implement GitLab branch for `record open`

- **Location**:
  - `crates/plan-issue-cli/src/`
- **Description**: Wire `record open` (both `tracking` and `dispatch`
  profiles) to use `forge-cli` (or library, per Sprint 1) for the GitLab
  branch. Cover issue create, three lifecycle comments, dashboard body edit,
  and labels.
- **Dependencies**:
  - Task 2.1
- **Complexity**: 7
- **Acceptance criteria**:
  - `record open` GitLab path produces the same envelope shape as the GitHub
    path: issue URL, comment URLs, dashboard markdown.
  - Stub-driven tests cover both profiles.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli ops::record::open`
  - Live smoke against sandbox repo.

### Task 2.3: Sandbox revalidation

- **Location**:
  - Downstream sandbox: `graysury/nils-cli-gitlab-sandbox`
- **Description**: Build the rebuilt binary, run the live `record open`
  against the existing `docs/plans/p8-smoke` bundle, audit the issue, and
  update the sandbox source doc.
- **Dependencies**:
  - Task 2.2
- **Complexity**: 2
- **Acceptance criteria**:
  - Sandbox doc Findings marks the `record open` portion of F-3 resolved.
- **Validation**:
  - Sandbox doc commit pushed.

## Sprint 3: Continue + close (`record post / audit / close`, `link-pr`)

**Goal**: Round out the lifecycle so an issue can be moved from open through
state/session/validation comments to closeout on GitLab.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-plan-issue-cli` (post/audit/close/link-pr branches)
  - Live: walk a sandbox tracking issue through state → validation → review
    → closeout.

### Task 3.1: `record post` GitLab branch

- **Location**:
  - `crates/plan-issue-cli/src/`
- **Description**: All lifecycle comment kinds (state, session, validation,
  review, closeout) must work on GitLab.
- **Dependencies**:
  - Task 2.2
- **Complexity**: 4
- **Acceptance criteria**:
  - Stub tests cover each kind.
  - Live sandbox smoke posts at least one of each kind.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli ops::record::post`

### Task 3.2: `record audit` + `record close` GitLab branch

- **Location**:
  - `crates/plan-issue-cli/src/`
- **Description**: Read-side audit must recognise GitLab `iid`-based comment
  URLs and parse the same `plan-issue-record:v2` markers. Close path must
  set GitLab issue state.
- **Dependencies**:
  - Task 3.1
- **Complexity**: 4
- **Acceptance criteria**:
  - Audit envelope on GitLab matches GitHub-side shape.
  - Close path returns ok envelope with `state: closed`.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli ops::record::{audit,close}`

### Task 3.3: `link-pr` GitLab branch

- **Location**:
  - `crates/plan-issue-cli/src/`
- **Description**: Resolve MR iid, update task ledger comment / dashboard,
  apply runtime status.
- **Dependencies**:
  - Task 3.2
- **Complexity**: 4
- **Acceptance criteria**:
  - Linking a GitLab MR to a sprint or task row works.
- **Validation**:
  - Live sandbox smoke.

## Sprint 4: Dispatch family + cleanup

**Goal**: Make `start-plan`, `start-sprint`, `ready-plan`, `accept-sprint`,
`close-plan`, `cleanup-worktrees`, `multi-sprint-guide`, and
`resolve-approval` work on GitLab. Sweep SKILL.md for stale references.

**Demo/Validation**:

- Commands:
  - Sandbox dispatch run end-to-end (one sprint, one task, single PR).
  - SKILL.md grep for `gh issue create`, GitHub-only language.

### Task 4.1: Dispatch lifecycle GitLab path

- **Location**:
  - `crates/plan-issue-cli/src/commands/`
- **Description**: Cover the dispatch subcommands (sprint lifecycle, plan
  lifecycle, worktree cleanup) on GitLab.
- **Dependencies**:
  - Task 3.3
- **Complexity**: 6
- **Acceptance criteria**:
  - Stub tests cover the new branches.
  - Sandbox dispatch sprint smokes through.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli commands::dispatch`
  - Live sandbox.

### Task 4.2: SKILL.md sweep + downstream sandbox close

- **Location**:
  - `.agents/skills/` (dispatch + issue families)
  - Downstream sandbox source doc
- **Description**: Update SKILL prereqs / Outputs that imply GitHub-only.
  Then mark F-3 fully resolved in the downstream sandbox source doc.
- **Dependencies**:
  - Task 4.1
- **Complexity**: 2
- **Acceptance criteria**:
  - No SKILL.md in dispatch/issue families says `gh issue create` or
    GitHub-only.
  - Sandbox doc Findings F-3 marked resolved with PR link.
- **Validation**:
  - `grep -r "gh issue create" .agents/skills/` returns nothing in dispatch / issue trees.
