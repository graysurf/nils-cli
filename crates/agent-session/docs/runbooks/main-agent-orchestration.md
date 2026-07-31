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

Fresh launch is preflighted before any durable assignment or provider side
effect. `assignment-launch-cwd-unavailable` means `launch.cwd` does not resolve
to an existing directory; create the managed worktree, then retry the same
request. For Codex, explicitly review and trust that exact canonical worktree
in Codex before launch. `provider-trust-required` routes that decision to the
user; do not accept the trust prompt on the user's behalf.
`provider-trust-unverified` means the active Codex configuration could not be
safely read or parsed; repair the provider configuration before retrying.
The preflight canonicalizes the Codex configuration directory and accepts only
a bounded regular `config.toml`; special files such as FIFOs fail without
waiting for producer input. On success, that canonical directory is persisted
with the pending start and session record, then passed to the provider process.
Exact replay therefore uses the same configuration root even when an input
symlink is retargeted or the controlling service has a different current
environment.
Trusting only a parent directory does not authorize a new managed worktree.
These errors report `retryable: true`, but their recovery remains explicitly
manual (`automatic: false`). In batch output they appear as isolated
`resumable: true` lane failures; other lanes continue. After creating the cwd
or repairing trust, replay the exact parent idempotency key and unchanged
manifest to launch only the repaired lanes.

Before creating the session record, the start path establishes a durable
operation fence bound to the controller's active claim, then revalidates the
exact current-run identity, unchanged starting assignment, pending idempotency
receipt, and any batch-lane lease. Normal claim release and replacement remain
blocked until the created worker is attached to the assignment and the fence is
terminalized. Loss of controller authority before the fence leaves the durable
pending start resumable and does not authorize a new child side effect. The
fence identity binds the request digest, idempotency key, resolved assignment,
and resolved worker session. An exact replay after controller process
interruption renews and completes that retained fence instead of stacking
another operation lease; a private acquisition token prevents a stale
invocation from terminalizing a newer owner. A private, fixed 256-way sharded
OS lock proves whether that owner is still live without unbounded lock-file
growth: concurrent exact replay joins the durable receipt without rotating the
token, while process death releases the lock so a replay can adopt the lease.
Pending replay adopts the fence before attaching an existing child, and
readiness or terminal receipt replay finishes any lease retained by a
post-commit cleanup failure.

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
- Read `prompt_observation.prompt.state` and
  `prompt_observation.composer.state` together. The former is an exact match
  against the bound provider transcript after worker creation; the latter is a
  content-free pane-digest comparison between paste and first Enter. The full
  exact-prompt observation has a fixed 500 ms allowance and pane observation has a
  one-second command bound; timeout or admission pressure reports
  `unavailable`.
- `classification: "composer_not_ready"` means the paste was not visible in
  that pane projection and the exact prompt was absent from the transcript.
  `prompt_not_present` means the pane changed but the exact prompt was absent.
  `prompt_observation_unavailable` preserves transport uncertainty.
- `checkpoint_timeout_after_prompt_submission` proves exact prompt submission
  but not bootstrap. `bootstrap_failure` additionally has
  `proof: "authoritative-provider-turn-terminated"` and returns before the
  outer timeout when the provider turn ends without an authenticated
  checkpoint. No recovery Enter is sent after that proof.
- `transport_uncertain`, `readiness_recovery_failed`, and
  `readiness_recovery_unavailable` distinguish an unknown recovery-send
  effect, a definitive bounded recovery failure with no authenticated
  checkpoint before the original deadline, and refusal before recovery input.
  A definitive recovery-send failure alone does not return a terminal
  readiness result: a late authenticated checkpoint still wins, while an
  authoritative terminated provider turn or the fixed original deadline ends
  the wait. Post-failure polling is coalesced with the finalizer renewal, and
  the final durable receipt rechecks the authenticated checkpoint under the
  registry lock before committing failure. Keep the accompanying prompt
  observation and recovery fence; none authorizes additional input.

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
progress. Existing classifications use
`main-agent.worker-supervise-result.v2` and
`main-agent.worker-recovery-action.v2`. The additive
`account_handoff_in_flight`, `readiness_stop_required`, and
`readiness_stop_in_progress` classifications use the corresponding closed v3
envelopes. `idle_claim_revocation_required` and
`idle_claim_revocation_in_progress` use the v4 envelopes with an explicit
`claim_revocation.in_flight` projection.
`claimed_runtime_stop_in_progress` uses the v5 envelopes with an explicit
`claimed_runtime_stop.in_flight` projection. The canary-authorized
`provider_stop_canary_release_in_progress`,
`provider_process_stopped_wrapper_live`, and generic
`process_runtime_stopped_wrapper_live_contradiction` classifications use the
v6 envelopes. `pre_bootstrap_attention_required` and
`provider_capacity_attention_required` use v7 envelopes with a bounded
`attention` projection. Branch on `classification`, never on prose:

