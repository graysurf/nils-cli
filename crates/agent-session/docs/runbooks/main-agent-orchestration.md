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

### Durable registry compatibility

The current orchestration registry and assignment outer schemas are v3, with
`nils-agent-session` 1.25.11 as their minimum reader and writer. The first v3
mutation of released v2 state preserves the exact pre-migration bytes at
`orchestration/registry.v2.rollback.json`.

Rollback requires every orchestration writer to be stopped. Restore that exact
snapshot as `orchestration/registry.json` before starting a released v2 binary.
The snapshot is not a live mirror: restoring it intentionally discards later
v3-only mutations. Never rewrite the schema spelling or use the current v3
decoder as evidence that a released v2 binary can read the restored bytes.

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
    "cwd": "/absolute/path/to/managed-worktree",
    "coordination_mode": "enforce"
  },
  "repository": "owner/repository",
  "worktree": "/absolute/path/to/managed-worktree",
  "scopes": ["src/owned", "tests/owned"]
}
```

`assignment_id`, `task`, `base_ref`, and `durable_refs` are optional. A
mutating enforce-mode worker requires `repository`, narrow `scopes`, and an
absolute `worktree` that resolves to the same checkout root as `launch.cwd`.
Launch options also accept `title`, `session_id`, and `agent_args`. An omitted
`session_id` is derived deterministically so an exact retry does not create a
second worker.

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
init -> rehydrate/status -> worker start --await-ready -> worker bootstrap
     -> worker supervise -> Main Agent acceptance -> retire -> close
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

If the exact Main Agent controller remains live but its coordination heartbeat
owner is durably stale, use the ownership-qualified macro:

```bash
main-agent self recover \
  --idempotency-key controller-recover-001 \
  --format json
```

The macro requires the current exact Main Agent incarnation, unchanged running
runtime identity, matching broker generation, active claim, and no active or
uncertain operation. It adopts the existing broker recovery primitive and
returns `healthy_noop` when the broker is already authoritative. There is no
top-level `recover` alias. The macro never changes an account, resumes or
replaces a provider, resends a prompt, sends Enter, or clears an operation
fence.

### 2. Start an interactive worker

Use the latest run revision returned by `init`, `status`, or `rehydrate`:

```bash
main-agent worker start \
  --assignment-file assignment.json \
  --if-run-revision RUN_REVISION \
  --idempotency-key worker-start-001 \
  --format json
