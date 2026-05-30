# Plan: provider-neutral plan-tracking + local backend

## Overview

Make the plan-tracking flow provider-neutral and add a third `local`
provider, implemented as a real `forge-cli Provider::Local` (in-process,
file-backed) backend. Three payoffs: (1) a provider-neutral e2e driver,
(2) hermetic local testing with no remote, and (3) a foundation whose issue
half is service-grade — networking the local backend later is the plan/issue
service path. Spans `sympoies/nils-cli` (bulk + decisions) and
`graysurf/agent-runtime-kit` (driver + fixtures); `graysurf/plan-tracking-testbed`
hosts e2e fixtures only.

## Read First

- Primary source: `docs/plans/2026-05-31-plan-tracking-local-provider/plan-tracking-local-provider-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Open questions carried into execution: local store-locator shape
  (`--store-root` / `FORGE_CLI_LOCAL_STORE`); runbook drift (reconcile vs file
  a finding); conformance scenario subset.

## Scope

- In scope: driver provider-neutral seam; a durable local-provider contract
  spec; `forge-cli Provider::Local` file-backed backend (issue half real,
  PR/CI half seeded); plan-issue-cli local routing; a cross-provider
  conformance suite; a real GitLab e2e target; a gated service-feasibility
  eval.
- Out of scope: any fixable source in `graysurf/plan-tracking-testbed`
  (fixtures only); a real VCS/CI behind the local PR half (stays seeded);
  committing to build the P5 service (eval only).

## Assumptions

1. `plan-issue` / `plan-tooling` / `forge-cli` on PATH are at or above the
   skill floors (host is 0.30.2).
2. GitHub e2e is green before P0 begins and stays the golden safety net.
3. A GitLab project can be provisioned for P4 when that task starts.

## Sprint 1: Driver neutrality + frozen contract

**Goal**: a provider-neutral driver with GitHub e2e still green, and the
local-provider contract frozen as a durable spec.

**Demo/Validation**:

- Command(s): `scripts/test-plan-tracking/run.sh setup happy-path` then the
  `assert` chain; doc review of the new spec.
- Verify: no direct `gh` in driver libs; GitHub happy-path/deliver asserts
  pass; contract spec committed in nils-cli.

### Task 1.1: Driver provider-neutral seam

- **Location**:
  - `agent-runtime-kit/scripts/test-plan-tracking/lib/setup.sh`
  - `agent-runtime-kit/scripts/test-plan-tracking/lib/status.sh`
  - `agent-runtime-kit/scripts/test-plan-tracking/lib/assert.sh`
  - `agent-runtime-kit/scripts/test-plan-tracking/lib/teardown.sh`
- **Description**: extract a thin seam so the driver stops calling `gh`
  directly — issue/pr inspection routes through `forge-cli issue/pr`; the two
  raw `gh api repos/...` branch calls become git-native (`git ls-remote
  --heads`, `git push origin --delete`), dropping the GitHub-REST dependency.
- **Dependencies**:
  - none
- **Acceptance criteria**:
  - No direct `gh` / `gh api` invocation remains in the driver libs.
  - GitHub happy-path and deliver e2e remain green at every step.
- **Validation**:
  - `scripts/test-plan-tracking/run.sh` happy-path + deliver assert chains pass.

### Task 1.2: Local-provider contract schema spec

- **Location**:
  - `nils-cli/crates/plan-issue-cli/docs/specs/local-provider-contract-v1.md`
- **Description**: promote the contract-schema-draft §2–§4 into a durable spec
  that doubles as the `forge-cli Provider::Local` on-disk store format; record
  the runbook-drift decision (reconcile `provider-routing-runbook.md` §4.1 vs
  file a `plan-issue-finding`).
- **Dependencies**:
  - none
- **Acceptance criteria**:
  - Spec committed under `docs/specs/`; the JSON store schema (§3) is
    explicit and authoritative.
  - Runbook-drift decision recorded.
- **Validation**:
  - Doc review; `agent-docs audit` if the repo declares the doc.

## Sprint 2: Local backend

**Goal**: `forge-cli Provider::Local` works end to end and plan-issue-cli can
route to it.

**Demo/Validation**:

- Command(s): `cargo test -p forge-cli -p plan-issue-cli`; a manual
  `forge-cli --provider local …` smoke against a temp store.
- Verify: issue half behaves as real; PR half reads seeded records.

### Task 2.1: forge-cli `Provider::Local` backend

- **Location**:
  - `nils-cli/crates/forge-cli/src/provider.rs`
  - `nils-cli/crates/forge-cli/src/backend.rs`
  - `nils-cli/crates/forge-cli/src/local_store.rs`
- **Description**: add a `Provider::Local` variant backed in-process by a
  file store (no gh/glab subprocess). Implement the issue half
  (create/view+comments/list/edit/comment/edit-labels/close) and the pr-read
  half (view/checks/comments) per the §3 schema, with deterministic
  urls/timestamps/numbering. Locate the store via `--store-root` and/or
  `FORGE_CLI_LOCAL_STORE`.
- **Dependencies**:
  - Task 1.2
- **Acceptance criteria**:
  - `forge-cli --provider local issue {create,view,list,comment,edit,close}`
    and `pr {view,checks,comments}` operate against a temp store.
  - PR/CI fields are read from seeded `prs/<n>.json` (driver-writes-JSON v1).
- **Validation**:
  - `cargo test -p forge-cli`.

### Task 2.2: plan-issue-cli local routing

- **Location**:
  - `nils-cli/crates/plan-issue-cli/src/forge_cli_adapter.rs:127`,
    `crates/plan-issue-cli/src/provider.rs`
- **Description**: parameterize the hardcoded `--provider gitlab` base args
  into a provider-carrying forge-routed adapter so `local` rides the same rail
  as GitLab; register the provider variant and `resolve_repo` selection.
- **Dependencies**:
  - Task 2.1
- **Acceptance criteria**:
  - A full plan-issue lifecycle (`record open` → `record close`) runs against
    `--provider local`.
- **Validation**:
  - `cargo test -p plan-issue-cli`.

## Sprint 3: Conformance + GitLab

**Goal**: prove the local fake matches real providers, and exercise the flow
against real GitLab.

**Demo/Validation**:

- Command(s): the conformance suite; `run.sh` against local and GitLab.
- Verify: identical issue-half outcomes across providers; GitLab flow green.

### Task 3.1: Cross-provider conformance harness

- **Location**:
  - `nils-cli` integration tests (+ `agent-runtime-kit` driver fixtures)
- **Description**: one scenario suite run against `{local, github, gitlab}`
  asserting identical observable outcomes for the issue/timeline half; Half B
  conformance-tested for shape (seeded). Keeps local honest — a complement to,
  never a replacement for, real-provider e2e.
- **Dependencies**:
  - Task 2.2
- **Acceptance criteria**:
  - Suite green across all three providers; scenario subset documented.
- **Validation**:
  - `cargo test` conformance target + driver run.

### Task 3.2: GitLab real e2e target

- **Location**:
  - `agent-runtime-kit/scripts/test-plan-tracking/` + an external GitLab project
- **Description**: make `TESTBED_REPO`/provider switchable in the driver; add
  GitLab fixtures; run the full flow against a real GitLab project.
- **Dependencies**:
  - Task 1.1
- **Acceptance criteria**:
  - `run.sh` happy-path/deliver green against the GitLab target.
- **Validation**:
  - Driver assert chain against `provider=gitlab`.

## Sprint 4: Service feasibility (gated)

**Goal**: a written go/no-go on networking the local backend into a service.

**Demo/Validation**:

- Command(s): none — design eval.
- Verify: recommendation recorded; no build started without a separate go.

### Task 4.1: Service feasibility eval

- **Location**:
  - `nils-cli/crates/plan-issue-cli/docs/specs/local-provider-service-feasibility.md`
- **Description**: evaluate lifting `forge-cli Provider::Local` behind HTTP as
  a standalone plan/issue service. Issue half is service-grade; the PR half
  stays a stub unless a real VCS/CI is wired in.
- **Dependencies**:
  - Task 3.1
- **Acceptance criteria**:
  - A go/no-go recommendation is written; this task does not commit to build.
- **Validation**:
  - Review of the recommendation.

## Testing Strategy

- Unit: `cargo test -p forge-cli -p plan-issue-cli` for the local backend and
  routing.
- Integration: the cross-provider conformance suite (Task 3.1).
- E2E: `scripts/test-plan-tracking/run.sh` against GitHub (P0 gate), local
  (P2/P2b), and GitLab (P4).

## Risks & gotchas

- Local must synthesize urls/timestamps deterministically (no wall-clock) or
  golden/conformance tests flake; `comment_issue` urls must round-trip for
  resolve-approval.
- The local PR/CI half is a seeded stub — never let local-green stand in for
  real-provider e2e.
- nils-cli is a shared checkout; do all code work in a dedicated worktree, not
  the main worktree.

## Rollback plan

- Each task lands as its own PR; revert the offending PR. The driver seam
  (1.1) preserves the GitHub path, so a bad local backend never blocks the
  existing GitHub e2e. No data migration is involved.