| Classification | Deterministic action |
| --- | --- |
| `healthy_progress` | Continue bounded supervision, or review a submitted result. |
| `startup_dialog_failure` | Route trust, update, authentication, permission, or MCP decisions to their owner. Never accept automatically. |
| `evidence_unavailable` | Preserve the worker and repair or reconcile unavailable, corrupt, or identity-mismatched evidence. Do not inject input or mutate. |
| `worker_unreachable` | Preserve the assignment and investigate the missing bound worker. Do not infer safe reassignment from absence alone. |
| `pre_claim_failure` | When `reassignment_safe:true`, cancel/retire or use `worker reassign`; otherwise preserve the worker. |
| `post_claim_failure` | The worker held its assignment-derived claim before its runtime died. Run revision-fenced `worker reconcile-stopped`, then retire. `worker cancel` and `worker reassign` refuse this state by design. |
| `provider_stop_canary_release_in_progress` | A release marker and matching durable reservation already exist. Replay only the returned exact argv, including its original idempotency key. |
| `provider_process_stopped_wrapper_live` | The exact provider child is stopped while its bounded canary supervisor and tmux wrapper remain live. Preserve the worker, retain this supervision result, then run only the returned revision- and incarnation-fenced `worker release-provider-canary` action. |
| `process_runtime_stopped_wrapper_live_contradiction` | Generic stopped process-runtime evidence conflicts with a visible tmux wrapper, but no exact canary release authority exists. Preserve the worker and reconcile identity evidence; the returned action is deliberately non-executable. |
| `account_handoff_in_flight` | Complete the reserved account handoff or execute its typed `worker account-handoff-cancel` action before any readiness stop. |
| `readiness_stop_required` | Execute the returned Main-owned exact argv for `worker stop-runtime`. It sends no provider input and preserves state; then re-supervise and use guarded pre-claim cancellation. |
| `readiness_stop_in_progress` | Execute the returned exact replay argv. It contains the original privately retained idempotency key and cannot send provider input. |
| `claimed_runtime_stop_in_progress` | Replay only the returned v5 exact `worker stop-claimed-runtime` argv. It retains the original revision, exact incarnation, and idempotency key and cannot send provider input. |
| `idle_claim_revocation_required` | Execute the returned Main-owned exact argv for `worker revoke-claim`. It fences only a durably running, authoritative-idle exact worker and sends no provider input. |
| `idle_claim_revocation_in_progress` | Replay only the returned exact argv and original idempotency key. Its assignment reservation blocks every competing lifecycle mutation. |
| `uncertain_mutation` | Preserve the exact worker and reconcile the operation. Do not cancel, retire, or reassign. |
| `coordination_broker_stale` | Route to the exact worker's authenticated broker owner. Do not copy its capability or renew its claim as a substitute. |
| `edit_authority_stale` | Preserve the exact worker and perform a bounded supervision recheck; route only durable broker-lost evidence to broker recovery. |
| `claim_renewal_required` | Ask the exact worker to renew its own current claim and revision using its own capability file. |
| `pre_bootstrap_attention_required` | Preserve the live starting worker and continue bounded bootstrap supervision. No claim exists to renew; do not send provider input or replace the worker. |
| `provider_capacity_attention_required` | Preserve the exact worker and conversation, wait for capacity, and continue bounded supervision. Do not switch accounts, resend the prompt, or send raw terminal input. This requires exact structured `serverOverloaded` evidence, not rendered prose. |
| `guidance_continuity_required` | Run revision-fenced `worker guidance-reconcile`; retain message identity and unread state. |
| `orphan_guidance_quarantine_required` | Run revision-fenced `worker guidance-quarantine`; quarantine only exact-controller stale-incarnation records when no `previous_worker` exists. |
| `account_handoff_capability_gap` | Preserve the worker. No public raw restart flag exists; retry only with a daemon-launched managed worker advertising `agent-session.codex-managed-account-handoff.v1`. |
| `account_handoff_required` | Run explicitly authorized `worker account-handoff` for a managed worker and allowlisted account. |
| `stale_provider_activity` | Continue bounded supervision or queue typed guidance at a turn boundary; never send raw terminal input. |
| `submitted_or_waiting_without_checkpoint` | Do not resend the prompt or inject Enter. Reassign only when the returned safety evidence permits it. |
| `safe_reassignment` | Start only a distinct assignment/session and clean worktree. Never reuse the old prompt. |

`worker diagnose` returns the same evidence without the supervisor wrapper. It
uses the versioned envelopes described above for their additive
classifications and v2 otherwise:

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

If instead the final durable worker-start receipt proves
`worker-checkpoint-timeout`, the recovery is terminal rather than unknown, and
the exact worker runtime remains live without a claim or operation,
supervision returns `readiness_stop_required` with a complete executable argv.
Run that argv, or equivalently:

