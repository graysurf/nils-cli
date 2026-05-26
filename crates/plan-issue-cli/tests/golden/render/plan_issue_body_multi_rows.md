# Multi Sprint Plan

Overview line.

## Task Decomposition

| Task | Summary | Owner | Branch | Worktree | Execution Mode | PR | Status | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1.1 | Bootstrap repo | subagent-alpha | issue/1-1 | issue-1-1 | per-sprint | TBD | planned | - |
| 1.2 | Add ci | subagent-alpha | issue/1-1 | issue-1-1 | per-sprint | TBD | planned | depends on 1.1 |
| 2.1 | Pipe/in/summary | subagent-charlie | issue/2-1 | issue-2-1 | per-sprint | TBD | planned | Multi line note |

## Consistency Rules

- `Status` must be one of: `planned`, `in-progress`, `blocked`, `done`.
- `Status` = `in-progress` or `done` requires non-`TBD` execution metadata (`Owner`, `Branch`, `Worktree`, `Execution Mode`, `PR`).
- `Owner` must be a subagent identifier (contains `subagent`) once the task is assigned; `main-agent` ownership is invalid for implementation tasks.
- `Execution Mode` should be one of: `per-sprint`, `pr-isolated`, `pr-shared` (or `TBD` before assignment).
- `Branch` and `Worktree` uniqueness is enforced only for rows using `Execution Mode = pr-isolated`.

## Risks / Uncertainties

- Sprint approvals may be recorded before final close; issue stays open until final plan acceptance.
- Close gate fails if task statuses or PR merge states in the issue body are incomplete.

## Evidence

- Plan source: `docs/plans/multi/multi-plan.md`
- Sprint approvals: issue comments (one comment per accepted sprint)
- Final approval: issue/pull comment URL passed to `close-plan`
