# Plan: Same-line Metadata Fixture

## Read First

- Primary source: plan-only waiver: integration fixture
- Source type: plan-only waiver
- Open questions carried into execution: none; direct source-doc waiver behavior
  is intentionally unchanged and covered by bundle validation tests.

## Sprint 1: Formatter-collapsed metadata

- **PR grouping intent**: `group` - **Execution Profile**: `parallel-x2` (parallel width 2)

### Task 1.1: Parse same-line metadata

- **Location**:
  - crates/plan-tooling/src/parse.rs
- **Description**: Accept formatter-collapsed sprint metadata pairs.
- **Dependencies**:
  - none
- **Acceptance criteria**:
  - Same-line metadata validates.
- **Validation**:
  - cargo test -p nils-plan-tooling same_line