```bash
main-agent worker stop-runtime ASSIGNMENT_ID \
  --worker-incarnation WORKER_INCARNATION \
  --if-revision ASSIGNMENT_REVISION \
  --idempotency-key stop-runtime-001 \
  --format json
```

This is a Main-owned stop-only lifecycle action, not provider input. It
revalidates the exact controller, run, manager, assignment revision, worker
incarnation, final readiness receipt, absent worker claim, quiescent
operations, no account-handoff reservation, and live verified runtime. It
briefly holds both registries to persist the session-owned exact-worker fence
before the assignment stopping reservation, then revokes only that worker's
broker capability. It releases those global locks before terminating the exact
tmux/cgroup/process boundary under the session lifecycle lock. The assignment remains `starting` at the same revision
with its session record and managed worktree. A session-owned fence denies all
resume and worker-authority restoration until `retire` deletes that exact
stopped session. While its state is `in_progress`, it also blocks ownership,
revision, and relationship mutations, including handoff and adopt. Verified
termination advances the fence to `stopped` before the assignment reservation
is cleared. If the recorded Main is no longer live after reservation, an
authenticated active successor may use orphan `adopt`; it transfers only the
exact worker, request digest, original idempotency key, progress receipt, and
fence replay authority. It does not restore worker authority or authorize a
different stop request. If that successor also becomes unavailable, repeat
orphan `adopt`; only the ownership revision advances while the original stop
revision remains fixed. If a successor disappears after the session fence
rebind but before the registry commit, a later successor may replace it only
after proving that pending controller is unavailable. Exact replay is idempotent.
Re-run `worker supervise`; the stopped lane must now be
`pre_claim_failure`, after which revision-fenced `worker cancel` followed by
`retire`, or a distinct safe `reassign`, completes the ordinary pre-claim
lifecycle.

Never infer this action from `submit_recovery.state: failed` alone. The final
worker-start receipt and its exact `worker-checkpoint-timeout` proof are
required. If the send remains `attempting` or `sent`, do not stop the live
runtime or inject input; preserve the fence. Only after the exact runtime has
stopped may `worker reconcile-recovery` terminalize that unknown send.

If the worker runtime is still durably running and the provider has
authoritatively returned to an idle turn boundary but the assignment-derived
claim remains active, supervision returns `idle_claim_revocation_required`.
This classification and its in-progress replay use the v4 diagnose, supervise,
and recovery-action envelopes with an explicit
`claim_revocation.in_flight` projection; the existing closed v3 runtime-stop
contract is unchanged.
Execute its exact argv:

```bash
main-agent worker revoke-claim ASSIGNMENT_ID \
  --worker-incarnation WORKER_INCARNATION \
  --if-revision ASSIGNMENT_REVISION \
  --reason "authoritative provider turn ended without worker claim release" \
  --idempotency-key revoke-claim-001 \
  --format json
```

The command requires the exact running process identity, activity boundary,
claim, broker, revision, incarnation, and zero active/uncertain operations. It
holds the session and activity locks through a target-only coordination seal,
and persists a per-assignment reservation with its progress receipt before the
seal. That reservation makes every competing assignment mutation return
`worker-claim-revocation-in-flight`. If supervision reports
`idle_claim_revocation_in_progress`, replay only its returned argv.

The command writes the session authority quarantine after the reservation and
before either crash boundary or the seal. That durable fence rejects resume,
bootstrap, broker provisioning, claim acquisition, checkpointing, and operation
admission while the retained process still exists. The seal then removes the
exact worker capability, stops its broker, and releases its claim. Exact replay
after a crash may use the persisted admission proof across later runtime or
activity drift only when the exact authority quarantine is already durable. A
replay from the reservation-only gap must first re-prove the same idle activity
revision and running runtime identity before writing that fence.
`worker-claim-revocation-evidence-drift` means those identities changed; it
leaves the quarantine and registries untouched and is not an instruction to
retry against the new evidence. The same recovery guard checks the exact
assignment-derived claim before persisting the fence. It accepts an unchanged
active claim or an already released claim with the same authoritative broker
and quiescent worker, but a different active claim returns
`worker-claim-mismatch` without fencing the session. A sealed replay then
converges without a second revocation. It produces one revision advance and one
terminal receipt. A `working` assignment becomes
`cancelled`; an `accepted` assignment remains accepted and can be retired
normally. Session metadata and the managed worktree are preserved, and
`input_sent:false` proves that no prompt, paste, Enter, or provider-exit command
was used. On a fresh request, `worker-runtime-stopped` routes to
`worker reconcile-stopped`; `coordination-runtime-unverified` fails closed.

