# Main Agent orchestration v1

## Status and ownership

- Status: implemented local contract.
- Owner: `nils-agent-session`.
- Public facade: the separate `main-agent` binary.
- Compatibility: additive to `agent-session.session.v1`; a session without an
  orchestration projection remains standalone.

The graph records workflow ownership, routing, recovery, and UI attribution. It
does not grant repository, provider, operating-system, or hook authority.
Coordination claims and operation leases remain the mutation authority.

## Trusted storage

The private registry is
`$AGENT_SESSION_STATE_DIR/orchestration/registry.json`; objective and assignment
packets are content-addressed under `orchestration/packets/`. Directories are
owner-only `0700`, files are owner-only `0600`, symlinks/foreign owners/unsafe
modes fail closed, and registry replacement is atomic under a bounded lock.

The supported schemas are:

- `agent-session.orchestration-registry.v3`
- `agent-session.orchestration-run.v1`
- `agent-session.orchestration-assignment.v3`
- `agent-session.session-orchestration.v1`
- `main-agent.objective-packet.v1`
- `main-agent.assignment-input.v1`
- `main-agent.checkpoint-input.v1`
- `main-agent.capabilities.v1`

Unknown fields, schema versions, and lifecycle states reject registry reads and
therefore reject mutations. Every identity reference is fenced by public
session ID, runtime incarnation, and original session `created_at`; `machine`
is an advisory routing/display hint only.

`nils-agent-session` 1.25.11 is the minimum reader and writer for
registry/assignment v3. It upgrades released v2 state in memory and,
immediately before the first successful v3 mutation, atomically preserves the
exact source bytes as the owner-only
`orchestration/registry.v2.rollback.json`. Only after that snapshot is durable
does it replace `registry.json` with v3. Version 3 owns the opaque
account-handoff reservation identity and every other post-v2 assignment field;
released v2 binaries fail closed on the new outer schema instead of receiving
unknown fields under the old version spelling. Historical v1 input remains
readable and preserves its exact source separately as
`registry.v1.rollback.json`, but v2 is the supported release rollback boundary.

Rollback is an explicit operator action: stop every orchestration writer,
verify no v3-only mutation must be retained, replace `registry.json` with the
preserved exact v2 rollback snapshot, and then start the released v2 reader.
The snapshot is a one-time migration source, not a live mirror; rolling back
after v3 mutations intentionally discards those newer mutations. A mismatched
pre-existing snapshot makes migration fail closed. The compatibility suite
uses a populated frozen v2 fixture and a deny-unknown-fields reader copied from
the released v2 contract; it restores the exact snapshot after representative
v3 mutations and never substitutes the current v3 decoder as rollback proof.

## Public projection and privacy

`agent-session list`, serve snapshots, and activity snapshots may add an
`orchestration` object to a session. It contains only run/assignment IDs, role,
relationship revision/state, bounded summaries, manager/collaborator/borrower
references, and worker counts. It never contains packet digests, packet bodies,
capabilities, prompts, transcripts, mailbox bodies, raw private paths, tokens,
environment values, tmux IDs, or PIDs.

Current relationships retain their exact incarnation identity. A resumed
worker with the same session ID and original `created_at` remains visible as
`role: "worker"` with `relationship_state: "rebind_required"` until its
authenticated, revision-fenced checkpoint updates the durable worker
reference. The assignment then retains `previous_worker` as bounded continuity
metadata for exact-controller guidance reconciliation. This continuity
projection is read-only metadata and grants no claim, operation, or repository
authority.

An authenticated `main-agent self show` or `rehydrate` may resolve the caller's
own private objective or assignment packet. A worker cannot resolve sibling
packets. Rehydration separates deterministic `durable` data from clock/liveness
dependent `observed` data.

## Facade lifecycle

All facade commands infer the caller from `AGENT_SESSION_CAPABILITY_FILE`; a
role environment variable is never authoritative.

```text
main-agent init --packet-file FILE --if-absent [--if-revision N] --idempotency-key KEY --format json
main-agent self show --format json
main-agent self recover --idempotency-key KEY --format json
main-agent rehydrate --format json|markdown
main-agent status --format json
main-agent checkpoint --file FILE --if-revision N --idempotency-key KEY --format json
main-agent bootstrap --idempotency-key KEY --format json
main-agent worker start --assignment-file FILE [--if-run-revision N] [--await-ready D] --idempotency-key KEY --format json
main-agent worker start --batch DIR --idempotency-key KEY --format json
main-agent worker list|show ...
main-agent worker wait [ASSIGNMENT_ID | --any] --until submitted|blocked|terminal [--timeout D] --format json
main-agent worker diagnose|supervise ASSIGNMENT_ID --format json
main-agent worker guidance-reconcile ASSIGNMENT_ID --if-revision N --idempotency-key KEY --format json
main-agent worker guidance-quarantine ASSIGNMENT_ID --if-revision N --idempotency-key KEY --format json
main-agent worker account-handoff ASSIGNMENT_ID --account ACCOUNT --if-revision N --authorize-account-change --idempotency-key KEY --format json
main-agent worker account-handoff-cancel ASSIGNMENT_ID --reservation-id RESERVATION_ID --account ACCOUNT [--intent-id INTENT_ID] --if-revision N --authorize-account-change --idempotency-key KEY --format json
main-agent worker request-changes ASSIGNMENT_ID --if-revision N --reason TEXT --idempotency-key KEY --format json
main-agent worker submit-recovery ASSIGNMENT_ID --if-revision N --timeout D --idempotency-key KEY --format json
main-agent worker reconcile-recovery ASSIGNMENT_ID --if-revision N --idempotency-key KEY --format json
main-agent worker stop-runtime ASSIGNMENT_ID --worker-incarnation INCARNATION --if-revision N --idempotency-key KEY --format json
main-agent worker stop-claimed-runtime ASSIGNMENT_ID --worker-incarnation INCARNATION --if-revision N --idempotency-key KEY --format json
main-agent worker reconcile-stopped ASSIGNMENT_ID --if-revision N --reason TEXT --idempotency-key KEY --format json
main-agent worker revoke-claim ASSIGNMENT_ID --worker-incarnation INCARNATION --if-revision N --reason TEXT --idempotency-key KEY --format json
main-agent worker cancel ASSIGNMENT_ID --if-revision N --reason TEXT --idempotency-key KEY --format json
main-agent worker reassign ASSIGNMENT_ID --assignment-file FILE --if-revision N --reason TEXT [--await-ready D] --idempotency-key KEY --format json
main-agent worker message|accept|release|delete ...
main-agent worker retire ID --if-revision N --idempotency-key KEY --format json
main-agent collaborate|borrow|handoff|adopt ...
main-agent close --if-revision N --idempotency-key KEY --format json
main-agent quick --assignment-file FILE [--tier L0|L1|L2|L3] [--await-ready D] --idempotency-key KEY --format json
```

`init` first confirms or acquires the caller-owned coordination claim, then
creates a run only with `--if-absent`. Continuity rebind requires the same
public session ID and original `created_at`, a fresh current capability, a
stopped prior incarnation, and the exact `--if-revision` fence. A recreated
session ID is therefore not continuity.

Every state mutation requires an active caller-owned claim, an expected
revision/absence fence, and an idempotency key. Read-only discovery does not.
Worker launch returns `pending-worker-checkpoint` until authenticated worker
self-check/checkpoint evidence advances the assignment; transport is never
reported as acceptance.

