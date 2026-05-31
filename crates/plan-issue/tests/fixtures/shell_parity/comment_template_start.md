## Sprint 1 Start

- Sprint: 1 (Contract fixtures)
- Tasks in sprint: 3
- Note: Main-agent starts this sprint on the plan issue and dispatches implementation to subagents.
- Execution Mode comes from current Task Decomposition for each sprint task.

| Task | Summary | Execution Mode |
| --- | --- | --- |
| S1T1 | Define split-prs CLI contract and output schema | pr-isolated |
| S1T2 | Capture deterministic parity fixtures from current task-spec behavior | pr-shared |
| S1T3 | Define deterministic normalization and error matrix | pr-shared |

## Sprint 1: Contract fixtures

### Task 1.1: Define split-prs CLI contract and output schema
- **Location**:
  - crates/plan-tooling/README.md
  - crates/plan-tooling/docs/specs/split-prs-contract-v1.md
- **Description**: Add split-prs deterministic contract details.
- **Dependencies**:
  - none
- **Complexity**: 3
- **Acceptance criteria**:
  - Contract file exists.
- **Validation**:
  - test -f crates/plan-tooling/docs/specs/split-prs-contract-v1.md

### Task 1.2: Capture deterministic parity fixtures from current task-spec behavior
- **Location**:
  - crates/plan-tooling/tests/fixtures/split_prs/per_sprint_expected.tsv
- **Description**: Capture per-sprint expected outputs.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 2
- **Acceptance criteria**:
  - Per-sprint fixture exists.
- **Validation**:
  - test -f crates/plan-tooling/tests/fixtures/split_prs/per_sprint_expected.tsv

### Task 1.3: Define deterministic normalization and error matrix
- **Location**:
  - crates/plan-tooling/docs/specs/split-prs-contract-v1.md
- **Description**: Add normalization and error matrix section.
- **Dependencies**:
  - Task 1.1
  - Task 1.2
- **Complexity**: 3
- **Acceptance criteria**:
  - Error matrix includes missing mapping case.
- **Validation**:
  - test -f crates/plan-tooling/docs/specs/split-prs-contract-v1.md
