# example-plan

## Task Decomposition

| Task | Summary | Owner | Branch | Worktree | Execution Mode | PR | Status | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |

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

- Plan source: `docs/plans/example/example-plan.md`
- Sprint approvals: issue comments (one comment per accepted sprint)
- Final approval: issue/pull comment URL passed to `close-plan`
