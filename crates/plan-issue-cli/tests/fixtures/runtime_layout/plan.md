# Plan: Runtime Layout Parity Fixture

## Overview

Tiny end-to-end fixture used by the canonical runtime-layout parity test.
Plan content is intentionally minimal so the parity assertions can pin the
shape of every emitted artifact without churning on plan content.

## Scope

- In scope:
  - One sprint with one task that the parity test can cover end-to-end.
- Out of scope:
  - Anything beyond canonical artifact emission.

## Assumptions

1. Test uses a temp `AGENT_HOME` and seeded prompts.

## Sprint 1: Smoke

**Goal**: Trip the canonical artifact emission path.
**Demo/Validation**:

- Command(s):
  - `cargo test -p nils-plan-issue-cli runtime_layout_parity`

**PR grouping intent**: `per-sprint`
**Execution Profile**: `serial`

### Task 1.1: Touch a sentinel

- **Location**:
  - `crates/plan-issue-cli/tests/fixtures/runtime_layout/plan.md`
- **Description**: No-op smoke task. The parity test asserts on the
  canonical artifacts emitted by `start-plan` + `start-sprint` for this
  one row.
- **Dependencies**:
  - none
- **Complexity**: 1
- **Acceptance criteria**:
  - Canonical artifacts emitted.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli runtime_layout_parity`
