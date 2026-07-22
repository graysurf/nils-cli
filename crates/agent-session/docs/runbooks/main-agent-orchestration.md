# Main Agent orchestration

Use this runbook to operate one durable Main Agent run and its managed workers
through the public `main-agent` and `agent-session` commands. The normative
schemas and state-machine rules live in
[Main Agent orchestration v1](../specs/main-agent-orchestration-v1.md).

## Prerequisites and trust boundary

- Install the matching `agent-session` and `main-agent` binaries. Interactive
  workers also require `tmux` and the selected provider CLI.
- Start the Main Agent through `agent-session` or the authenticated Agent
  Console launch path. Run `main-agent` inside that managed session.
- `main-agent` authenticates the caller from `AGENT_SESSION_CAPABILITY_FILE`.
  A role name, session ID, tmux name, environment label, or orchestration record
  is not authentication. Do not copy a capability file into another session.
- Keep objective, assignment, checkpoint, and message files private. Use an
  owner-only directory and mode `0600` files; do not place secrets in command
  arguments, logs, cards, or public summaries.
- `init` can acquire the caller's coordination claim from the objective packet.
  Every later mutation requires the authenticated caller's active claim.
  Orchestration relationships do not replace repository authorization,
  provider consent, coordination claims, or operation leases.
- If Agent Console is in use, run `agent-session serve` on loopback behind its
  authenticated per-machine edge. Browser clients must use that edge for the
  bearer-protected WebSocket attach route.

Pass `--state-dir PATH` when the process cannot discover the intended
`agent-session` state directory. Use the same directory for `agent-session` and
`main-agent` throughout one workflow.

## Minimal input packets

The examples below are valid minimal packets. Replace summaries and paths with
real values. Optional arrays and objects can carry the fuller objective,
assignment, repository, scope, and durable-reference context.

### Objective packet

Save as `objective.json`:

```json
{
  "schema_version": "main-agent.objective-packet.v1",
  "tier": "L0",
  "objective_summary": "Deliver the bounded change",
  "work_context": {
    "schema_version": "agent-session.work-context-input.v1",
    "intent": "implementation",
    "tier": "L0",
    "summary": "Deliver the bounded change"
  }
}
```

`run_id`, `objective`, `done_criteria`, `constraints`, `durable_refs`, and
`next_action` are optional. Add `repositories`, `worktrees`, `provider_refs`,
`plan_refs`, and `scopes` to `work_context` when they improve coordination.

### Assignment packet

Save as `assignment.json`; `cwd` must name an existing directory when the
worker is launched:

```json
{
  "schema_version": "main-agent.assignment-input.v1",
  "task_summary": "Implement and validate the bounded worker task",
  "launch": {
    "agent": "codex",
    "cwd": "/absolute/path/to/repository"
  }
}
```

`assignment_id`, `task`, `repository`, `worktree`, `base_ref`, `scopes`, and
`durable_refs` are optional. Launch options also accept `title`, `session_id`,
`coordination_mode`, and `agent_args`. An omitted `session_id` is derived
deterministically so an exact retry does not create a second worker.

### Checkpoint packet

Save as `checkpoint.json`:

```json
{
  "schema_version": "main-agent.checkpoint-input.v1",
  "summary": "Current work is durable",
  "next_action": "Continue the bounded task"
}
```

A worker can additionally set `state` to `working`, `blocked`, or `submitted`
and can provide `result_summary` or `blocker_summary`. A Main Agent checkpoint
records only the summary and next action.

## Safe lifecycle

The ordinary lifecycle is:

```text
init -> rehydrate/status -> worker start -> worker self/checkpoint
     -> Main Agent acceptance -> release -> delete -> close
```

Read the returned `revision` after every mutation. The variable-like names in
the commands below (`RUN_REVISION`, `ASSIGNMENT_REVISION`, and
`ASSIGNMENT_ID`) mean values copied from the latest successful JSON response;
they are not shell environment requirements.

### 1. Initialize the durable run

From the authenticated Main Agent session:

```bash
main-agent init \
  --packet-file objective.json \
  --if-absent \
  --idempotency-key init-001 \
  --format json
```

`--if-absent` is mandatory. It prevents an unguarded create while allowing an
exact existing continuity identity to be returned. Preserve the objective
packet: the same packet is required for a later Main Agent rebind.

Immediately read both the bounded public state and the private recovery
capsule:

```bash
main-agent status --format json
main-agent rehydrate --format markdown
```

`status` omits private packet bodies. `rehydrate` includes the authenticated
caller's private objective or assignment packet and separates durable data from
clock-dependent observations.

### 2. Start an interactive worker

Use the latest run revision returned by `init`, `status`, or `rehydrate`:

```bash
main-agent worker start \
  --assignment-file assignment.json \
  --if-run-revision RUN_REVISION \
  --idempotency-key worker-start-001 \
  --format json
```