If the recorded Main becomes unavailable while the reservation is in progress,
an authenticated active successor may run revision-fenced orphan `adopt`.
Adoption transfers only the exact worker, original request and idempotency key,
persisted admission proof, progress receipt, and authority-quarantine identity.
It advances the ownership revision but leaves the original revoke revision
fixed; replay the supervision-provided argv with that original revision.
A crash before the adoption registry save leaves the prior owner intact and
the same adoption remains retryable. If the prior controller died between the
reservation save and quarantine write, the successor reconstructs the exact
fence only after re-proving the unchanged idle activity revision, running
runtime identity, assignment-derived claim, authoritative broker, and operation
quiescence under the worker lifecycle/activity/coordination guards. Drift
fails closed. Concurrent identical revoke calls re-read the receipt after the
lifecycle lock; a waiter that initially saw no receipt adopts the winner's
progress, while a waiter behind finalization returns the one terminal receipt.

If instead the worker already reached `working` — bootstrap acquired its
assignment-derived claim — and its exact runtime is still live at an
authoritative idle boundary, the typed post-claim stop-only action can create
the stopped-with-live-claim state without provider input:

```bash
main-agent worker stop-claimed-runtime ASSIGNMENT_ID \
  --worker-incarnation WORKER_INCARNATION \
  --if-revision ASSIGNMENT_REVISION \
  --idempotency-key stop-claimed-runtime-001 \
  --format json
```

Use it only when current supervision and broker reads prove the exact worker is
authoritative-idle, running, operation-quiescent, and still carries its exact
assignment-derived active claim. It first persists an assignment-, revision-,
incarnation-, request-, and idempotency-bound session identity, then the
existing runtime fence and progress receipt before stopping the verified
runtime. Identity-first interruption still projects the exact replay without a
global receipt scan. The fence makes the launch wrapper's clean broker stop
fail closed, so the action can prove `worker_claim_active_after:true`. It
persists a narrow exact-claim mutation fence, releases the global coordination
lock for the bounded runtime stop, then reacquires it for the post-stop proof.
Unrelated broker heartbeats and claim mutations remain available. It preserves
the `working` assignment at the same
revision, the session, and the managed
worktree, and returns `input_sent:false`. If interrupted, run only the exact v5
replay argv projected by `claimed_runtime_stop_in_progress`.
If a post-stop interruption outlives the claim TTL, exact replay returns a
truthful degraded result with `worker_claim_active_after:false` and restores
the `reconcile-stopped` route. Do not count that recovery as B2 field closure;
only `true` plus reconcile stage-1 `observed_at_stage1:true` qualifies.
If the Main controller dies after the typed stop completes, an active
successor may `adopt` the orphan assignment. Adoption rebinds both persistent
stop identities and records their original controller/revision lineage before
the assignment revision advances; an interrupted adoption is replayed with
the same successor command. If that pending successor also dies before the
registry save, another active Main may recover only the same origin-consistent
adopted revision after proving the intermediate controller non-live. The
successor then runs `reconcile-stopped` at the
adopted revision and may delete the terminal exact worker normally.

For the B2 field boundary, an assignment packet may opt in to the narrowly
scoped Codex-only canary:

```json
{
  "provider_stop_canary": {
    "schema_version": "main-agent.provider-process-stop-canary.v1"
  }
}
```