Assignment creation is fenced by the caller's active claim, the current-main
check, and assignment-absence — not by the run revision. `--if-run-revision` is
therefore optional on `worker start`: supply it to assert an expected run
revision, or omit it so parallel and batch launches do not serialize on a shared
run revision. Before creating a fresh assignment or provider session, `worker
start` MUST canonicalize `launch.cwd` and prove it is an existing directory.
Failure returns `assignment-launch-cwd-unavailable` with
`next_action: create-managed-worktree`; the invalid path is not persisted or
returned. A fresh Codex launch additionally requires that exact canonical
directory to carry `trust_level = "trusted"` in the active Codex
`config.toml`. Parent-directory trust is insufficient. Missing project trust
returns `provider-trust-required`; an unreadable, malformed, oversized, or
otherwise unverifiable configuration returns `provider-trust-unverified`.
The bounded read MUST reject non-regular files without blocking for external
input. The Codex configuration directory used for this decision MUST be
canonicalized, recorded in the pending start receipt and session runtime, and
passed explicitly to the child provider launch.
Both occur before assignment persistence, session creation, tmux launch, or
prompt delivery. Trust remains a user-owned provider decision and MUST NOT be
accepted automatically. Exact idempotent replay of an already persisted start
restores the request-digest-bound canonical cwd and provider configuration
directory from its pending receipt rather than re-running fresh-launch trust.
Historical pending receipts without either field perform the same bounded
preflight once and durably upgrade the receipt before launch. A successful
fresh preflight passes the canonical directory itself into session and provider
launch; the untrusted path spelling is not re-resolved at that boundary.
Because preflight I/O runs outside the orchestration registry lock, the
caller's active claim and all registry guards are revalidated before assignment
persistence. Before session creation, the exact active claim MUST acquire a
durable operation fence that blocks claim release or replacement through
successful child attachment. With that fence active, the exact current-main,
starting-assignment, pending-receipt, and batch-lane authority MUST be
revalidated before the child side effect; the fence becomes terminal only after
the child is attached or the launch fails. Fence identity MUST bind the request
digest, idempotency key, resolved assignment ID, and resolved worker session ID.
Each acquisition MUST carry a private generation token so a stale invocation
cannot terminalize a newer owner. A bounded sharded live-owner lock MUST prevent
concurrent exact replay from rotating that token; the replay joins the durable
receipt, and only proven owner-lock release permits crash takeover. Exact
pending replay MUST adopt the retained fence before attaching an existing
child; readiness or terminal receipt replay MUST finish the same fence before
returning so a post-commit cleanup failure cannot leave claim mutation blocked.

An assignment packet may declare `depends_on: [assignment_id]`
(same-run assignment ids, max 64). `worker start` refuses with
`dependency-not-satisfied` — listing each unmet dependency and its observed
state (or null when missing or in another run) — until every dependency has been
accepted (`accepted`, or the post-accept `released`). The ordering edge is stored
on the launched assignment and surfaced by `worker list`/`show` and `rehydrate`,
so it survives compaction. Dependency existence is enforced only at this gate,
never as a registry invariant, so releasing and deleting a dependency after a
dependent launches cannot brick registry reads. This is advisory ordering, not an
access-control boundary.

`worker start --batch DIR` launches every `*.json` assignment packet in `DIR`
as one call, fencing each lane independently so a failing lane isolates to its
own typed result (`{assignment_file, ok, result | error}`) instead of aborting
the batch; the command itself succeeds and the caller branches on each lane's
`ok`. Batch launch is transport-only and rejects `--await-ready`, so its
bounded lane count cannot multiply single-assignment readiness deadlines. The
parent idempotency key first commits an immutable sorted manifest of lane names
and raw packet digests. Exact replay resumes incomplete manifest lanes;
transient or ambiguous child failures remain incomplete and reconcile from the
child receipt, while deterministic failures become terminal lane results.
Manually repairable `assignment-launch-cwd-unavailable`,
`provider-trust-required`, and `provider-trust-unverified` lanes are returned
as isolated `ok: false, resumable: true` results while independent lanes
continue. The parent remains in progress; after the stated manual action, an
exact parent-key and manifest replay reclaims only those resumable lanes and
does not duplicate completed lanes.
Membership, name, order, or raw-byte changes conflict before any new worker
launch. The immutable parent manifest makes the historical
`{parent}-{index}` child key unambiguous, so new and rolling-upgrade callers
share one lane authority instead of splitting work across key schemes.
`main-agent quick --assignment-file FILE` is the L0/L1 fast-path: it
synthesizes an ephemeral run and work-context claim from the assignment (the
packet MUST declare a `repository`), launches the single worker in one call, and
marks the run ephemeral so it auto-closes once that worker is torn down — no
explicit `close`. A session that already controls a run must use the granular
`init` + `worker start` path instead. `worker start` and `quick` use the same
explicit `--await-ready` readiness proof and runtime-owned single-Enter
recovery described below. Bare `worker start` and `quick` both preserve the
released `5m` readiness default; callers opt out explicitly with
`--await-ready 0`.
A malformed duration is rejected before the ephemeral run is created.
Quick parent idempotency binds the canonical readiness duration; changing that
duration under the same key conflicts. Historical quick parent digests and
`{parent}-worker` child keys remain replayable for rolling upgrades.

`worker start --await-ready D` folds the readiness proof into launch: after the
worker is bound it waits up to a bounded `D` (0-5m; `0` = launch-only) for the
worker's authenticated, revision-fenced, incarnation-matched checkpoint to
advance the assignment past `starting`, then returns a typed `readiness`
(`ready` once it advanced, else `readiness_failed` with a `safe_state`). That
nonzero wait persists one fixed deadline and leased finalizer. Concurrent exact
replays join the same readiness attempt, a superseded finalizer cannot overwrite
its successor, and every replay converges on the same final receipt. The
readiness-progress receipt persists the automatic recovery reservation and its
reserved/sending/sent substage. The attempt reservation and `reserved`
continuation are committed atomically while rechecking the current finalizer
and its live lease. A successor after reservation continues that same
reservation; a successor after Enter observes the persisted sent result; an
ambiguous sending substage fails closed and never authorizes another Enter.
The `sending` lease outlives the pane-input timeout, while recoverable substages
retain the short takeover lease. The
checkpoint advancing is the readiness + newer-turn + identity proof, so the Main
Agent branches on one typed result instead of hand-running the verified-startup
sequence. `readiness.delivery.state: confirmed` requires that checkpoint and
names `authenticated-worker-checkpoint` as its proof. While a fresh Codex or
Claude launch remains `starting`, automatic readiness recovery and
`worker submit-recovery` share one durable reservation. The serialized send
boundary rechecks the exact incarnation, live tmux status, authoritative
`starting` activity with no turn or dialog, an incarnation-matched broker that
is ready, heartbeat-fresh, and backed by its matching capability, and the
absence of claims or active/uncertain operations. With those coordination
guards still held it revalidates the reserving Main Agent session/incarnation,
active controller claim, run controller, assignment manager, and exact
reservation immediately before sending at most one recovery Enter. A
definitive pre-delivery failure is recorded against that
reservation. A tmux timeout or wait failure is an unknown external-effect
outcome and preserves the `attempting` reservation plus mutation fences. No
path may retry input. The runtime keeps waiting inside the original deadline
and never resends the prompt. The additive
`submit_key_recovery` projection reports eligibility, attempt count, and
result; confirmation requires a newer authenticated checkpoint from the
reserved worker incarnation. A later accepted, released, or relationship
revision does not invalidate that worker-authored proof. Before such proof,
every manager-owned assignment mutation is fenced. A recovered checkpoint uses
`delivery.transport_state: submit-key-recovery-succeeded`. Existing/replayed
sessions, Hermes, stopped or replaced sessions, and any second recovery
keypress are ineligible. A final timeout reports `delivery.state: unverified`,
`automatic_retry_safe: false`, and explicitly forbids duplicate prompt or
further Enter injection. A successful terminal submit command alone is not
provider acceptance. Terminal readiness failure includes the additive,
content-free `prompt_observation` projection. Its `prompt` member compares the
private generated worker prompt with only the exact bound provider transcript
after the worker's creation time and reports `submitted`, `not_present`, or
`unavailable`. The complete exact-prompt observation, including the private
prompt read and provider-source resolution, has a fixed 500 ms allowance with
bounded reader admission; an expired or saturated observation degrades to
`unavailable` without delaying the readiness contract indefinitely. Its
`composer` member reports whether the tmux pane digest
changed between paste and the first Enter; it never includes pane or prompt
content. Persisted composer data is allowlisted to the three documented states
and reconstructed with the fixed proof label before output. Each pane
observation has a one-second command bound. `composer_not_ready` means the
paste was not visible in that bounded
pane projection and the exact prompt was not present in the authoritative
transcript. `prompt_not_present` means the exact prompt was absent even though
the pane changed. `prompt_observation_unavailable` preserves uncertainty when
either provider source binding or pane observation cannot prove more.
`checkpoint_timeout_after_prompt_submission` means the exact prompt reached
the bound provider transcript but no authenticated checkpoint followed before
the deadline. When privacy-safe activity also proves that the authoritative
provider turn completed or failed after exact prompt submission, readiness
returns `bootstrap_failure` immediately with
`proof: authoritative-provider-turn-terminated`; it never waits for the outer
deadline or sends a recovery Enter after that proof. A terminated turn without
exact prompt proof retains the applicable prompt/composer uncertainty
classification. None of these observations authorizes prompt resend or another
Enter. Recovery terminal paths retain the same observation:
`transport_uncertain` means the one recovery Enter has an unknown external
effect, `readiness_recovery_failed` means that bounded recovery failed
definitively, and `readiness_recovery_unavailable` means its durable
reservation was refused before input. The wait takes no registry lock, so it
never blocks the
worker's own checkpoint; `--await-ready 0` preserves the launch-only
`pending-worker-checkpoint` result. `worker retire ID` is the teardown macro: it
composes release -> delete and reports the worker's absence in one call,
replacing the hand-run
release -> delete -> confirm sequence. An accepted assignment is released first;
an already-terminal one goes straight to delete. Per-step idempotency keys are
derived from the retire key so a retry converges through each step's receipt.
Every single-worker start result also includes additive `polling` evidence. The
explicit launch-only mode reports zero readiness-registry reads and writes. A
bounded wait reports its timeout plus conservative read/write upper bounds
derived from the 250 ms readiness poll and five-second finalizer renewal.