A successful start creates an `agent-session.session.v1` record in
`mode: "interactive"`, launches a real tmux-backed provider session, and
returns the worker session ID and incarnation. The response still reports
`acceptance.state: "pending-worker-checkpoint"` and
`transport_only: true`; launch transport is not worker readiness, task
completion, or Main Agent acceptance.

Before accepting any result, verify all of these conditions:

1. `agent-session list --format json` contains the worker with its orchestration
   assignment projection and a usable live status.
2. The loopback serve `GET /sessions` projection contains the same interactive
   session when Agent Console is used.
3. `GET /sessions/{id}/attach` upgrades to the bearer-protected WebSocket PTY
   route through the Agent Console edge and yields real terminal output. The
   route must remain usable for input and resize control as defined by the
   [Serve API v1](../specs/serve-api-v1.md).
4. The attached worker is not a blank pane, metadata-only placeholder, or
   unrelated session.

A missing, blank, stopped, or non-attachable worker fails operational
acceptance. Do not turn `pending-worker-checkpoint` into evidence of readiness
and do not edit the private registry to bypass the failure.

### 3. Establish worker identity and checkpoint

Inside the worker's own interactive session, first authenticate its assignment:

```bash
main-agent self show --format json
main-agent rehydrate --format markdown
```

The response must say `role: "worker"` and name the expected assignment. Then
declare the assignment's repository and path scope so the worker has the active
claim required by orchestration mutations:

```bash
agent-session work-context set \
  --tier L0 \
  --repository OWNER/REPOSITORY \
  --path PATH/TO/SCOPE/ \
  --summary "Implement the assigned worker task"
```

Use the assignment packet's tier, repository, and scopes. When the checkout
origin is canonical, `--repository` can be omitted. A trailing slash declares
a path prefix; without it, `--path` declares one exact path. Resolve any
enforce-mode conflict before continuing.

Then create a working checkpoint, for example:

```json
{
  "schema_version": "main-agent.checkpoint-input.v1",
  "summary": "Assignment understood; implementation is starting",
  "next_action": "Implement the scoped change",
  "state": "working"
}
```

Submit it with the latest assignment revision:

```bash
main-agent checkpoint \
  --file worker-working.json \
  --if-revision ASSIGNMENT_REVISION \
  --idempotency-key worker-working-001 \
  --format json
```

Use more checkpoints when the durable summary or next action materially
changes. A blocked worker should set `state: "blocked"` and
`blocker_summary`. When the task and its validation are ready for Main Agent
review, checkpoint `state: "submitted"` with a bounded `result_summary`:

```json
{
  "schema_version": "main-agent.checkpoint-input.v1",
  "summary": "Implementation and scoped validation are complete",
  "next_action": "Await Main Agent acceptance",
  "state": "submitted",
  "result_summary": "Focused tests and required checks passed"
}
```

After the submitted checkpoint is durable, the worker should finish or release
its mutation operation and clear its own work context when it no longer needs
the claim:

```bash
agent-session work-context clear
```

### 4. Review, accept, and release

Back in the Main Agent session, refresh the assignment:

```bash
main-agent status --format json
main-agent worker show ASSIGNMENT_ID --format json
```

Review the actual deliverables and validation independently. A submitted
checkpoint is a worker report, not automatic acceptance. Only a `submitted`
assignment can be accepted:

```bash
main-agent worker accept ASSIGNMENT_ID \
  --if-revision ASSIGNMENT_REVISION \
  --idempotency-key worker-accept-001 \
  --format json
```

After acceptance is recorded, make the assignment terminal with the revision
returned by `accept`:

```bash
main-agent worker release ASSIGNMENT_ID \
  --if-revision ASSIGNMENT_REVISION \
  --idempotency-key worker-release-001 \
  --format json
```

The normal public V1 terminal path is `submitted -> accepted -> released`.
`cancelled` is reserved for compatibility or a future transition. There is no
public V1 `main-agent worker cancel` command; do not invent one or write the
registry directly.

### 5. Delete the released worker

Deletion requires a `released` (or already reserved `cancelled`) assignment,
the latest assignment revision, a released worker claim, and no active or
uncertain worker operation:

```bash
main-agent worker delete ASSIGNMENT_ID \
  --if-revision ASSIGNMENT_REVISION \
  --idempotency-key worker-delete-001 \
  --format json
```

The command delegates to guarded `agent-session` deletion and verifies the
exact worker session identity. Retry an ambiguous result exactly as described
below. `cleanup_pending: true` means the logical deletion succeeded but the
retained physical cleanup record still needs the ordinary session-maintenance
path; it does not restore the worker card or assignment to a live state.

### 6. Close the run and converge cards

Repeat accept, release, and delete for every worker. Then read current run state
and close only after every assignment is terminal:

