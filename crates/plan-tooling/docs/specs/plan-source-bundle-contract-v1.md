# Plan-Source Bundle Contract v1

> Status: **Active contract.** Applies to `plan-tooling validate` checks for plan-created bundles
> under `docs/plans/<slug>/`.

## Purpose

Plan-source bundles keep implementation planning artifacts linked without making execution state
mandatory before work starts. `plan-tooling validate` treats the bundle as a hard linkage contract:
the plan names its source doc, the source doc recommends the sibling plan and execution-state path,
and an existing execution state names the plan it is executing.

Durable-artifact cleanup audit is a separate advisory flow. Bundle validation answers whether linked
planning artifacts are internally consistent; cleanup audit answers whether a completed artifact is a
candidate for `delete`, `keep`, `rehome`, or `manual-review`.

## Bundle Shape

A bundle lives in one directory:

```text
docs/plans/<slug>/
```

Accepted source-doc names:

- `docs/plans/<slug>/<slug>-discussion-source.md`
- `docs/plans/<slug>/<slug>-review-source.md`

Required plan name:

- `docs/plans/<slug>/<slug>-plan.md`

Recommended execution state name:

- `docs/plans/<slug>/<slug>-execution-state.md`

Other plan files are still validated as normal Plan Format v1 files, but bundle linkage checks only
apply when the file name and parent directory both use the same `<slug>`.

## Plan Requirements

The plan `Read First` section must use one accepted sibling source doc as `Primary source`:

```md
## Read First

- Primary source: `docs/plans/<slug>/<slug>-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Open questions carried into execution: none
```

Review-backed source docs use the same source type — only the filename differs:

```md
- Primary source: `docs/plans/<slug>/<slug>-review-source.md`
- Source type: discussion-to-implementation-doc
```

`Source type: plan-only waiver` remains valid for plan-only documents, but it opts out of bundle
linkage because there is no source doc to check.

## Source Doc Requirements

The source doc must recommend the sibling plan and execution-state path:

```md
- Recommended plan: `docs/plans/<slug>/<slug>-plan.md`
- Recommended execution state: `docs/plans/<slug>/<slug>-execution-state.md`
```

The `Recommended execution state` line is required even when execution has not started. The file
itself is optional until there is execution state to persist.

## Execution-State Requirements

When the recommended execution-state file does not exist yet, the bundle is still valid. This is the
normal not-yet-started state.

When the file exists, it should point at the plan:

```md
- Source document: `docs/plans/<slug>/<slug>-plan.md`
```

Direct source-doc execution is allowed only for bounded execution with an explicit waiver:

```md
- Source document: `docs/plans/<slug>/<slug>-discussion-source.md`
- Direct source-doc execution waiver: bounded single-step execution accepted by the user
```

The waiver value must explain the reason. Empty values, `not applicable`, `n/a`, and `none` do not
waive direct source-doc execution.

Once a tracking issue exists, the execution-state header should carry the issue URL in its
`- Tracking issue:` bullet. After closeout, `Status`, `Current task`, `Next task`, merged-PR evidence,
and `Handoff` must all describe the terminal state; closeout or merge must not remain as future work.
`plan-issue record open` / `record close` write these automatically, and `plan-tooling
exec-state-sync` can repair the same fields in existing bundles offline; see the Durable
Execution-State Synchronization section of the issue-backed plan record contract. This keeps a
completed bundle coherent and discoverable by `plan-archive discover`, which infers provider refs
only from local Markdown.

The repair rejects multiline `Current task` / `Next task` values, a `Handoff` body containing a
top-level structural level-two heading, duplicate top-level `Handoff` sections, and targets without
one top-level structural `Execution State` section. Nested headings in examples remain contained.
These checks fail before the atomic file replacement so a field cannot escape into a peer section
or partially update the bundle.

## Validation Rules

`plan-tooling validate` fails a bundle when:

- the plan `Primary source` is not an accepted sibling source doc;
- the source doc exists but omits `Recommended plan`;
- the source doc exists but omits `Recommended execution state`;
- the source doc recommends a different plan or execution-state path;
- the execution-state file exists but omits `Source document`;
- the execution-state file points to a different plan; or
- the execution-state file points directly to the source doc without a valid
  `Direct source-doc execution waiver`.

`plan-tooling validate` accepts:

- a not-yet-started bundle with a source doc and plan but no execution-state file;
- an in-progress bundle whose execution state points at the sibling plan; and
- a bounded direct source-doc execution whose execution state includes a valid waiver.

## Fixture

`crates/plan-tooling/tests/fixtures/plan_bundle/valid-plan.md` is the canonical plan fixture for a
valid not-yet-started bundle. Tests that use it should place the file at
`docs/plans/valid/valid-plan.md` and pair it with a source doc containing:

```md
- Recommended plan: `docs/plans/valid/valid-plan.md`
- Recommended execution state: `docs/plans/valid/valid-execution-state.md`
```

No `valid-execution-state.md` file is required for the not-yet-started case.