Once a final durable `main-agent.worker-start-result.v1` receipt for the exact
controller, assignment, worker session, and worker incarnation records
`readiness.state: readiness_failed`,
`delivery.proof: worker-checkpoint-timeout`, and
`automatic_retry_safe: false`, supervision MAY classify the lane
`readiness_stop_required` only when the assignment is still `starting`, the
exact runtime is live, the worker claim is absent, operations are quiescent,
no submit-recovery record is still `attempting` or `sent`, and neither an
account-handoff reservation nor a runtime-stop reservation exists. An
account-handoff reservation classifies as `account_handoff_in_flight` and
routes to its typed cancellation/completion path. A durable runtime-stop
reservation classifies as `readiness_stop_in_progress` and routes only to a
typed executable exact replay containing the privately retained original
idempotency key. The additive
`main-agent.worker-diagnose-result.v3`,
`main-agent.worker-supervise-result.v3`, and
`main-agent.worker-recovery-action.v3` envelopes are emitted for
`account_handoff_in_flight` and the two runtime-stop classifications; existing
classifications retain the exact v2 top-level shape. The additive
`runtime_stop` projection is present only in the v3 diagnosis (and its nested
v3 supervision snapshot). The
required-stop recovery action MUST be Main-owned, directly executable, and contain the exact
`worker stop-runtime` argv with assignment revision, worker incarnation, and a
stable idempotency key.

The closed v3 classification and recovery-action unions remain unchanged.
`idle_claim_revocation_required` and `idle_claim_revocation_in_progress` use
`main-agent.worker-diagnose-result.v4`,
`main-agent.worker-supervise-result.v4`, and
`main-agent.worker-recovery-action.v4`. A v4 diagnosis carries the explicit
`claim_revocation.in_flight` projection and does not reuse the v3-only
`runtime_stop` projection. Existing v2 and v3 classifications retain their
exact schema identifiers and top-level projections.

`claimed_runtime_stop_in_progress` uses
`main-agent.worker-diagnose-result.v5`,
`main-agent.worker-supervise-result.v5`, and
`main-agent.worker-recovery-action.v5`. Its diagnosis carries only the
additive `claimed_runtime_stop.in_flight` projection. Its recovery action kind
is `exact_claimed_runtime_stop_replay` and contains the original assignment
revision, exact worker incarnation, and privately retained idempotency key.
Existing v2, v3, and v4 classifications retain their exact schema identifiers
and projections.

`worker stop-runtime` MUST authenticate the exact current Main controller and
its active, unexpired claim; revalidate run ownership, assignment revision,
primary manager, worker binding, and the final readiness receipt; and hold the
worker lifecycle lock through the runtime side effect. While briefly holding
the coordination and orchestration registries together, it MUST first persist
the session-owned exact-worker runtime-stop fence, then persist an exact
per-assignment reservation and strict claim-bound progress receipt before
sealing only that worker's broker/capability. A marker-first interruption MUST
be safely adopted by exact replay. It MUST release both global registry locks
before external process I/O. Competing assignment
mutations MUST reject the reservation, while unrelated coordination and
orchestration mutations remain available. It MUST refuse a worker claim, active/completing/
reconcile-pending operation, changed incarnation, non-live or unverifiable
runtime, missing final readiness proof, in-flight recovery, or an
account-handoff reservation. It terminates the verified
tmux/cgroup/process-session/process-group boundary without provider input.
The admitted controller claim is rechecked at the seal boundary; expiry after
that seal cannot restore the already-revoked worker authority, while any
crash/replay must authenticate a current active, unexpired claim anew. After
verified termination it CAS-finalizes the unchanged reservation, clears the
assignment fence, and stores the result while preserving the assignment
revision and state, session directory and record, and managed worktree. A
session-owned exact-worker runtime-stop fence MUST remain until guarded
retirement deletes the session; every CLI, HTTP, maintenance, broker, claim,
bootstrap, and checkpoint authority-restoration path MUST reject it. Exact replay MUST
return the stored result without a second termination. Its `in_progress` state
MUST block every non-runtime-stop assignment mutation, including ownership and
revision transfer after a marker-first interruption. Verified termination MUST
advance the fence to `stopped` before the assignment reservation is cleared. A following
stopped-runtime diagnosis MUST be non-healthy
and route to the existing guarded pre-claim cancel/retire/reassign lifecycle.
If the reservation controller is no longer live, orphan `adopt` MAY transfer
replay authority to an authenticated active successor Main only by atomically
rebinding the exact session fence, assignment reservation, and original
progress receipt. The worker, request digest, original idempotency key, and
reserved fence revision MUST remain unchanged. Adoption MUST fail while the
recorded controller remains live and MUST NOT restore worker authority.
Successive orphan transfers MUST remain recoverable: each transfer advances
the assignment ownership revision monotonically while the original reserved
stop revision remains immutable. The session fence MUST retain the prior
registry controller during its pre-commit rebind so a later successor can
replace an unavailable pending successor without widening any stop identity.
A `submit_recovery.state: failed` record alone is never a discriminator.
Prompt load-buffer, paste-buffer, and Enter effects occur exactly once for a
completed worker-start stage. A retry may remove and relaunch only an exact
matching worker record that durably proves tmux never launched; it never treats
that failed record as a completed launch.

