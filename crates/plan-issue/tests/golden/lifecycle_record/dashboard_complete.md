## Final Dashboard

This issue is the durable tracking surface for an issue-backed plan execution. The full source, plan, and execution logs remain in
append-only issue comments.

- Status: complete
- Profile: tracking
- Target scope: sympoies/nils-cli#541
- Current task: Task 2.5 done
- Next action: Close issue
- Validation: pass
- Linked PRs: sympoies/nils-cli#547
- Blockers: none
- Review approval: https://example.test/approval

## Durable Record

- Source snapshot: [source snapshot](https://example.test/source)
- Plan snapshot: [plan snapshot](https://example.test/plan)
- Execution state: [execution state](https://example.test/state)
- Latest session: [Execution Session](https://example.test/session)
- Latest validation: [Validation Evidence](https://example.test/validation)
- Latest review: [Review Evidence](https://example.test/review)
- Closeout comment: [closeout](https://example.test/closeout)

## Guardrails

- The issue body is a mutable dashboard only.
- Append-only issue comments are the durable source of truth.
- `plan-tooling` owns plan parsing, validation, batching, and PR split modeling only.
- Provider create, comment, edit, and close operations remain owned by `forge-cli` or provider atoms.
