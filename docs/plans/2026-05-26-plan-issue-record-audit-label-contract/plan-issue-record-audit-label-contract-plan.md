# Plan: `plan-issue record audit` Label Contract (Docs-Only)

## Overview

Close out sympoies/nils-cli#535 by documenting the contract that
`plan-issue record audit` does not validate provider-issue labels, and that
label verification must be performed via a separate provider-state call.
This plan is docs-only: it amends the v2 record contract spec and the
`plan-issue-cli` CHANGELOG; it adds no flags, no code, and no fixtures.
Closing via docs is the cheaper of the two options enumerated in #535 and
matches actual repository usage — no current caller passes `--label` to
`record audit`.

## Read First

- Primary source:
  docs/plans/plan-issue-record-audit-label-contract/plan-issue-record-audit-label-contract-discussion-source.md
- Source type: discussion-to-implementation-doc
- Open questions carried into execution: none

## Scope

- In scope:
  - Tightened wording for the `plan-issue record audit` section of
    `crates/plan-issue-cli/docs/specs/issue-backed-plan-record-contract-v2.md`
    explicitly stating that label verification is out of scope.
  - Brief CHANGELOG entry in `crates/plan-issue-cli/CHANGELOG.md` recording
    the contract clarification and referencing #535.
  - Final closing comment on #535 linking to the tracking issue closeout
    and citing the spec change as the resolution.
- Out of scope:
  - Adding `--label` (or any new flag) to `record audit`.
  - Changing `lifecycle_record::audit_record`, the audit JSON envelope, or
    the visible-completeness lint.
  - Changing `record open` / `record post` / `record close` label flags.
  - Building a new label-verification subcommand.
  - Touching any production Rust source under `crates/`.

## Assumptions

1. Workspace `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
   covers the docs hygiene gates this plan exercises.
2. `plan-tooling validate` is sufficient as the plan-bundle gate; no
   special acceptance fixtures are required for a docs-only change.
3. The v2 spec is the canonical source for `record audit` behavior; no
   other doc surface (runbooks, README, CHANGELOG) currently makes a
   conflicting claim that would also need to be edited. Sprint 1 Task 1.1
   re-confirms this before editing.

## Sprint 1: Document the audit label contract

**Goal**: Land a single docs PR that pins the contract: `record audit` does
not validate labels, and callers must check labels through the provider.

**Demo/Validation**:

- Commands:
  - `plan-tooling validate --file docs/plans/plan-issue-record-audit-label-contract/*-plan.md --format text --explain`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
  - `grep -RIn "record audit" docs/ crates/plan-issue-cli/docs/ crates/plan-issue-cli/CHANGELOG.md`
- Verify: Spec language explicitly names labels as out of scope for audit;
  CHANGELOG references #535; no other doc surface still claims labels are
  audit-covered.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 1.1: Inventory existing doc claims about `record audit`

- **Location**:
  - `crates/plan-issue-cli/docs/specs/issue-backed-plan-record-contract-v2.md`
  - `crates/plan-issue-cli/README.md`
  - `crates/plan-issue-cli/CHANGELOG.md`
  - `docs/` (top-level plan and runbook surfaces touching plan-issue audit)
- **Description**: Run `grep -RIn "record audit" docs/ crates/plan-issue-cli/`
  to enumerate every doc surface that describes `record audit`. Confirm the
  v2 spec is the only surface that defines the audit contract and that no
  other doc claims `record audit` covers labels.
- **Dependencies**: none
- **Complexity**: 1
- **Acceptance criteria**:
  - Inventory captured in the execution-state Session Log.
  - If any non-spec surface implies labels are audited, list it in the
    Session Log so Task 1.2 / 1.3 can amend it too.
- **Validation**:
  - `grep -RIn "record audit" docs/ crates/plan-issue-cli/`

### Task 1.2: Amend v2 spec audit section with explicit label boundary

- **Location**:
  - `crates/plan-issue-cli/docs/specs/issue-backed-plan-record-contract-v2.md:257-268`
- **Description**: Append a short paragraph to the
  `### plan-issue record audit` section stating that label verification is
  out of scope. Direct callers to use the provider for label checks (e.g.
  `gh issue view --json labels`, `forge-cli pr view`, or an equivalent
  provider-native call), and note that label mutation remains the
  responsibility of `record open` / `record post` / `record close`.
- **Dependencies**: Task 1.1
- **Complexity**: 1
- **Acceptance criteria**:
  - Spec section explicitly names labels as out of scope for audit.
  - Spec wording does not imply a future intent to add `--label` to
    `record audit`; if a separate label-verification command becomes
    desirable, it is referenced as an open follow-up, not as a planned
    flag on audit.
- **Validation**:
  - `plan-tooling validate --file docs/plans/plan-issue-record-audit-label-contract/*-plan.md --format text`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`

### Task 1.3: Add CHANGELOG entry for the contract clarification

- **Location**:
  - `crates/plan-issue-cli/CHANGELOG.md`
- **Description**: Add an entry under the in-flight (Unreleased) section
  recording that issue #535 is closed by docs clarification rather than by
  adding a flag, with a one-line summary of the spec change and a link to
  the v2 spec section.
- **Dependencies**: Task 1.2
- **Complexity**: 1
- **Acceptance criteria**:
  - CHANGELOG entry references #535 and the v2 spec amendment.
  - Entry sits under the correct unreleased / in-flight heading per the
    crate's existing CHANGELOG conventions.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`

### Task 1.4: Open and merge the docs PR, then close #535

- **Location**:
  - GitHub PR against `main`
  - sympoies/nils-cli#535
- **Description**: Use the active PR delivery skill to open a single docs
  PR with the spec and CHANGELOG changes. After merge, post a closing
  comment on #535 linking to the merged PR and the v2 spec section, then
  close #535.
- **Dependencies**: Task 1.3
- **Complexity**: 1
- **Acceptance criteria**:
  - PR is merged to `main` with `--docs-only` CI lane green.
  - #535 is closed with a comment that cites the tracking issue closeout
    and the merged spec section.
- **Validation**:
  - `forge-cli` (or the current PR delivery skill) reports the PR as
    merged and #535 as closed.