This additive private packet field is the only public authority switch and is
admitted only on Linux, where PID start-time and parent-death semantics provide
the required exact-process evidence.
Ordinary `agent-session start`, daemon launches, Claude workers, unknown
schemas, and already-created non-canary sessions cannot acquire the
capability. On Linux, every successful tmux launch persists the pane start
time beside the pane PID before returning. Canary admission consumes that
immutable launch-time tuple directly; a single matching process-session member
remains a read-only compatibility fallback for older session records, while
missing, conflicting, or stale evidence fails closed. The compiled supervisor
reconstructs only the recorded Codex
binary and arguments for its exact session incarnation through a same-release
compiled guardian. Before releasing the held tmux runtime, `worker start`
commits the same starting assignment from revision 1 with no worker to revision
2 bound to the newly persisted exact session and incarnation. The supervisor
independently waits at most five seconds, without launching the provider or
reading terminal input, to observe that exact binding. Any other assignment
transition, identity, or timeout fails before provider launch. The pending
worker-start receipt records `assignment-created`,
`worker-bound-runtime-held` (prior-version compatibility),
`worker-bound-canary-startup-pending`, `canary-startup-failed`,
`runtime-released-prompt-attempting`, `prompt-delivered`, and
`prompt-outcome-unknown` phases. The rev2 prebind first
installs an exact-incarnation execution quarantine, so generic resume cannot
replace the canary process boundary while worker-start is pending. A failure
proven to occur before prompt delivery rolls the exact rev2 binding back to
rev1 while retaining that quarantine through exact typed session cleanup,
revokes and forgets only that failed incarnation's empty broker, and permits
replay of the same idempotency key. Removing the quarantined session directory
is the rollback authority-release event. If the rollback commit itself is not
proven, worker start preserves the rev2 binding, held exact session, receipt,
and quarantine; it does not delete the session, and only the identical
idempotency replay may resume the transaction. An interruption while the runtime is
still held may release and continue
only the exact recorded incarnation. A prior-version
`worker-bound-runtime-held` receipt with no release gate first migrates
durably to the startup-pending phase; if its release gate already exists, it
remains fail-closed as prompt-outcome-unknown. The distinct
`worker-bound-canary-startup-pending` phase may cross the release gate only to
complete authenticated startup admission; success advances to prompt-attempt
exactly once. A `canary-startup-failed` phase retains the rev2 binding, exact
session/incarnation, receipt, and quarantine. Exact replay returns the same
typed error; terminalize it through revision-fenced pre-claim `worker cancel`
and exact `worker retire` (or the same closeout stages). Once any other release gate or prompt-attempt
phase was observed without a durable prompt outcome, replay never sends the
prompt again and returns typed outcome-unknown; it cannot finalize worker-start
success. Only after a final worker-start receipt may the exact active
quarantine gain a separate immutable release proof matching the assignment,
revision, session/incarnation, reason, and runtime-identity digest. The proof
is persisted before the active marker is removed, so interruption can replay
without an authority gap. A missing or mismatched marker fails closed; the
release proof is retained in an identity-keyed private proof collection until
exact session cleanup. Later typed lifecycle operations may create and release
a new active quarantine into a distinct proof without overwriting prior proofs;
an exact identity already present in the proof collection cannot be persisted
active again. Rollback
retains the active quarantine until exact session cleanup. An authenticated bootstrap from that
exact session/incarnation may recognize only its matching
`runtime-released-prompt-attempting`, `prompt-delivered`, or
`prompt-outcome-unknown` startup transaction and wait up to five seconds for
the final receipt, absent startup quarantine, and matching typed release proof.
It does not acquire a claim during the
wait; timeout is typed and retryable after exact worker-start replay. Every
other bootstrap and every generic resume remain fail-closed. A stopped rev2 runtime cannot be
finalized as a successful worker
start; a stopped runtime-held phase is safe to roll back because provider and
prompt launch were never authorized. A stopped
`worker-bound-canary-startup-pending` phase is likewise rolled back while its
release gate is absent; after release it becomes the retained typed startup
failure and requires the cancellation/retirement recovery above.
For current records, an absent recorded cgroup is affirmative stopped evidence
only when the observer still matches the persisted boot, cgroup namespace, and
mount namespace plus the canonical cgroup-v2 mount identity, with no
subordinate mount that could mask the exact scope path. A pre-provenance
`canary-startup-failed` record has no such mount provenance, so `worker cancel`
accepts absence only when the fixed root-owned `/usr/bin/systemctl` reaches the
exact owning user manager through its verified private runtime socket and
reports the exact UUID-derived scope as `not-found`, `inactive`, `dead`, with
an empty control group. The same proof is re-established under the exact
session lock before a cancelled worker is physically deleted; that deletion
skips runtime termination and gains no signal or tmux-kill authority. The
query has a two-second bound, a strict output cap, no shell or inherited
environment, and no termination authority. Missing, untrusted, malformed,
loaded, or unreachable evidence leaves the session and assignment retained.
After releasing the startup-pending runtime,
worker-start waits up to fifteen seconds for the exact guardian-authenticated
startup channel to return a live provider PID/start-time identity inside the
incarnation-derived child cgroup before it records
`runtime-released-prompt-attempting`. Owner-only marker files are diagnostic
signals and never satisfy this admission. An unverified guardian, invalid child
identity, or timeout returns
`provider-stop-canary-startup-failed`; no prompt bytes or submit key have been
sent. The exact failure is durably retained as `canary-startup-failed`, so
exact worker-start replay returns the same typed result without relaunch or
prompt transport. Diagnose its bounded `stage` and `failure_code`, then use the
typed pre-claim recovery. Never retry by inspecting the pane, resending the
prompt, or injecting Enter. Before guardian launch, the supervisor
becomes a child
subreaper and creates and pins the incarnation-derived child cgroup v2
directory, membership, freeze, and event handles. The guardian reopens only
that device/inode-fenced boundary, becomes the provider tree's child
subreaper, installs its parent-death handler, moves the provider into the
cgroup before exec, and enters a process session distinct from the tmux
supervisor. Before provider exec, the provider
gets its own user, mount, and cgroup namespaces with that exact child cgroup
as the cgroup namespace root and a read-only `/sys/fs/cgroup` view. It
therefore cannot migrate itself or a descendant to the writable delegated
parent or a sibling, while the outer guardian retains the exact observation
and freeze handles. The provider's host UID is mapped only to namespace UID
zero for this private mount setup. Before exec, the guardian locks off root and
setuid capability reconstruction, clears every capability set and bounding
entry, sets no-new-privileges, and installs a native-architecture seccomp
filter. The filter denies user-namespace creation through `unshare` or classic
`clone`, denies `setns`, and makes uninspectable `clone3` return `ENOSYS` so
ordinary thread creation can fall back to inspected classic `clone`. Every
non-standard inherited descriptor is close-on-exec; local-domain `socket` and
`socketpair` creation and `io_uring_setup` are denied, and the standard user
D-Bus, SSH-agent, and runtime-directory discovery variables are removed. This
prevents the ordinary same-UID user-systemd, D-Bus, agent, and
container-daemon process-broker channels, including `IORING_OP_SOCKET`
bypasses, while preserving ordinary Internet sockets. Startup fails closed if
any part of that privilege seal cannot be installed or verified.