The generated worker prompt starts with one deterministic,
worker-authenticated `main-agent bootstrap` command. The prompt names the exact
running `main-agent` executable rather than relying on the worker's `PATH`, so
bootstrap uses the same release that created the assignment. Bootstrap resolves
only the caller's bound assignment, reads that worker's private assignment
packet, derives the coordination claim from the packet's
repository/scopes plus the HMAC fingerprint derived from the authenticated
worker session's canonical `cwd`, and records the initial `working` checkpoint.
Its private `main-agent.bootstrap-result.v1` response also returns
`checkpoint_file`, the exact runtime-issued
`AGENT_SESSION_CHECKPOINT_FILE` for the authenticated session/incarnation.
Workers MUST write later checkpoint JSON to that pre-created owner-only file
before invoking `main-agent checkpoint --file` with the current revision and a
stable idempotency key; the generated prompt states this complete sequence.
An arbitrary project output path is not the managed-worker checkpoint-write
boundary. Before worker launch, compatibility-sensitive callers MUST require
`main-agent capabilities --provider <codex|claude> --format json` to return
`main-agent.capabilities.v1` with
`capabilities.runtime_checkpoint_file` exactly
`main-agent.runtime-checkpoint-file.v1`,
`capabilities.runtime_hook_checkpoint_write` exactly
`runtime-kit.checkpoint-write-admission.v1`, and `compatible:true`. The hook
capability is derived for that selected provider from the installed sibling
`agent-hook` inventory's
bundle version `2026.07.28.1` or newer and its locked
`agent-session.coordination.v1` rules. That bundle version
is the first policy whose paired handler admits the checkpoint write. The probe
also requires the selected provider's converged doctor record and executes its
installed handler's `runtime-kit.handler-capabilities.v1` self-probe, so new
policy with stale or missing handler code is rejected without coupling a
healthy installation to the other provider. Either mixed-deployment direction
therefore fails before worker launch. The `1.25.11` registry floor alone does
not prove this additive paired API.

The current controller incarnation MUST also pass `main-agent self readiness
--format json` before `init` or mutation. This authenticates the session and
requires its exact runtime-derived `AGENT_SESSION_CHECKPOINT_FILE` path to
refer to the expected owner-only regular file. `init`, `rebind`, and worker
`bootstrap` independently enforce the same precondition before claim
acquisition or orchestration mutation. A missing, mismatched, linked, or
permission-drifted file returns `runtime-checkpoint-unavailable` with a typed
resume-or-restart requirement.
Before minting the private checkout-shell grant, bootstrap requires the
packet's absolute `worktree`, `launch.cwd`, the durable assignment worktree,
and the authenticated session `cwd` to resolve to the same canonical checkout
root. The absolute assignment `worktree` remains private durable routing
metadata and MUST NOT be serialized into the fingerprint-only claim field. A failure
before claim acquisition advances the exact assignment to `blocked` with a
durable typed pre-claim blocker. Its
idempotency key is stable for the assignment, so an exact replay converges. A
claim conflict or stale assignment produces a typed failure before mutation;
it is not evidence that the prompt was undelivered and MUST NOT trigger prompt
resend or Enter injection. The Main Agent's claim and each worker assignment
MUST therefore use non-overlapping scopes, and mutating workers MUST launch in
their own managed worktrees.

`worker wait` is read-only completion-awareness for the orchestrating Main
Agent — the CLI counterpart to the operator console's sub-second SSE push. It is
a bounded (1-60s), level-triggered long-poll: given an assignment id or `--any`,
it returns once a watched assignment is in the `--until` target state
(`submitted`, `blocked`, or the terminal set `accepted|released|cancelled`), or
reports `{"outcome":"timeout"}` when the bound elapses. It takes no registry
lock and requires only the authenticated live main controller — no claim,
revision fence, or idempotency key. Like `pending-worker-checkpoint`, a
`--until submitted` result reports a state transition only; it is never itself
acceptance evidence, and the Main Agent MUST still gather the review evidence
below before `worker accept`.

### Interactive worker acceptance

`worker start` MUST resolve the assignment to a real
`agent-session.session.v1` record with `mode: "interactive"` and a tmux-backed
provider runtime. The worker MUST be present in `agent-session list` and the
serve `GET /sessions` projection with the matching orchestration relationship.
While the worker is expected to be live, it MUST yield real terminal output and
accept input through the bearer-protected WebSocket
`GET /sessions/{id}/attach` route used by Agent Console.

A metadata-only record, blank placeholder, unrelated/replaced incarnation, or
non-attachable worker does not satisfy launch acceptance. The Main Agent MUST
not infer readiness or task completion from
`pending-worker-checkpoint`; that state proves transport only. It MUST require
the worker's authenticated bootstrap/checkpoint and the task-specific review
evidence before `worker accept`. `self show` and `rehydrate` remain read-only
diagnostics for an already typed bootstrap or readiness failure; they are not
the normal prompt-delivery protocol.

After a provider resume, a revision-fenced checkpoint from the authenticated
worker may atomically rebind the assignment to its new incarnation only when
the session ID and original `created_at` still match and the prior incarnation
is no longer live. A worker checkpoint cannot regress a `submitted`,
`accepted`, `released`, or `cancelled` assignment to a pre-terminal worker
state. `worker request-changes` is the manager-only revision-fenced exception:
it permits exactly `submitted -> working`, preserves the bound worker and
private packet, clears stale result and blocker summaries, and records a
bounded review-revision checkpoint and reason atomically with the new
assignment revision. Its idempotency receipt is scoped to the authenticated
current Main Agent and exact logical request. Wrong roles, changed
primary-manager ownership, stale revisions, and every non-submitted source
state fail closed.

Accept/release are explicit Main Agent transitions. The ordinary successful
path is `submitted -> accepted -> released`. Once an assignment is `accepted`,
`released`, or `cancelled`, worker checkpoints are rejected before any result,
checkpoint, worker binding, state, or revision mutation. `worker cancel`,
`worker reconcile-stopped`, and the live-idle `worker revoke-claim` transition
below are the only public facade transitions into `cancelled`; daemon-owned
force group cleanup reaches the same state through its own contract below.
`worker cancel` may terminalize only the named
failed pre-claim assignment, with an exact revision, active Main Agent claim,
no worker claim, and no active or uncertain worker operation. The coordination
registry lock remains held across the orchestration transition so claim
acquisition cannot race cancellation. Operators MUST NOT synthesize
cancellation by editing the private registry.

### Live authoritative-idle claim revocation

`worker revoke-claim` owns the case where a `working` or `accepted` worker
runtime is still durably running, its provider has authoritatively returned to
an idle turn boundary, and its exact assignment-derived claim remains active.
It sends no provider input. A `working` assignment becomes `cancelled`; an
`accepted` assignment remains accepted. Without ownership transfer, both
transitions advance the assignment revision exactly once, while revision
exhaustion fails before any durable write.

Eligibility requires the authenticated current Main Agent and active exact
controller claim, current assignment revision and worker incarnation, durably
running process identity, authoritative idle activity, exact assignment-derived
worker claim, authoritative broker, and zero active or uncertain operations.
The session-record and activity locks remain held through the coordination seal.
Under that seal the command atomically persists a strict progress receipt and
per-assignment claim-revocation reservation. The reservation blocks every
competing assignment mutation.

