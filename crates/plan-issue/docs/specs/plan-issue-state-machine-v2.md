# plan-issue State Machine v2 (Issue-Backed Lifecycle)

## Status

- This spec defines the v2 state machine that pairs with
  [issue-backed plan record contract v2](issue-backed-plan-record-contract-v2.md).
- It is the primary state machine for new agent-runtime-kit lifecycles.
- It supersedes
  [plan-issue state machine v1](plan-issue-state-machine-v1.md) for new
  issue-backed plan records. The v1 Task Decomposition state machine is
  retained only for the prior `start-plan` / `start-sprint` commands while
  they remain available; it is no longer the default agent-runtime-kit
  lifecycle truth.

## Scope

- In scope:
  - Plan record states and transitions driven by `plan-issue record`.
  - Required lifecycle evidence per state.
  - Strict closeout gate inputs.
- Out of scope:
  - Task Decomposition `Owner` / `Branch` / `Worktree` runtime metadata
    materialization. That contract continues to live in
    [plan-issue CLI contract v2](plan-issue-contract-v2.md) for the
    `start-plan` / `start-sprint` commands.
  - JSON envelope formatting (see contract spec).

## Canonical State Objects

- Plan record: one provider issue holding source, plan, state, session,
  validation, review, and closeout comments under the canonical
  `plan-issue-record:v2` marker family.
- Lifecycle role evidence: parsed structured payloads, indexed by role with
  the latest comment timestamp winning.
- Active payload contract: the state machine consumes the current lifecycle
  payload schema only. The next state payload replacement is allowed to replace
  old v2/current-only state semantics without a permanent v2 reader or mixed
  old/new reconciliation rule. Existing issues that need preservation should
  use one-off migration/repair or a new tracking issue before closeout.

## Plan Record States

- `RECORD_UNOPENED`: no provider issue exists for this plan bundle.
- `RECORD_OPEN_INITIAL`: issue exists with `source`, `plan`, and initial
  `state` payloads; no session evidence yet.
- `RECORD_OPEN_ACTIVE`: at least one `session` payload exists or `state`
  payload has progressed beyond the initial snapshot.
- `RECORD_VALIDATING`: latest `state` reports `status=complete` and at
  least one `validation` payload exists with `overall=pass`.
- `RECORD_REVIEWED`: latest `review` payload exists with decision in
  `{approve, comments-only}` and no unresolved blocker / major findings.
- `RECORD_READY_FOR_CLOSE`: every closeout gate (see below) passes.
- `RECORD_CLOSED`: provider issue is closed and `closeout` payload was
  posted with `final_status=complete`.

## Transitions

| Transition        | From                                         | To                                           | Command                                    |
| ----------------- | -------------------------------------------- | -------------------------------------------- | ------------------------------------------ |
| Open              | `RECORD_UNOPENED`                            | `RECORD_OPEN_INITIAL`                        | `plan-issue record open`                   |
| Append session    | `RECORD_OPEN_INITIAL` / `RECORD_OPEN_ACTIVE` | `RECORD_OPEN_ACTIVE`                         | `plan-issue record post --kind session`    |
| Update state      | `RECORD_OPEN_*`                              | unchanged or progression                     | `plan-issue record post --kind state`      |
| Append validation | `RECORD_OPEN_ACTIVE`                         | `RECORD_VALIDATING` (when state is complete) | `plan-issue record post --kind validation` |
| Append review     | `RECORD_VALIDATING`                          | `RECORD_REVIEWED`                            | `plan-issue record post --kind review`     |
| Repair dashboard  | any                                          | unchanged                                    | `plan-issue record repair-dashboard`       |
| Close             | `RECORD_REVIEWED` / `RECORD_READY_FOR_CLOSE` | `RECORD_CLOSED`                              | `plan-issue record close`                  |

Each transition is realized through one provider-backed command. The state
machine evaluates the latest payload per role; older payloads remain
auditable but do not satisfy gate requirements once superseded.

## Closeout Gate Invariants

`plan-issue record close` evaluates the following before closeout writes:

1. `source` and `plan` markers exist with structured payloads. `commit`
   matches a known commit in the local repo when `--bundle` is provided.
2. Latest `state` payload has `status=complete`.
3. Latest `state` payload `tasks` array has every entry in `done`,
   `deferred`, or `waived`, matching the terminal Task Ledger vocabulary.
4. Latest `validation` payload has `overall=pass`.
5. Latest `review` payload `decision` is `approve` or `comments-only`,
   with no `findings` entry whose disposition is `residual` and severity
   `blocker` or `major`. When visible completeness is requested, the latest
   review comment must also include visible review context: lenses, outcome
   evidence, or finding rows.
6. Every entry in latest `state.prs` resolves through the provider to a
   merged PR with a non-empty `merge_sha`.
7. Approval evidence (`--approval`) is present and parses as a provider
   comment URL or non-empty approval text.
8. Every requested remote-provider label addition exists in the repository
   catalog. Local stores have no catalog and keep free-form additions. For all
   providers, when one `state::*` label is added, all current state siblings
   are included in the same label edit; final read-back must contain only that
   state label.

When checks 1–8 or label-catalog preflight fail, `record close` exits without
provider writes. While holding the lifecycle lock, it rereads gate-bearing issue
and PR evidence, reevaluates the gate, and closes the issue as the first
mutation. It then converges labels, resolves closeout evidence under the
contract's latest-semantic-match and single-post-attempt rule, and writes the
final dashboard. Failures after confirmed closure do not reopen the issue and
are returned for retry or repair. Every failure emits a machine-readable code
matching the spec
[Strict Closeout Validation](issue-backed-plan-record-contract-v2.md#strict-closeout-validation).

## Dashboard Invariants

Dashboards are derived from the audit evidence:

- `## Current Dashboard` while `RECORD_CLOSED` is not reached.
- `## Final Dashboard` after `record close` succeeds.
- Durable Record links are pulled from the latest marker per role; pending
  roles show `pending`.
- Dashboard repair is idempotent: running it twice yields the same body.

## Worktree and Branch Invariants

The v2 state machine does not touch worktrees or branches. Those concerns
remain owned by `forge-cli` and dispatch-lane skills. `record post --kind
state` may carry branch / worktree information inside its structured
payload, but `plan-issue record` does not create, edit, or clean
worktrees in v2.

## Dry-Run Contract

`--dry-run` is honored across `record open`, `record post`,
`record repair-dashboard`, and `record close`:

- No provider mutations are issued.
- The rendered comment body, dashboard body, and intended provider actions
  are printed.
- The same JSON result shape as live mode is emitted, with mutation
  fields marked `dry_run=true`.

## Failure Contract

- Closeout gate violations and provider verification failures exit 1.
- Argument / usage errors exit 2.
- Successful transitions exit 0.
- All structured failures emit a stable machine-readable code in the JSON
  envelope `error.code` field.