This canary is an exact-process termination contract, not a general same-UID
host sandbox. A separately pre-existing same-UID process broker reachable over
an allowed Internet-domain socket, or a deferred host scheduler driven only by
filesystem writes, remains outside its authority and containment boundary.
Canary assignments MUST use the bounded field-proof prompt and MUST NOT ask
the provider to invoke an external process broker or persistence mechanism.
The stop proof covers only the guardian-authenticated provider identity and
its pinned incarnation cgroup members.
Provider cwd, executable, and standard streams are rejected if they are
cgroupfs-backed, and every non-standard inherited descriptor is sealed
close-on-exec after namespace setup. This prevents an inherited path handle
from bypassing the read-only rooted view. The guardian also requires exactly
one inherited cgroup2 mount at `/sys/fs/cgroup`; any alternate bind alias
fails admission before provider exec.
The guardian continuously pidfd-pins only processes whose ancestry terminates
at its exact subreaper identity, including orphans after an immediate provider
leader exit. An authorized stop freezes membership, rejects any
unrelated or reused PID present in the stable snapshot, and signals only that
pinned set; `cgroup.kill` is never used. A same-UID host process concurrently
migrated into the cgroup is never a termination target, even if it arrives
after the final snapshot. Such an out-of-provider host process can cause only
a bounded cleanup failure/denial of service and is outside the canary's
termination-authority trust boundary. The provider itself cannot perform that
migration from its rooted read-only namespace.
An abrupt supervisor exit is handled by the guardian's pinned boundary. A
hard guardian crash reparents the provider tree to the supervisor subreaper,
which performs the same membership validation through its already-pinned
handles and never reopens a path for termination. A host without the
required namespaces or a privately delegated writable cgroup v2 parent
refuses the canary before provider exec. It accepts no
executable, PID, signal, timeout, or tmux target. Its typed stop request is
admitted only for the current run/controller, exact assignment revision,
exact worker incarnation, active assignment-derived claim, authoritative-idle
provider turn, authoritative broker, and zero active or uncertain operations.
Admission first commits a mutually exclusive orchestration-owned, owner-only
sidecar reservation bound to the run, controller, assignment revision,
activity revision, runtime identity, exact worker/controller claim tuples,
worker incarnation, request digest, idempotency key, and the supervisor's
observed child PID/start-time identity. A session-owned execution fence is
durable before the stop marker, blocks CLI and HTTP resume, and remains until
stopped reconciliation.
The sidecar deliberately leaves the released v3 registry and assignment wire
schema unchanged. Before provider exec, the guardian requires the live
controller pane to match the PID/start-time tuple already persisted for that
controller incarnation. Fresh controller and canary-wrapper launches carry
that tuple independently of the dynamic process-session member snapshot. The
guardian pins the controller process with a pidfd for its lifetime and
opens a private Unix control socket. A marker plus sidecar is never sufficient authority: stop and
release additionally require Linux `SO_PEERCRED` proof that the requesting
CLI process descends from that captured controller and is outside the
provider child cgroup. Every provider descendant remains in that rooted
cgroup and therefore cannot self-authorize even though it shares the host
UID and can read owner-only state. The guardian also requires the matching
durable reservation before acting. It freezes the pinned cgroup, verifies
every stable member as the exact provider leader or a previously pidfd-pinned
descendant, signals only those identities, and removes the same pinned empty
cgroup. Guardian request parsing and acknowledgement share one
250-millisecond absolute deadline, with supervisor-loss checks between reads;
controller connect, request, and acknowledgement also share one absolute
deadline. A trickled frame or full local socket backlog therefore fails
closed within the typed bound instead of delaying crash cleanup. If the
guardian dies abnormally, the supervisor validates and signals
only provider members reparented to its exact subreaper through the boundary
it pinned before launch. It never reopens a path for termination. Main
independently proves that the same PID/start-time identity is absent or stopped
before writing the supervision proof. Private markers are bounded regular
files opened without symlink following and must be owned by the current user
with no group/other access. They are coordination signals, not bearer
authority. The wrapper remains live for at most 120 seconds.