```

A bare single-assignment start defaults to waiting up to 5 minutes for
readiness folded into the result. Select explicit `--await-ready 0` for
launch-only behavior. Batch launch is transport-only and does not accept
`--await-ready`, preventing its bounded
lane count from multiplying readiness deadlines. Before any lane launch, batch
start persists an immutable parent manifest of sorted lane names and raw packet
digests. Exact replay resumes incomplete lanes; added, removed, renamed,
reordered, or byte-edited packets fail with `idempotency-conflict` before a
new side effect. A transient or ambiguous child failure remains incomplete and
is reconciled through that lane's durable receipt; only deterministic failures
become terminal lane results. For a nonzero wait, the
runtime persists one fixed deadline and leased finalizer; concurrent exact
replays join the same readiness attempt and return the same final receipt.
Automatic recovery reservation and reserved/sending/sent substages live in
that receipt. Reservation and the `reserved` continuation commit together
under the current finalizer fence, so an expired predecessor cannot reserve
after takeover. A successor finalizer continues the same attempt after either
reservation or Enter without reserving or sending again. The `sending` substage
holds a lease longer than the pane-input timeout; other substages retain the
short takeover lease.

A mutating worker must use its own managed worktree. Its assignment `scopes`
must be narrow enough not to overlap the Main Agent claim or another live
worker. The Main Agent owns orchestration, review, and acceptance; it must not
claim worker-owned implementation paths. An enforce-mode `claim-conflict`
therefore means the ownership plan is wrong, not that prompt transport failed.

A successful start creates an `agent-session.session.v1` record in
`mode: "interactive"`, launches a real tmux-backed provider session, and
returns the worker session ID and incarnation. With `--await-ready`, branch only
on the typed `readiness` result:

- `state: "ready"` plus `delivery.state: "confirmed"` proves a newer,
  authenticated, incarnation-matched worker checkpoint.
- `state: "readiness_failed"` plus `delivery.state: "unverified"` is a
  deterministic failure to prove delivery after any eligible recovery is
  exhausted. Do not resend the prompt or inject another Enter; retain the
  bound worker for typed session-transport diagnostics.
- `classification: "submitted_or_waiting_without_checkpoint"` with
  `proof: "authoritative-provider-turn-terminated"` returns before the outer
  timeout when an authoritative provider turn completed or failed without a
  checkpoint. No recovery Enter is sent after this proof.

For a fresh Codex or Claude launch that remains `starting`, the runtime rechecks
the exact session incarnation and live tmux status, sends one recovery Enter,
and continues waiting within the original `--await-ready` deadline. This is not
a Main Agent decision or a prompt retry. The typed `submit_key_recovery`
projection reports eligibility, whether the single recovery was attempted, and
whether it produced the authenticated checkpoint. Existing/replayed sessions,
Hermes, stopped or replaced sessions, and a second keypress are ineligible.
Prompt transport loads, pastes, and submits exactly once per completed start
stage. An exact replay never repeats those effects; if `new-session` failed
before launch, replay may replace only the same matching record carrying the
durable never-launched proof.

`acceptance.state: "pending-worker-checkpoint"` and `transport_only: true`
remain the launch-only result when `--await-ready 0` is selected. Launch
transport alone is not worker readiness, task completion, or Main Agent
acceptance.

`worker start` and `main-agent quick` perform the same readiness proof and the
same runtime-owned single-Enter recovery, and both default to
`--await-ready 5m`. Do not hand-drive a worker that stays `starting`:
read its typed `readiness` result instead. If that result is
`readiness_failed`, the recovery is already exhausted, so treat it as a
transport defect to report rather than a prompt to resend.
The quick parent receipt binds the canonical readiness duration. Reusing one
key with a different duration conflicts before a second launch, while
historical quick parent receipts and `{parent}-worker` child receipts remain
replayable during rolling upgrades.

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

### 3. Bootstrap worker identity and claim

The generated prompt tells the worker to run one deterministic command first:

```bash
main-agent bootstrap \
  --idempotency-key BOOTSTRAP_KEY_FROM_PROMPT \
  --format json
