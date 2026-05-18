# Plan: Valid Bundle

## Read First

- Primary source: `docs/plans/valid/valid-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Open questions carried into execution: none

## Scope

- In scope:
  - Validate the sibling source-doc, plan, and optional execution-state linkage.
- Out of scope:
  - Create an execution-state file before implementation starts.

## Sprint 1: Bundle validation

**PR grouping intent**: per-sprint
**Execution Profile**: serial

### Task 1.1: Validate bundle shape

- **Location**:
  - `docs/plans/valid/valid-plan.md`
- **Description**: Confirm that a not-yet-started bundle can pass with a source doc and plan only.
- **Dependencies**:
  - none
- **Complexity**: 2
- **Acceptance criteria**:
  - The plan primary source points at `docs/plans/valid/valid-discussion-source.md`.
  - The companion source doc recommends `docs/plans/valid/valid-plan.md`.
  - The companion source doc recommends `docs/plans/valid/valid-execution-state.md`.
  - `docs/plans/valid/valid-execution-state.md` is optional before execution starts.
- **Validation**:
  - plan-tooling validate --file docs/plans/valid/valid-plan.md