```bash
main-agent worker stop-provider-canary ASSIGNMENT_ID \
  --worker-incarnation WORKER_INCARNATION \
  --if-revision ASSIGNMENT_REVISION \
  --idempotency-key stop-provider-canary-001 \
  --format json
```

During the bounded hold, supervision must return
`provider_process_stopped_wrapper_live`, never `healthy_progress` or
`evidence_unavailable`. Every competing assignment mutation is fenced.
Release only the same exact canary:

```bash
main-agent worker release-provider-canary ASSIGNMENT_ID \
  --worker-incarnation WORKER_INCARNATION \
  --if-revision ASSIGNMENT_REVISION \
  --idempotency-key release-provider-canary-001 \
  --format json
```

Release sends no provider input and lets the wrapper, broker heartbeat, and
tmux session exit normally. Exact request identity makes interrupted stop and
release calls replayable. `release_requested` is durable before the
controller-authenticated guardian request, so a crash after the marker but
before that request or the terminal receipt resumes the same reservation.
Supervision exposes that exact replay as
`provider_stop_canary_release_in_progress`; a competing idempotency identity
fails closed. `worker_claim_preserved` reports the exact admitted claim tuple
as observed at terminal release; it is `false`, without blocking cleanup, if
that claim expired or disappeared during the bounded hold. If the
controller does not release within 120 seconds, the wrapper exits
automatically. That timeout invalidates the field observation, but does not
wedge the assignment: once exact runtime and tmux evidence are stopped,
`reconcile-stopped` may consume the expired reservation and session fence
after terminalizing the assignment. If the original controller is lost, a
successor may adopt only after exact runtime and tmux evidence are stopped;
the controller-bound sidecar is consumed, while the execution fence remains
until successor-owned stopped reconciliation.

Once the exact runtime is stopped, supervision returns
`post_claim_failure`. Do not reach for
`worker cancel`, `worker reassign`, or
Agent Console force group cleanup: the first two refuse a post-claim assignment
and the third deletes the Main Agent session that would have to run it. Use the
guarded post-claim transition:

```bash
main-agent worker reconcile-stopped ASSIGNMENT_ID \
  --if-revision ASSIGNMENT_REVISION \
  --reason "worker runtime stopped after claim acquisition" \
  --idempotency-key reconcile-stopped-001 \
  --format json
```

It holds the exact session-record lock and the worker coordination-quiescence
guard while proving the runtime stopped from both the tmux session state and the
persisted cgroup/process-session/process-group identity. A live runtime returns
`worker-runtime-still-live` and unknown runtime evidence returns
`coordination-runtime-unverified`; neither is retried automatically. Unlike
`worker cancel` an active worker claim is expected, but any active, completing,
or reconcile-pending operation lease returns `worker-not-quiescent`, a changed
incarnation returns `worker-incarnation-changed`, and a non-`working` assignment
returns `assignment-state-conflict`. The first stage persists the exact
controlling Main Agent claim ID, revision, and expiry. Authentication, replay,
and the target-only seal use an observational coordination lock: they do not
normalize notifications, renew claims, probe or renew operations, or write
maintenance state. An unchanged active, unexpired tuple authorizes normal
continuation. After release or expiry, interrupt and retry the exact request
only after that same current run owner and exact Main session/incarnation has
acquired a distinct active, unexpired successor claim. Replay binds that
successor under the no-renew seal transaction and records both tuples in the
final proof. A same-ID revision change is not a successor; a different
session/incarnation/owner and a replay without a current claim return
`claim-not-active` without changing target authority.

Before the assignment becomes `cancelled`, the command persists a session-owned
authority quarantine. It deliberately leaves `assignment.worker_quarantine`
empty because released registry v3 reserves that field for reconciled submit
recovery. Direct CLI and HTTP resume, maintenance resume, claim, bootstrap, and
checkpoint paths read the session marker and return `worker-quarantined` before
a replacement launch identity, broker, or capability can be provisioned. The
marker remains until guarded retirement/deletion removes the retained session.

On success only that worker's coordination authority is revoked — its broker
stops, its claim is released, and its capability is removed. Nothing else
changes: the managed worktree and its unaccepted diff are untouched, other
workers keep their claims, and the run plus the Main Agent session stay active.
The result carries `input_sent: false` and `worktree_preserved: true`; no prompt,
paste, or Enter is ever sent. The v2 result reports
`worker_claim_active_after: false`. Its worker-claim proof reports
`active_disposition: "absent"`, whether an active claim was observed at stage
one, and conservative `release_provenance: "not_attributed_to_attempt"`. It
does not attribute the release to the current invocation, so a normal
completion, an exact retry after the seal committed, and a prior external
release all report the same terminal claim truth. Existing completed v1
receipts remain byte-stable on exact replay; new operations emit v2. The proof
also records the original and continuation controller claim tuples and whether
authorization was `original` or `successor`. Retire the assignment through the
ordinary path next. A later distinct replacement is still subject to the
`worker reassign` clean-worktree requirement, so review or discard the retained
diff first.