```

Bootstrap authenticates the current worker, returns only its private assignment
packet, derives and acquires the declared work-context claim, and records the
revision-fenced `working` checkpoint. The `worker start --await-ready` caller
observes that checkpoint as the `authenticated-worker-checkpoint` delivery
proof. Any eligible single-Enter recovery is already owned and bounded by that
runtime call. Do not add another Enter, resend the prompt, or manually repeat
bootstrap after its typed result.

For a mutating worker, the assignment's absolute `worktree` is required private
durable routing metadata. It is never copied into the claim's
fingerprint-only `worktrees` field. Before bootstrap mints the private
checkout-shell grant, the packet worktree, `launch.cwd`, durable assignment
worktree, and authenticated worker session `cwd` must all resolve to the same
canonical checkout root. Bootstrap then derives the HMAC fingerprint from that
authenticated `cwd`. A failure before claim acquisition records a
durable `[pre-claim:<code>]` blocker and advances the assignment to `blocked`,
so the Main Agent can diagnose and cancel that exact worker without registry
edits or group force cleanup.

For diagnostics after failure, `main-agent self show --format json` and
`main-agent rehydrate --format markdown` may be used read-only to distinguish
role/rebind, packet, claim-conflict, and provider-session problems. Fix the
typed cause and launch a new assignment/session identity when required; do not
turn pane appearance into delivery evidence.

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

### 4. Supervise and recover macro-first

Use the repeatable read-only supervisor before manual lifecycle composition:

```bash
main-agent worker supervise ASSIGNMENT_ID --format json
```

It combines the durable assignment with privacy-safe provider activity, claim,
active/uncertain operation counts, exact worker identity, and clean-worktree
progress. Branch on `classification`, never on prose:

| Classification | Deterministic action |
| --- | --- |
| `healthy_progress` | Continue bounded supervision, or review a submitted result. |
| `startup_dialog_failure` | Route trust, update, authentication, permission, or MCP decisions to their owner. Never accept automatically. |
| `evidence_unavailable` | Preserve the worker and repair or reconcile unavailable, corrupt, or identity-mismatched evidence. Do not inject input or mutate. |
| `worker_unreachable` | Preserve the assignment and investigate the missing bound worker. Do not infer safe reassignment from absence alone. |
| `pre_claim_failure` | When `reassignment_safe:true`, cancel/retire or use `worker reassign`; otherwise preserve the worker. |
| `uncertain_mutation` | Preserve the exact worker and reconcile the operation. Do not cancel, retire, or reassign. |
| `coordination_broker_stale` | Route to the exact worker's authenticated broker owner. Do not copy its capability or renew its claim as a substitute. |
| `edit_authority_stale` | Preserve the exact worker and perform a bounded supervision recheck; route only durable broker-lost evidence to broker recovery. |
| `claim_renewal_required` | Ask the exact worker to renew its own current claim and revision using its own capability file. |
| `guidance_continuity_required` | Run revision-fenced `worker guidance-reconcile`; retain message identity and unread state. |
| `orphan_guidance_quarantine_required` | Run revision-fenced `worker guidance-quarantine`; quarantine only exact-controller stale-incarnation records when no `previous_worker` exists. |
| `account_handoff_capability_gap` | Preserve the worker. No public raw restart flag exists; retry only with a daemon-launched managed worker advertising `agent-session.codex-managed-account-handoff.v1`. |
| `account_handoff_required` | Run explicitly authorized `worker account-handoff` for a managed worker and allowlisted account. |
| `stale_provider_activity` | Continue bounded supervision or queue typed guidance at a turn boundary; never send raw terminal input. |
| `submitted_or_waiting_without_checkpoint` | Do not resend the prompt or inject Enter. Reassign only when the returned safety evidence permits it. |
| `safe_reassignment` | Start only a distinct assignment/session and clean worktree. Never reuse the old prompt. |

`worker diagnose` returns the same evidence without the supervisor wrapper:

```bash
main-agent worker diagnose ASSIGNMENT_ID --format json
```

Inspect each packet, session, activity, coordination, and worktree evidence
entry by its `state`: `present`, `absent`, `unavailable`, or
`identity_mismatch`. Diagnostic reads never erase failures into an apparent
absence. A missing exact bound worker is `worker_unreachable`; corrupt,
unreadable, or identity-mismatched required evidence is
`evidence_unavailable`. Both are non-retry-safe and forbid automatic recovery
input or mutation.

Worktree progress is not porcelain status alone. Diagnosis persists a bounded,
privacy-safe material fingerprint over porcelain path state, staged and
unstaged binary/full-index diffs, and bounded untracked path/content material.
Continued same-path edits, same-size edits, untracked content changes, and
deletion-only changes reset progress age. Oversized, timed-out, or unsupported
material is unavailable. A prior-incarnation snapshot is valid history but
starts a new progress clock after authenticated worker rebind.

Broker heartbeat freshness and claim expiry are distinct evidence.
`owner-stale-dirty`-style edit refusal is broker/edit-authority staleness, not
proof that the claim's `updated_at` is old. Do not prescribe
`work-context renew` unless diagnosis returns `claim_renewal_required`.

When a resumed worker retains unread guidance on its immediately prior
incarnation, the current primary controller can reconcile it without exposing
or consuming the body:

```bash
main-agent worker guidance-reconcile ASSIGNMENT_ID \
  --if-revision ASSIGNMENT_REVISION \
  --idempotency-key guidance-reconcile-001 \
  --format json
```

Only unread, unexpired messages from that exact controller move to the current
worker incarnation. The message ID is retained, revision advances once,
forwarding provenance is bounded, and unrelated, expired, read, or
acknowledged messages do not move. Exact replay returns the same receipt.

If stale unread guidance exists but the assignment retains no
`previous_worker`, supervision returns
`orphan_guidance_quarantine_required` instead of prescribing the impossible
reconcile action. Quarantine those orphan records with:

```bash
main-agent worker guidance-quarantine ASSIGNMENT_ID \
  --if-revision ASSIGNMENT_REVISION \
  --idempotency-key guidance-quarantine-001 \
  --format json