The command persists the session authority quarantine after the reservation
and before exposing either crash boundary or applying the target-only seal.
That durable fence blocks resume, bootstrap, broker provisioning, claim
acquisition, checkpointing, and operation admission while the retained process
still exists. The seal then releases that worker claim, stops its broker, and
removes its capability. A crash after the reservation or seal is recovered only
by the identical request and idempotency key; supervision reports
`idle_claim_revocation_in_progress` with the exact replay argv even if the
runtime or activity evidence later drifts. Replay may rely on persisted
admission evidence only after the exact session authority quarantine exists.
If execution stopped after the reservation save but before that fence, replay
must re-prove the unchanged authoritative idle activity revision and running
runtime identity before persisting it. Otherwise it returns
`worker-claim-revocation-evidence-drift` without creating the quarantine or
changing either registry; the operator must restore a provably matching safe
boundary or retain the reservation for explicit recovery. Missing-fence replay
also revalidates the assignment-derived claim before writing the quarantine. A
matching claim may still be active, or it may already be released while the
same authoritative broker and quiescent worker remain; a different active
claim fails with `worker-claim-mismatch` and leaves the quarantine absent.
Finalization clears the reservation,
preserves the session and worktree, and stores
`main-agent.worker-revoke-claim-result.v1`. Stopped or unknown runtime evidence
on a fresh request is rejected and remains owned by `worker reconcile-stopped`.

If the reservation controller is no longer live, orphan `adopt` MAY transfer
the exact reservation and progress receipt to an authenticated active successor
Main. The worker, request digest, original idempotency key, original requested
revision, persisted activity/runtime proof, and session authority quarantine
identity MUST remain unchanged. Each successful ownership transfer advances
the assignment revision monotonically; terminal replay still uses the original
requested revision and does not add another revision after the transfer. A
crash before the atomic registry save leaves the prior owner authoritative, so
the unchanged adoption can be retried. If the controller crashed after the
reservation save but before quarantine persistence, adoption MAY reconstruct
that exact quarantine only while holding the worker lifecycle and activity
locks plus a matching active worker-claim seal. The current provider boundary,
runtime identity, assignment-derived claim, broker, and operation quiescence
MUST still match the persisted proof; drift fails closed. Concurrent identical
calls MUST re-read the receipt after acquiring the exact worker lifecycle lock:
a waiter adopts matching progress or returns the matching terminal result even
when both calls initially observed no receipt.

### Post-claim stopped-worker terminalization

`worker stop-claimed-runtime` creates the exact live-claim stopped-worker state
without sending provider input. It is distinct from the pre-claim
`worker stop-runtime` transition and does not terminalize the assignment or
release worker authority. The caller MUST be the authenticated current Main
controller and primary manager with an exact active, unexpired controller
claim. The assignment MUST be `working` at `--if-revision`; its worker MUST
match `--worker-incarnation`, have a verified live runtime and authoritative
idle activity boundary, retain the exact assignment-derived active unexpired
claim, retain the authoritative broker and work context, and have no active,
completing, reconcile-pending, or otherwise uncertain operation.

Admission holds the worker lifecycle and activity locks, then an observational
coordination guard while it binds the exact controller claim, worker claim,
activity revision, runtime-identity digest, broker, work context, and operation
quiescence. While that guard is held, the command persists a session-owned
`agent-session.claimed-runtime-stop-identity.v1` sidecar, the existing
runtime-stop fence, and then the strict
`main-agent.worker-stop-claimed-runtime-progress.v1` idempotency receipt. The
identity binds assignment, revision, exact worker and controller, request
digest, and the original idempotency key, so even an identity-first
interruption can project the exact replay without scanning global receipts.
The existing fence and registry-v3 shapes remain unchanged. Together the
identity and fence deny competing assignment mutation plus CLI, HTTP,
maintenance, broker, claim, bootstrap, checkpoint, and operation authority;
in particular, the held launch wrapper's clean `broker stop` cannot release
the worker claim after runtime termination. The command releases the
orchestration registry, then reacquires an observational coordination guard
that binds the exact controller and worker claim tuples and persists a narrow
mutation fence for those tuples. It releases the global registry lock before
stopping the exact tmux/cgroup/process-session/process-group boundary.

The independently versioned sidecars and process-owned mutation-fence lock
MUST make changes to those exact claim tuples fail closed for the full
termination interval while unrelated coordination remains available.
Fence activation first makes a one-way, wire-compatible coordination registry
marker transition from v1 to fence-aware v2, before publishing any manifest or
tuple sidecar. Older writers therefore fail closed at every partial activation
boundary rather than bypassing sidecars they do not understand. Current readers
accept both markers, and exact replay reconstructs partial v2 activation before
termination. An interrupted owner releases the OS lock so a later exact replay
can recover without a wall-clock safety lease. Session-owned
runtime-stop fencing independently denies every worker claim-changing ingress
until guarded retirement, including raw claim replacement and advisory
set/clear. A post-stop observational
transaction MUST prove the same worker claim ID, revision, expiry, session, and
incarnation plus the original Main controller claim remain exact, active, and
unexpired. The worker claim expiry MUST exceed the bounded termination window. The first TTL-window
check occurs while the initial coordination guard is held and before any
identity, fence, or progress receipt is persisted; a second check immediately
before termination catches elapsed-time drift after reservation. If persisted
runtime evidence already proves this operation stopped the exact runtime,
replay may finalize under the authenticated current controller without
repeating termination. Finalization stores
`main-agent.worker-stop-claimed-runtime-result.v1` without advancing the
assignment revision, clears the exact claim-mutation fence, then advances the
session fence to `stopped`. If the
process fails between those writes, exact completed replay MUST finish the
fence transition. The result reports `runtime_stopped:true`,
`worker_claim_active_after:true`, `input_sent:false`, and preserves the
session and worktree. The identity and fence remain until guarded retirement.
Competing assignment mutations fail closed from the identity write until the
fence reaches `stopped`. An interrupted attempt is classified as
`claimed_runtime_stop_in_progress` and only the exact v5 replay argv may resume
it; an exact completed replay returns the stored result without another stop.
If the process was durably stopped but an interruption outlives the worker
claim TTL, exact replay MUST retire the bounded claim-mutation fence and commit
the same result schema with `worker_claim_active_after:false` plus
`proof.interrupted_after_stop_claim_degraded:true`. That truthful degraded
result is not B2 field-closure evidence, but it restores the typed
`reconcile-stopped` path without leaving controller or assignment authority
permanently fenced.

If the Main controller dies after the claimed stop reaches `stopped`,
authenticated orphan `adopt` MUST rebind both persistent stop identities to
the successor and adopted assignment revision before the registry ownership
transition. Both identities retain the original stop revision and controller
as immutable lineage. A crash after the sidecar rebind but before the registry
save leaves the old assignment owner in the registry; exact adoption replay
MUST recognize the partially advanced sidecars and converge without weakening
the runtime quarantine. If that pending successor also dies, a later
authenticated successor MAY chain the rebind only when the sidecar remains at
the pending adopted revision, its immutable origin exactly matches the
registry source, and the intermediate controller is proven non-live.
`reconcile-stopped` then advances from the adopted
revision, and terminal worker deletion MUST accept that rebound lineage rather
than requiring the dead controller's original revision.

`worker reconcile-stopped` terminalizes a `working` assignment whose exact
worker runtime is durably stopped. `worker cancel` deliberately refuses that
state: bootstrap already recorded the `working` checkpoint, so the failure is
post-claim and the worker's assignment-derived claim may still be alive on TTL.
Without this transition such a lane could only be closed by daemon force group
cleanup, which deletes the Main Agent session that would have to run it.

Eligibility is exact. The caller MUST be the authenticated current Main Agent
with an active claim, the run controller, and the assignment primary manager.
Authentication and replay admission use a non-renewing coordination read, so a
healthy broker cannot silently renew an expired claim before authorization is
checked.
The assignment MUST be `working` at the supplied `--if-revision`, with no submit
recovery in flight and no account-handoff reservation in flight. The bound
worker's session record MUST match the exact incarnation. Stopped proof combines
the persisted cgroup, process-session, and process-group runtime identity with
the tmux session state: live evidence dominates, unavailable or unresolvable
evidence stays unknown, and both sources MUST agree the runtime is gone. A
running runtime returns `worker-runtime-still-live`; unknown runtime evidence
returns `coordination-runtime-unverified`. Neither is automatically retry-safe.

