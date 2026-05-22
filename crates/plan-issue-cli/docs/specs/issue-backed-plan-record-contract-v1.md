# Issue-Backed Plan Record Contract v1

## Scope

`plan-issue record ...` is the deterministic rendering and audit surface for
issue-backed plan records whose durable state lives in provider issue comments.
It does not create, comment on, edit, or close provider issues. Provider CRUD
remains owned by `forge-cli` or the active provider adapter.

This contract is separate from the older `start-plan` / `status-plan` /
`close-plan` Task Decomposition runtime. Those commands remain compatible for
existing dispatch lanes, while `record` commands support lightweight tracking
issues and dispatch issues that keep the provider issue body as a mutable
dashboard.

## Profiles

- `tracking`: single-agent issue-backed plan execution.
- `dispatch`: tracking plus subagent lanes, PR grouping, review state, and
  dispatch closeout gates.

Both profiles share the same visible dashboard and comment section names. The
dispatch profile adds a dispatch ledger instead of making `## Task
Decomposition` the top-level issue body truth.

## Command Boundary

- `plan-tooling`: plan parsing, validation, dependency batches, and PR split
  modeling only.
- `plan-issue record`: dashboard/comment rendering, lifecycle marker audit,
  dispatch ledger rendering, and closeout readiness evaluation.
- `forge-cli`: provider issue and PR create/comment/edit/close/read
  operations.

## Command Surface

- `record render-dashboard`: renders the mutable issue body dashboard.
- `record render-comment`: renders one append-only source, plan, state,
  session, validation, review, or closeout comment.
- `record audit`: reads issue body Markdown plus provider comments JSON and
  reports recognized lifecycle markers.
- `record closeout-gate`: evaluates closeout checks from audit evidence.
- `record build-dispatch-ledger`: uses the same plan metadata and split
  grouping rules as task-spec generation to render a dispatch ledger table.

All commands are local and deterministic. They may write rendered Markdown to
`--out`, but they never mutate a provider by themselves.

## Marker Families

`record render-comment` can emit two marker families:

- `--marker-family compat`: compatibility markers used by existing tracking
  and dispatch skills, including `plan-tracking-issue:*`,
  `execute-from-tracking-issue:*`, `tracking-issue-closeout:*`, and
  `deliver-dispatch-plan:*`.
- `--marker-family shared`: profile-aware `issue-backed-plan:*` markers for a
  future single-family migration.

`record audit` recognizes both families and also accepts current lightweight
v2 tracking markers such as `execute-plan-tracking-issue:*`.

Marker detection only accepts a marker that is the first non-empty line in a
comment. Markers quoted inside source or plan snapshots are ignored.

## Dashboard Shape

The dashboard body uses:

- `## Current Dashboard` or `## Final Dashboard`
- `## Durable Record`
- `## Guardrails`
- `## Original Tracker` when title or issue URL are available

The body is a mutable dashboard only. Durable state comes from append-only
comments.

## Closeout Gate

`record closeout-gate` reports structured checks and rendered Markdown. It can
require source snapshot, plan snapshot, complete state, session, validation,
review, closeout, explicit approval, and linked PR references. Provider merge
state still belongs to the provider/PR tooling layer.