```

The action is manager-, revision-, worker-, and incarnation-qualified. It
marks only this exact controller's unread, unexpired messages addressed to
non-current incarnations as `quarantined`; current-incarnation and unrelated
controller messages remain unchanged. Exact replay returns the same receipt,
and repeated supervision no longer prescribes guidance reconciliation.

For a Codex worker launched through the managed app-server protocol with typed
account and auto-resume controls, quota or authentication recovery uses:

```bash
main-agent worker account-handoff ASSIGNMENT_ID \
  --account ALLOWLISTED_ACCOUNT \
  --if-revision ASSIGNMENT_REVISION \
  --authorize-account-change \
  --idempotency-key account-handoff-001 \
  --format json
```

The macro preserves the exact incumbent worker and provider conversation,
queues the typed next-account transition, waits for that exact incarnation to
apply it, verifies the durable binding, and re-arms structured continuation
when a quota turn supports it. Only `starting`, `working`, or `blocked`
assignments are eligible; submit recovery and account handoff reservations are
mutually exclusive. Invalid account nicknames fail before a reservation is
written. `/logout`, raw prompt/Enter input, worker
replacement, and ambient account inference are forbidden. Raw workers expose
no `--allow-raw-restart` flag; the action fails closed without changing account
or runtime. Only after true provider and material staleness may diagnosis run a
bounded selected-account rate-limit probe, and only an exact durable selected
account is valid provenance.

An account-handoff reservation remains a mutation fence until the apply and
auto-resume receipt commits. If an apply fails, is superseded, or times out
while its durable intent is still queued or failed, cancel it without changing
the bound account:

```bash
main-agent worker account-handoff-cancel ASSIGNMENT_ID \
  --reservation-id RESERVATION_ID \
  --account ACCOUNT \
  --intent-id INTENT_ID \
  --if-revision ASSIGNMENT_REVISION \
  --authorize-account-change \
  --idempotency-key account-handoff-cancel-001 \
  --format json
```

Cancellation requires the same exact manager, assignment revision, worker
incarnation, authoritative broker, active claim, and operation quiescence. It
uses the reservation's private authenticated `reservation_id`, `account`, and
`account_intent_id` fields as the three selectors above; never infer them from
ambient provider state or public projections. A frozen released-v1 reservation
has no stored opaque reservation or provider-side intent identity; its private
authenticated view derives the stable reservation selector from the request
digest, and it may omit only `--intent-id`. It still requires the exact
reservation and account selectors.
It
captures the non-applying pending intent's account and revision, then clears
that intent only if both still match under the session-record lock; a newer
intent is preserved while cancellation succeeds and clears only the stale
assignment reservation. No cancellation retry is needed because that
reservation is gone. It
does not apply an account, re-arm auto-resume, restart or replace the worker,
send prompt/Enter input, or use `/logout`. An already-bound reserved account
must converge by retrying the original handoff; an actively applying intent
remains fenced until it becomes safely cancellable.

If the typed evidence proves the provider is still in its initial startup
state, with the exact incarnation, no dialog, no claim, no active/uncertain
operation, and no prior recovery attempt, one independently callable primitive
may send exactly one Enter:

```bash
main-agent worker submit-recovery ASSIGNMENT_ID \
  --if-revision ASSIGNMENT_REVISION \
  --timeout 5s \
  --idempotency-key recover-001 \
  --format json
```

Automatic startup recovery and this primitive share one durable attempt
reservation. The serialized input boundary immediately rechecks the exact
worker incarnation, live session, authoritative `starting` activity with no
turn or dialog, a ready/fresh/capability-backed broker for that incarnation,
claim, and operation state. While those coordination guards are retained it
also revalidates the reserving Main Agent session/incarnation, active claim,
run controller, assignment manager, and exact recovery record immediately
before Enter. A definitive pre-delivery failure is recorded
before returning. A tmux timeout or wait failure is treated as an unknown
external-effect outcome and preserves the `attempting` record plus every
manager-mutation fence. Neither case can authorize a second Enter under another
key, and the prompt is never resent.
The explicit command stores a provisional receipt with that reservation; an
exact retry joins the recorded attempt without sending input again. When the
send outcome is still unknown, that receipt remains provisional rather than
freezing the observation: the same key can later record checkpoint confirmation
or a definitive failure, still without another Enter.
If automatic readiness recovery was interrupted, the explicit command may
adopt its incarnation-bound reservation for observation only. It does not
revoke a potentially live sender: an unknown pre-send outcome remains fenced
while the reserved incarnation can still act. A newer worker checkpoint may
resolve it. If there is no checkpoint and the exact runtime has stopped, use
the non-resend reconciliation primitive:

```bash
main-agent worker reconcile-recovery ASSIGNMENT_ID \
  --if-revision ASSIGNMENT_REVISION \
  --idempotency-key reconcile-001 \
  --format json
