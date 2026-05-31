# plan-tooling Docs Index

> Ownership: Maintained by the `plan-tooling` crate maintainers. Keep this index updated when docs are added, moved, or removed.

## Specs

- [`Plan-Source Bundle Contract v1`](specs/plan-source-bundle-contract-v1.md): **active**
  contract for sibling source docs, plans, and optional execution state validated by
  `plan-tooling validate`.
- [`split-prs Contract v2`](specs/split-prs-contract-v2.md): **active** contract. Grouping-only
  `split-prs` output (`task_id`, `summary`, `pr_group`); runtime lane metadata is materialized by
  `plan-issue`.
- [`split-prs Contract v1`](specs/split-prs-contract-v1.md): **deprecated**, retained for
  historical reference. Documents the pre-v2 output shape where `split-prs` emitted runtime
  execution metadata fields (`branch`, `worktree`, `owner`, `notes`). Superseded by v2.

## Runbooks

- [`split-prs Build Task-Spec Cutover`](runbooks/split-prs-build-task-spec-cutover.md):
  command mapping and parity checks for downstream `build-task-spec` migration.

## Reports

- None yet. Add documents under `docs/reports/` and register them here.

## Links

- Back to crate README: [`../README.md`](../README.md)