Each stage holds the exact session-record lock as its lifecycle boundary, so
resume or replacement cannot restore an executing runtime while stopped proof
is established or authority is sealed. The first-stage coordination guard
refuses any active, completing, or reconcile-pending operation lease and any
present incarnation-mismatched broker, and binds the exact active, unexpired
controlling Main Agent claim to the stopped-worker observation. The second
stage reacquires both guards, re-proves the same stopped incarnation and
runtime-identity digest, and revalidates the controller claim by claim ID,
revision, state, incarnation, and expiry in the destructive coordination
transaction. Unlike `worker cancel` an active worker claim is expected and
permitted, and a stopped or absent worker broker is not a liveness dependency.
Unaccepted worktree content is deliberately not a precondition: preserving the
stopped worker's diff is the purpose of the transition.

Execution is two durable stages under one idempotency key. The first stage
first persists the session-owned authority quarantine, then terminalizes the
assignment as `cancelled`, advances the revision once, records the bounded
reason as the blocker summary, and stores a strict typed
`main-agent.worker-reconcile-stopped-progress.v1` receipt in the same
orchestration save. The session quarantine is deliberately not projected into
`assignment.worker_quarantine`: that v3 field remains reserved for a
`reconciled` submit recovery, so both interrupted and completed B2 registries
remain readable by the released v3 contract. The progress receipt's closed
fields include `state:
"in_progress"`, `stage: "authority_quarantined"`, the exact assignment and
worker, a 64-hex runtime-identity digest, Boolean claim observations, and the
exact controlling claim ID, revision, and expiry epoch observed in stage one.
Missing, mistyped, unknown, mismatched, or invalid-stage progress fails with
`orchestration-store-invalid` before cleanup. Progress is never a public
success result.

The second stage revokes only that worker's coordination authority — stopping
its broker, releasing its claim when still active, and removing its capability
— and finalizes the receipt as
`main-agent.worker-reconcile-stopped-result.v2` while the controller-claim
transaction remains held. The unchanged original claim tuple authorizes normal
continuation. If that claim was released or expired, a fresh exact replay by the
same current run owner and exact Main Agent session/incarnation may explicitly
bind one distinct active, unexpired successor claim. The original tuple stays
in progress, and the final proof records both original and continuation tuples
plus `mode: "original"|"successor"`. Same-claim revision or expiry drift is not
a successor, and a different session, incarnation, or run owner cannot take
over progress. Without a current active claim, replay returns
`claim-not-active` and leaves target authority unchanged.

The order is deliberate: an interruption between stages leaves a terminal
assignment whose stopped worker may still hold a claim, but the retained
session already carries an incarnation-independent authority quarantine.
Direct CLI resume, HTTP resume, maintenance resume, claim, bootstrap, and
checkpoint paths therefore fail with `worker-quarantined` before provisioning
a launch identity, broker, or capability. An exact replay parses the progress,
revalidates the session marker, ownership, and stopped lifecycle, and converges
the second stage;
concurrent exact calls either converge to the terminal result or receive the
typed in-progress error, never a progress-shaped success. Exact replay of a
completed request returns the original terminal receipt without advancing the
revision again. If execution stops after the target-only seal is durable but
before the terminal receipt save, the strict progress receipt remains. An
identical retry proves the stopped lifecycle, binds the current original or
explicit successor claim, observes the already-sealed target, and commits the
final result; every later replay is side-effect free. All B2 capability
authentication, claim capture, and target-seal snapshots use an observational
coordination lock that performs no notification normalization, claim renewal,
operation renewal probe, or maintenance write. Normal coordination callers
retain full maintenance.

The result reports `terminalized`, `worker_claim_active_after: false`,
`input_sent: false`, `worktree_preserved: true`,
`automatic_retry_safe: false`, and a bounded `proof` object naming the stopped
runtime, its privacy-safe runtime identity digest, the exclusive record-lock
lifecycle boundary, coordination quiescence, and original/continuation
controller authorization. The worker-claim proof reports the stable terminal
truth `active_disposition: "absent"`, the persisted stage-one observation, and
the conservative `release_provenance: "not_attributed_to_attempt"`. It does not
claim whether the normal attempt, a pre-receipt attempt that committed the seal,
or a prior external release made the claim inactive. Therefore normal
completion and recovery after the durable seal cannot report contradictory
results for the same logical operation. The prior
`main-agent.worker-reconcile-stopped-result.v1` shape is closed: existing
completed v1 receipts remain byte-stable on exact replay, but new completions
emit v2. No path
through this command loads a prompt, pastes, sends Enter, deletes a session,
closes the run, touches the managed worktree, or changes any other assignment,
worker claim, or broker. After success the worker session record is retained
until ordinary `worker retire`, and a distinct replacement assignment remains
subject to the existing `worker reassign` clean-worktree requirement.

The assignment registry admits `worker_quarantine` only for an exact
`reconciled` submit recovery. B2 uses only the session-owned marker, which
remains until guarded retirement/deletion removes the retained session
directory; successful target sealing does not clear it.

### Supervision and primitive recovery

`worker diagnose` reads assignment, exact worker identity, provider activity,
claim, active/uncertain operation counts, and clean-worktree progress without
mutation. `main-agent.worker-diagnose-result.v2` and
`main-agent.worker-supervise-result.v2` publish the expanded closed
classification union; `main-agent.worker-recovery-action.v2` publishes the
matching closed action-kind union, including
`stopped_worker_terminalization`. The v1 schema ids remain the prior closed
contracts and are not emitted with the expanded union. `worker supervise` is
the repeatable bounded macro over the same evidence. Its v2 closed
classifications are `healthy_progress`,
`startup_dialog_failure`, `pre_claim_failure`, `post_claim_failure`,
`uncertain_mutation`,
`submitted_or_waiting_without_checkpoint`, `safe_reassignment`,
`worker_unreachable`, and `evidence_unavailable`; every result includes a
deterministic `next_action`. Packet, session, activity, coordination, and
worktree evidence is projected as `present`, `absent`, `unavailable`, or
`identity_mismatch`. A missing bound worker is `worker_unreachable`; corrupt,
unreadable, or identity-mismatched required evidence is
`evidence_unavailable`. Both classifications are non-retry-safe and forbid
automatic input or mutation.

The full supervision classification set additionally includes
`coordination_broker_stale`, `edit_authority_stale`,
`claim_renewal_required`, `guidance_continuity_required`,
`orphan_guidance_quarantine_required`,
`account_handoff_capability_gap`, `account_handoff_required`, and
`stale_provider_activity`. Broker-heartbeat/edit-authority staleness is not
claim-expiry evidence: only `claim_renewal_required` directs the exact worker to
renew its own claim. `coordination_broker_stale` routes to exact-incarnation
broker-owner recovery; `edit_authority_stale` requests a bounded recheck while
no durable broker-lost timestamp exists.

`post_claim_failure` is the `working` counterpart of `pre_claim_failure`: the
assignment is `working`, its bound worker still matches, and both the tmux
session state and the persisted process runtime identity prove the exact
incarnation is durably stopped. Only the fail-closed guards
`evidence_unavailable`, `worker_unreachable`, and `uncertain_mutation` outrank
it. It MUST outrank `coordination_broker_stale`, `edit_authority_stale`,
`claim_renewal_required`, the guidance and account-handoff classifications,
`startup_dialog_failure`, `stale_provider_activity`, and `healthy_progress`,
because every one of those prescribes an action that requires a live worker.
Its `next_action` names `worker reconcile-stopped`, its `recovery_action.kind` is
`stopped_worker_terminalization`, and `automatic_retry_safe` is `false`.
Diagnosis additionally publishes `post_claim_terminalization_safe`, which is
independent of worktree cleanliness. This classification MUST NOT be reported as
`pre_claim_failure`: `worker cancel` requires an absent worker claim and refuses
a `working` assignment, so routing it there would publish an unexecutable
instruction. An unknown or live runtime keeps its ordinary live-worker
classification rather than becoming terminalizable.
If persisted process evidence is stopped while the tmux wrapper still appears
live, the sources conflict and supervision MUST return
`evidence_unavailable`; a visible wrapper MUST NOT overrule exact stopped
process evidence into `healthy_progress`.