```

Reconciliation holds the exact session-record and coordination-quiescence
guards while proving stopped tmux/runtime evidence, absent worker claim, no
active or uncertain operation, and unchanged Main Agent authority. Runtime
proof combines every persisted cgroup, process-session, and process-group
identity: a live source dominates, unavailable or unresolvable evidence stays
unknown, and stopped requires every available source to prove absence. Before
terminalizing the attempt as `reconciled`, the command atomically persists a
session-owned incarnation-bound quarantine marker without loading, pasting, or
sending input. That bounded marker rejects `agent-session resume`, maintenance
resume, work-context claim, worker bootstrap, checkpoint, and equivalent
authority restoration for the retained worker record without requiring
unrelated sessions to load the orchestration registry. If the process stops
before the registry commit, an exact retry adopts the matching marker and
finishes reconciliation. A following `worker
cancel` reacquires the exact stopped-runtime and
quiescence guards; it does not require the stopped worker broker to remain
ready or retain its capability. It still rejects a live runtime, a present
incarnation-mismatched broker, non-quiescent coordination state, or a Main
Agent claim revoked before the orchestration commit. Reconciliation fails closed
if the worker might still execute. Every manager-owned assignment
mutation, including force cleanup and relationship
changes, is refused while the attempt is `attempting` or awaiting its bounded
checkpoint result unless a newer worker checkpoint already proves input was
consumed. Success requires that newer authenticated checkpoint from the
reserved worker; later accepted, released, or relationship revisions do not
invalidate the worker-authored proof.

For a durable failed pre-claim blocker, cancellation terminalizes only that
named assignment while holding the coordination registry lock across the
revision-fenced orchestration transition:

```bash
main-agent worker cancel ASSIGNMENT_ID \
  --if-revision ASSIGNMENT_REVISION \
  --reason "pre-claim bootstrap failure" \
  --idempotency-key cancel-001 \
  --format json
```

Cancellation refuses an active claim, an active or uncertain operation, a
startup dialog, a post-bootstrap blocked worker, and every unrelated
assignment. Retire it through the ordinary proof, or use the guarded macro:

```bash
main-agent worker reassign ASSIGNMENT_ID \
  --assignment-file replacement.json \
  --if-revision ASSIGNMENT_REVISION \
  --reason "pre-claim bootstrap failure" \
  --idempotency-key reassign-001 \
  --format json
```

The replacement packet must declare a distinct assignment ID and canonical
clean worktree. An explicit session ID must be distinct; when it is omitted,
the ordinary `worker start` digest derives it. The retained failed worktree must
also be clean. Reassignment writes durable progress after cancellation,
retirement, and replacement start. An exact retry checks its top-level receipt
before mutable preconditions, skips completed stages, and resumes the first
unfinished stage with the same inner idempotency keys. A transient retirement
failure retains the exact worker; replay does not cancel twice and resumes its
pending delete. A failed never-launched replacement can then be safely removed
and relaunched under the same stable session ID, with prompt load, paste, and
Enter occurring only on the successful launch. Ordinary retirement of a
proven-absent exact session still records the normal logical-delete
tombstone/receipt/revision. Macro failure returns `failed_stage` and
`last_proven_safe_state`; retry the same macro request to resume, or continue
with the matching primitive when intentionally changing the request.

`worker retire` is also a staged durable macro. If release succeeds and delete
fails, retry the identical retire key with the original revision; its progress
receipt replays release and resumes delete. Child stage keys are digest-backed
and remain within the public idempotency-key bound even when the parent key is
128 bytes.

### 5. Review, accept, and release

Back in the Main Agent session, refresh the assignment:

```bash
main-agent status --format json
main-agent worker show ASSIGNMENT_ID --format json
```

Review the actual deliverables and validation independently. A submitted
checkpoint is a worker report, not automatic acceptance. If review requires
another worker revision, return only that exact submitted assignment to its
bound worker:

```bash
main-agent worker request-changes ASSIGNMENT_ID \
  --if-revision ASSIGNMENT_REVISION \
  --reason "Address the bounded review findings" \
  --idempotency-key worker-request-changes-001 \
  --format json