```bash
main-agent status --format json
main-agent close \
  --if-revision RUN_REVISION \
  --idempotency-key close-001 \
  --format json
agent-session work-context clear
```

`close` marks the durable run closed; it does not delete the Main Agent's own
interactive session. Confirm that `agent-session list` and the serve
`GET /sessions` projection no longer show deleted worker sessions and that the
Main Agent projection reports the closed run. Agent Console cards converge from
those projections; never repair a stale card by editing orchestration storage.

When the Main Agent terminal is no longer needed, stop it and have an external
operator use the normal guarded command:

```bash
agent-session delete MAIN_SESSION_ID
```

## Revision and idempotency rules

Every state mutation uses an absence or revision fence and an idempotency key.
Follow these retry rules:

- Copy the current run revision from `init`, `status`, or `rehydrate`. Copy the
  current assignment revision from `worker start`, `worker show`, `status`, or
  a mutation response.
- `--if-run-revision` fences `worker start`. `--if-revision` fences the run for
  `init` rebind, Main Agent checkpoints, and `close`, or the assignment for
  worker checkpoints and assignment mutations.
- On `orchestration-revision-conflict`, use the reported `current_revision` only
  after re-reading the resource and confirming the intended transition still
  applies. The revised request gets a new idempotency key.
- After a timeout, disconnect, store-busy response, or other ambiguous outcome,
  retry the identical command with the identical file contents, fence, caller
  incarnation, and idempotency key. A successful replay returns the original
  outcome.
- Reuse a key only for the same logical request. Reusing it with a different
  operation, packet, assignment, fence, or message fails closed with
  `idempotency-conflict`.
- Use non-empty keys of at most 128 ASCII letters, digits, `-`, `_`, `.`, or
  `:`. Keys are retry identities, not credentials.

## Collaboration, borrowing, handoff, and adoption

All relationship mutations require the current assignment revision, a new
idempotency key, the authenticated Main Agent's active claim, and an exact live
session reference formatted as `SESSION_ID@SESSION_INCARNATION` where
applicable.

- `collaborate ASSIGNMENT_ID --session REF` adds durable, non-authoritative
  routing metadata. A collaborator does not become the primary manager and
  gains no mutation or repository authority.
- `borrow ASSIGNMENT_ID --session REF --duration 30m` adds non-authoritative,
  time-bounded routing metadata. Durations use `s`, `m`, or `h` and are capped
  at eight hours. Expiry does not transfer primary ownership.
- `handoff ASSIGNMENT_ID --to REF` transfers primary assignment routing to a
  live Main Agent with an active run. The current manager must have no active or
  uncertain mutation operation. The worker identity does not change.
- `adopt ASSIGNMENT_ID` moves an orphaned assignment into the authenticated
  Main Agent's active run only when the prior primary manager reference is no
  longer live. It is a recovery transition, not a way to override a live
  manager.

These commands do not send task content. Use `main-agent worker message` for a
private mailbox message, and keep authority decisions outside public summaries.

## Resume and rebind

`agent-session resume` preserves a session's public ID and original
`created_at` while creating a new runtime incarnation. Until the authenticated
session proves continuity, `self show`, `status`, and public projections report
`rebind_required`.

For a resumed Main Agent:

1. Run `main-agent self show --format json` or `rehydrate` and read the current
   run revision.
2. Confirm the prior controller incarnation is no longer live.
3. Re-run `init` with the identical objective packet, `--if-absent`, the exact
   `--if-revision`, and a key for this rebind request.

For a resumed worker:

1. Run `main-agent self show --format json` and verify the expected assignment.
2. Confirm the old worker incarnation is no longer live.
3. Submit a revision-fenced `main-agent checkpoint`. The successful checkpoint
   atomically binds the assignment to the new incarnation.

A deleted-and-recreated session with the same display ID but a different
`created_at` is not continuity. Handoff or orphan adoption is the supported
manager recovery path when continuity cannot be proved.

## Privacy and authority

- The private registry and packet store are owner-only local state. Do not edit
  them, synchronize them through a repository, or expose them through the
  unauthenticated edge.
- `main-agent self show`, `rehydrate`, and a primary manager's `worker show` can
  reveal private packet content. `main-agent status`, `agent-session list`,
  activity projections, and serve snapshots expose only bounded relationship
  metadata.
- Public projections never grant claim, operation, repository, provider,
  operating-system, or review authority. A Main Agent must still enforce the
  surrounding workflow's authorization and acceptance rules.
- The WebSocket attach route is bearer-protected. Browser clients cannot add
  the bearer header themselves, so the Agent Console edge must inject it. Never
  put the bearer token or a session capability in a URL, packet, checkpoint,
  prompt, card, or mailbox body.
- Session IDs, incarnations, revisions, and idempotency keys are selectors and
  fences, not secrets and not authorization credentials.