Worktree staleness uses a durable, privacy-safe material fingerprint scoped to
the assignment and exact worker incarnation. The bounded input combines
porcelain path state, staged and unstaged binary/full-index diffs, and
untracked path/content bytes. Same-path and same-size continued edits,
untracked content changes, and deletion-only changes produce new fingerprints.
Oversize, timeout, non-regular, unreadable, or inconsistent enumeration is
unavailable. An unchanged fingerprint retains its original `changed_at`; a new
worker incarnation starts a new progress clock even when the material matches
the prior incarnation.

`self recover` is the only facade macro for controller broker recovery; there
is no top-level alias. It requires the exact current Main Agent role and
incarnation, unchanged running persisted runtime identity, matching broker
generation and runtime digest, an active retained claim, and no active or
uncertain operation. It delegates to the broker adopt primitive, treats an
already-authoritative exact broker as an idempotent `healthy_noop`, and
post-verifies the same run, claim revision, and broker identity. It never
changes accounts, resumes/replaces providers, resends prompts, sends Enter, or
clears operation fences.

Authenticated resume bootstrap releases the immediately stale worker claim,
acquires the assignment-derived claim for the current incarnation, and carries
only unread, unexpired guidance from the exact current controller. Carried
guidance retains message identity and unread state, advances revision once,
and records bounded forwarding provenance. Unrelated-controller, expired,
read, and acknowledged messages remain on their original incarnation.
`worker guidance-reconcile` is the revision-fenced, idempotent controller
action for a retained `previous_worker`; it revalidates exact manager and
worker identities across coordination-to-orchestration locking, never exposes
message bodies, and never records worker consumption.
When stale exact-controller guidance has no retained `previous_worker`,
supervision instead prescribes `worker guidance-quarantine`. That action
revalidates the current manager, assignment revision, worker, and absence of a
prior identity under the same lock order, then marks only unread/unexpired
non-current-incarnation records from that controller as `quarantined`.
Current-incarnation and unrelated-controller messages are preserved.

`worker account-handoff` is available only when the exact incumbent Codex
worker exposes both managed app-server account control and structured
auto-resume control. Explicit `--authorize-account-change`, current assignment
revision, exact Main Agent authority, authoritative worker broker, active
worker claim, operation quiescence, and a `starting|working|blocked` assignment
are mandatory. Account input is validated before reservation, and an account
handoff reservation is mutually exclusive with submit recovery. The macro queues or joins
the typed next-account transition, waits for the same worker incarnation,
verifies the allowlisted durable binding, and re-arms a structured quota
continuation when eligible. `/logout`, raw terminal input, prompt resend, and
worker/session replacement are forbidden. Raw workers advertise no restart
flag and fail closed without account/runtime mutation. A bounded raw
rate-limit probe is eligible only after both provider and material progress are
stale, no structured quota evidence exists, managed controls are absent, and
an exact durable selected-account provenance exists; ambient authentication is
never account provenance.

Failed, superseded, and queued timed-out reservations are recoverable through
`worker account-handoff-cancel`. The typed action requires explicit
authorization and revalidates exact manager, revision, reservation, worker,
broker, claim, and operation quiescence. It atomically compare-and-clears the
observed queued/failed pending account and revision plus the matching
reservation while preserving the selected account and leaving auto-resume
disabled. If a newer pending intent superseded the reservation, cancellation
succeeds by clearing only the stale assignment reservation and reports the
newer intent as preserved; no cancellation retry is needed because the stale
reservation is gone. It refuses an actively applying owned intent and
directs an already-applied intent back to exact replay of the original
handoff.

The cancellation selectors come from the durable reservation returned by the
failed handoff or a private authenticated assignment view:
`reservation_id` supplies `--reservation-id`, `account` supplies `--account`,
and `account_intent_id` supplies `--intent-id`. Current v3 reservations require
all three identities. A frozen released-v1 reservation predates the stored
opaque reservation field and provider-side intent identity; its authenticated
view derives the stable reservation selector from the request digest and may
omit only `--intent-id`. The exact reservation and account selectors remain
mandatory at the CLI boundary.

`worker submit-recovery` is eligible only for an incarnation-matched
assignment in the initial provider startup state, with no dialog, claim,
active/uncertain operation, prior recovery record, or authoritative terminal
turn. It durably reserves its single attempt before sending exactly one Enter,
uses the same serialized send boundary as automatic recovery, waits for the
exact newer worker checkpoint, never resends the prompt, and refuses every
second attempt even under a different idempotency key.
The explicit command stores a provisional idempotency receipt atomically with
the reservation. An exact replay joins or reconstructs that attempt without
resending input; a different key is refused for an explicit attempt. An
interrupted automatic attempt may be adopted under an explicit receipt for
observation, but that path never sends input or revokes a potentially live
sender. An unknown pre-send outcome remains mutation-fenced while the reserved
incarnation might act; only a newer worker checkpoint or the guarded non-resend
reconciliation primitive resolves it after the sender returns.
While recovery is `attempting` or `sent` and no newer worker checkpoint exists,
all manager-owned assignment mutations fail with
`submit-recovery-in-flight`, including relationship changes and force group
cleanup. Timeout, terminal provider activity, send failure, or checkpoint
confirmation after a recorded send resolves that fence. An observer timeout
while the record is still `attempting` reports
`submit-recovery-send-outcome-unknown` and preserves the fence. That unknown
outcome does not finalize the provisional idempotency receipt: an exact-key
retry remains observation-only and can later upgrade the receipt when a newer
reserved-worker checkpoint or definitive failure resolves the attempt.

`worker reconcile-recovery` is eligible only for an unknown `attempting`
reservation bound to the current run, controller, assignment, and exact worker
incarnation. It acquires the exact session-record lock, proves the tmux/runtime
is stopped by combining all persisted cgroup, process-session, and process-group
sources, then retains the worker coordination-quiescence guard while proving
the worker claim absent and active/uncertain operations quiescent. Under the
established coordination-to-orchestration lock order it revalidates current
Main Agent authority and the unchanged revision before terminalizing the record
as `reconciled`. Live evidence dominates, unavailable evidence remains unknown,
and stopped requires every available identity source to prove absence.
Platforms that cannot enumerate descendants treat process-group disappearance
as unknown rather than stopped. Before the `reconciled` transition, a
session-owned marker is atomically persisted with the exact-worker quarantine;
the registry transition follows only after that durable fence exists. An exact
retry adopts a matching marker if execution stopped between those commits. Resume,
maintenance resume, claim, bootstrap, checkpoint, and equivalent execution
authority restoration reject that retained worker until guarded terminal
cleanup makes execution impossible. The result records stopped/quiescent proof and
`input_sent:false`. No path through this command loads, pastes, sends Enter, or
clears a fence while the reserved incarnation might still execute. Cancellation
of this absorbing record reacquires the exact session-record boundary, reproves
the stopped runtime/tmux state, and holds coordination quiescence through the
revision-fenced transition. A stopped or absent matching broker is therefore
not a liveness dependency; a present incarnation mismatch still fails closed.
The exact Main Agent claim is revalidated from the held coordination guard
immediately before the orchestration mutation, so concurrent claim release
cannot terminalize the assignment.