```

This manager-only transition is revision-fenced and idempotent. It preserves the
bound worker and private packet, clears stale result and blocker summaries,
records the review reason as the next action, and changes only `submitted` to
`working`. The worker must later submit a new exact result.

Only a `submitted` assignment can be accepted:

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

The normal successful path is `submitted -> accepted -> released`.
`cancelled` is reachable only through guarded `worker cancel` for a proven
failed pre-claim assignment with no claim and no active/uncertain operation.
Never synthesize cancellation by editing the private registry.

### 6. Delete the released worker

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

### 7. Close the run and converge cards

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

### Agent Console group cleanup

Agent Console can remove the Main Agent and all workers still primarily managed
by its active run through the daemon-owned group-cleanup route. Review the
preview before confirming. If any assignment is nonterminal, the first
confirmation is review-only and a second explicit force confirmation is
required; forced cleanup records those assignments as cancelled and may discard
unaccepted output. Neither mode overrides an active, completing, or
reconcile-pending operation lease. Safe mode also retains active worker claims;
force may release them only after the daemon atomically proves operation
quiescence and seals the exact worker coordination authority before deletion.

The daemon deletes workers first and the Main Agent last. If any worker fails,
the Main Agent remains available and the result identifies which workers were
deleted, absent, or failed. Resolve any reported maintenance cleanup before
retrying. The separate **Main only** action intentionally bypasses this group
cleanup and can orphan live workers; use it only when preserving those workers
for later adoption is the desired recovery path.

An incomplete cleanup response is provisional progress, not a stable terminal
receipt. Retry the identical request and idempotency key: the daemon resumes
after the last committed stage and may return success. After a
`completed: true` result is stored, later exact replays return that same
terminal value and never repeat a deletion.

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
  uncertain mutation operation. A source-session quiescence guard remains held
  through the transition, and the assignment run plus primary manager move
  atomically; source routing stops as target routing begins. The worker identity
  does not change. Handoff is rejected when the assignment has dependencies or
  another source-run assignment depends on it, because either edge would become
  cross-run.
- `adopt ASSIGNMENT_ID` moves an orphaned assignment into the authenticated
  Main Agent's active run only when the prior primary manager reference is no
  longer live. It is a recovery transition, not a way to override a live
  manager.

These commands do not send task content. Use `main-agent worker message` for a
private mailbox message, and keep authority decisions outside public summaries.
Message delivery revalidates the exact run, primary manager, and worker while
holding the same coordination-to-orchestration lock order as handoff through
the mailbox commit. It also requires the exact active sender claim from that
retained coordination snapshot; handoff makes the same claim check from its
guard. A source message therefore commits before the transfer or is rejected
after it, and claim release cannot race either mutation.

Worker checkpoints are reports only while the assignment remains worker-owned.
After `accepted`, `released`, or `cancelled`, they are rejected without changing
the durable state, result, checkpoint, worker binding, or revision.

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
3. Run `main-agent bootstrap` with a fresh idempotency key. Bootstrap releases
   the old incarnation's claim, acquires the assignment-derived claim for the
   current incarnation, carries only exact-controller unread/unexpired
   guidance, and records the revision-fenced `working` checkpoint that binds
   the assignment to the new incarnation.
4. Re-run `main-agent self show --format json`; require
   `role:"worker"` and `rebind_required:false` before editing.

The assignment projection retains `previous_worker` so the continuity repair
is auditable. If bootstrap completed its claim transition but the checkpoint
was interrupted, recover with the current claim plus a revision-fenced
   checkpoint; `worker guidance-reconcile` is the idempotent controller action for
   retained `previous_worker` guidance, while `worker guidance-quarantine`
   handles exact-controller orphan guidance when no prior identity is retained.

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