The command is revision-fenced and idempotent across two durable stages. Its
strict progress receipt is internal only; an identical concurrent invocation
never returns it as public success. If a call stops after quarantine and
terminalization, retry the identical request and idempotency key: it
revalidates the exact stopped worker lifecycle and the current original or
explicit same-Main successor claim, seals only that target, and converges the
terminal result without another revision. The
same retry is safe if the target seal committed but the process stopped before
the final receipt save: typed progress remains durable, the already-sealed
target is accepted idempotently, and subsequent completed replays do not
mutate coordination or orchestration state.

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
`working`. It also records a private typed companion identity for the exact
request-changes revision; the frozen assignment.v3 record remains compatible
and the human-readable checkpoint summary is not re-entry authority. The
worker must later submit a new exact result. A review revision created by an
older installed binary may backfill that companion only from one exact
controller-scoped `worker-request-changes` receipt whose run, revision, worker,
manager, guidance, and recomputed request digest all match. Missing, malformed,
or ambiguous receipt evidence fails closed.

If the exact Codex turn has already completed and its one private review
message is still unread, inspect `worker diagnose` first for current activity,
claim/operation, and guidance evidence. Retain the exact
`notification.generation` from the successful `worker message` result; that
result is the machine-readable generation fence. If it was not retained, fail
closed instead of guessing or creating another message. `worker reenter`
itself authoritatively validates the live worker incarnation, detached idle
composer, typed request-changes identity, quiescence, and unread guidance:

```bash
main-agent worker reenter ASSIGNMENT_ID \
  --worker-incarnation WORKER_INCARNATION \
  --if-revision ASSIGNMENT_REVISION \
  --if-notification-generation NOTIFICATION_GENERATION \
  --idempotency-key worker-reenter-001 \
  --format json
```

This action does not create another mailbox message or resend the assignment
prompt. It re-queues only the named notification generation. A crash-retained
reservation is revalidated against the current run, manager, typed
request-changes revision, worker, runtime, idle composer, coordination
quiescence, and unread guidance before any retry. The notification controller
still performs the final incarnation, idle-turn, live-runtime,
detached-session, authoritative-broker, no-claim, and no-operation checks
immediately before its one fixed body-free prompt and single Enter. An
unresolved submission outcome fails closed.

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
failed pre-claim assignment with no claim and no active/uncertain operation, or
through guarded `worker reconcile-stopped` for a `working` assignment whose exact
worker runtime is durably stopped, or through guarded `worker revoke-claim` for
a durably running authoritative-idle worker whose exact assignment-derived claim
remains active. Never synthesize cancellation by editing the private registry.

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
exact worker session identity. Before leaving the orchestration lock it stores
an assignment-revision-, worker-, controller-, request-, and
idempotency-bound delete reservation. That reservation fences handoff,
adoption, and every competing assignment mutation until the exact delete
replay finishes; a runtime-stop-fenced session is deletable only through the
matching terminal assignment and reservation. Retry an ambiguous result
exactly as described below. `cleanup_pending: true` means the logical deletion
succeeded but the retained physical cleanup record still needs the ordinary
session-maintenance path; it does not restore the worker card or assignment to
a live state.

### 7. Close the run and converge cards

After every worker has reached an accepted, released, or cancelled terminal
state, write the private final checkpoint and use the run-wide macro:

```bash
main-agent closeout \
  --if-run-revision RUN_REVISION \
  --checkpoint-file /private/path/final-checkpoint.json \
  --idempotency-key closeout-001 \
  --format json
```

Require `handoff_ready: true`. The result records checkpoint and final run
revisions, every worker disposition, bound controller-claim disposition, and
the final projection readback. `provider_session_preserved: true` confirms
that the Main Agent's own interactive provider session was not stopped or
deleted.

`handoff_ready: false` is a resumable partial result. Inspect
`progress_receipt.completed_stages`, `retained_exceptions`, and
`cleanup_pending`; resolve only the named worker or operation, then retry the
identical command, checkpoint content, initial run revision, and idempotency
key. Do not switch to a new key after a committed stage. A pre-provenance run returns
`controller-claim-provenance-required` rather than guessing ownership from a
matching context.

The lower-level `worker retire`, `close`, and `work-context release` commands
remain available for diagnosis and intentional primitive recovery. They are
not the normal closeout path because only `closeout` retains run-wide progress
and performs the final ownership/readback checks.

Confirm that `agent-session list` and the serve `GET /sessions` projection no
longer show live worker sessions and that the Main Agent projection reports
the closed run. Agent Console cards converge from those projections; never
repair a stale card by editing orchestration storage.

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