`worker reassign` composes diagnosis, `worker cancel`, ordinary guarded
retirement, and `worker start`. Both retained and replacement worktrees MUST be
clean; replacement assignment ID, session ID, and canonical worktree MUST be
distinct when a session ID is supplied; omission uses the ordinary deterministic
`worker start` derivation. The reason is recorded on the cancelled assignment.
The macro stores progress after cancel, retire, and start. Exact retries inspect
the top-level receipt before mutable preconditions and resume from the last
completed stage without repeating it. A transient delete/kill failure retains
the old session and its pending retire receipt; the exact replay skips
cancellation, converges that retirement once, and starts the replacement once.
A failed never-launched replacement is safely replaced under its stable session
ID, so completed prompt transport is not repeated. A macro failure returns `failed_stage`
and `last_proven_safe_state`. Ordinary retirement treats a proven-absent exact
session as an idempotent logical delete, records the normal
tombstone/receipt/revision, and never fabricates inner success. Trust, update,
authentication, permission, and MCP dialogs are diagnostic classifications
only and MUST never be accepted automatically.

Composite commands derive bounded, digest-backed child idempotency keys.
`worker retire` stores top-level progress before release, after release, and
after delete, so retrying the original key and revision resumes an accepted
assignment whose release already committed.

The delete stage persists a private
`main-agent.worker-delete-identity.v1` reservation while holding the
orchestration registry lock. It binds the assignment revision, exact worker,
primary controller, request digest, and idempotency key before session
deletion begins. While present it rejects handoff, adoption, and every
competing assignment mutation. Only the exact delete replay can finish or
clear it, and deletion of a runtime-stop-fenced terminal worker additionally
requires that matching reservation. This prevents a stale assignment clone
from authorizing session deletion after ownership transfer.

Collaborators and bounded borrowers are visible routing metadata, not write
authority. Borrow expiry does not change primary ownership. Handoff requires a
live target Main Agent and a quiescent current manager. The source coordination
guard is held through one atomic run-plus-primary-manager update. Handoff
rejects both the assignment's own dependencies and reverse source-run
dependents, because moving either edge would make it cross-run. Worker-message
delivery uses the same coordination-to-orchestration lock order and retains
both locks after revalidating the exact active sender claim, run, primary
manager, and worker through mailbox persistence. Handoff revalidates the
source claim from its retained coordination guard. Thus a source message
commits before handoff or fails after it, and a concurrent claim release
invalidates either mutation at its boundary. Adoption requires the old manager
reference to be stale. Delete
requires a released/cancelled assignment, released worker claim, no active or
uncertain operation, and the producer's exact logical-delete/tombstone checks.
Physical cleanup failure is a maintenance record and cannot restore the live
session projection.

### Daemon-owned group cleanup

The bearer-protected serve route
`GET /sessions/{id}/orchestration/group-cleanup` previews deletion of one exact
Main Agent group. Its `agent-session.main-agent-group-cleanup.v1` response
contains the exact Main Agent reference, active run ID and revision, a sorted
list of assignments still primarily managed by that Main Agent, whether each
requires force, and a digest over the complete plan. Collaborators, borrowers,
workers handed off to another primary manager, and assignments from another
run are outside the plan.

A single plan is capped at 64 primary assignments. This bound keeps the
resumable per-worker result/fence checkpoints, serialized receipts, and
exclusive worker-authority lock set within a fixed worst case; preview fails
with `group-cleanup-batch-too-large` before worker locks are acquired when the
run exceeds it. Split a larger objective into smaller runs before cleanup.

`POST` accepts only
`agent-session.main-agent-group-cleanup-request.v1` with the previewed Main
Agent incarnation, run revision, plan digest, `safe` or `force` mode, and an
idempotency key. Any identity, revision, or plan drift fails before cleanup.
Safe mode rejects a plan containing nonterminal assignments. Force mode
terminalizes exactly those assignments as `cancelled`; accepted assignments
advance to `released`. Before any transition becomes durable, cleanup locks
the coordination registry for every exact worker. Active or uncertain
operation leases reject both modes. Safe mode also rejects an active claim;
force may revoke that claim only after proving operation quiescence, and seals
the broker/capability boundary before releasing the lock so no new lease can
race deletion.

Execution is deliberately ordered: delete or confirm absence of every planned
worker, close the run, then delete the Main Agent. Worker identity is checked
again before deletion. A worker failure returns a typed
`agent-session.main-agent-group-cleanup-result.v1` partial result and preserves
both the active run and Main Agent. A failure after worker deletion but before
or during Main deletion still reports `main_deleted: false`; clients reconcile
the per-worker outcomes and keep the Main Agent available for recovery. An
incomplete result is a provisional durable checkpoint: an exact retry resumes
after the last committed stage and may converge to success without repeating
completed deletions. Once `completed: true` is stored, exact replay is stable
and returns that terminal result byte-for-byte at the value boundary.

Incomplete checkpoints use the private
`agent-session.main-agent-group-cleanup-receipt.v2` schema. The receipt stores
the exact original `requested_session_id` beside the canonical
`principal_session_id`; post-delete recovery requires exact selector equality,
the canonical identity from the embedded plan, and its matching pending
registry fence. The v1 receipt remains readable only when the requested
selector exactly equals its stored principal and the same plan/fence checks
succeed. The same schema-specific selector check applies while the canonical
session is still live: a retry through another alias cannot adopt or rewrite
the original selector mapping. Prefix, abbreviation, and unique-scan inference
are never ownership evidence. Completed alias cleanup persists an exact
alias-keyed terminal receipt alongside the canonical receipt, but only the
receipt keyed by the current exact selector is replayable.

Progress-capacity reclamation treats unreadable, malformed, changing, or
uncertain-principal records as live. A classified stale record is never
removed by a verify-then-pathname-unlink sequence. Reclamation instead uses one
private bounded recycle slot and an atomic exchange: the replacement identity
is verified on both paths, a file-count reclamation installs the complete
incoming checkpoint by atomic rename, and byte-only reclamation installs a
valid compact retired receipt. Before exchange, a durable journal binds the
source key and optional destination key to both descriptors' device, inode,
length, modification time, and content digest. Recovery uses that exact
mapping to roll back a stranded replacement or finish an already-installed
checkpoint; unknown identities fail closed unless the exact stale source
remains in the slot and the unrelated progress-path replacement can therefore
be preserved without ambiguity. The recycle and progress directories are both
pinned by validated descriptors. Exchange, final rename, rollback, and
recovery use those authorities, and both changed directory namespaces are
synced before the durable journal may transition to idle. Canonical
directory-identity drift aborts before exchange. If a pathname replacement
races an exchange, both resulting identities are checked and the displaced
replacement is exchanged back before the transaction is retired. A replacement
after exchange is likewise retained when the exact stale source remains in the
slot. Final installation records a durable `installed` journal phase only after
the prepared destination identity and changed progress namespace are synced.
An unproven destination identity is atomically moved back to the source key and
verified before the transaction retires; after the installed phase is durable,
destination replacement or removal is retained as the later state and recovery
can retire the exact stale slot without ambiguity. Reclaimable bytes are
classified and summed before any sync-heavy compaction begins, and active
residue is recovered before the capacity snapshot, so an admission that is
provably impossible does not mutate stale receipts and a recovered size change
cannot invalidate its projection. A matching canonical pending fence keeps
released-v1 alias progress live even if the alias now resolves to another session. A
crash therefore cannot create unbounded residue or permanently disable
admission.

## Recovery and failure semantics

Revision conflict returns `orchestration-revision-conflict` with
`current_revision`. Rebind without a fence returns
`orchestration-revision-required`. Unsafe/corrupt storage returns a typed data
or unavailable error before mutation. Idempotent replay returns the original
outcome; reusing a key for different input fails closed.

The durable recovery capsule is ordered by assignment ID through the registry's
ordered maps. Its `observed` section alone contains `observed_at` and current
liveness annotations, so compaction/resume clients can compare durable state
independently.
