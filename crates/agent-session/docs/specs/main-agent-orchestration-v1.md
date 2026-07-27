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
run revision. An assignment packet may declare `depends_on: [assignment_id]`
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
provider acceptance. When privacy-safe activity proves that an authoritative
provider turn completed or failed while the assignment still lacks a
checkpoint, readiness returns immediately with
`classification: submitted_or_waiting_without_checkpoint` and
`proof: authoritative-provider-turn-terminated`; it never waits for the outer
deadline or sends a recovery Enter after that proof. The wait takes no registry
lock, so it never blocks the
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
checkpoint, worker binding, state, or revision mutation. The `worker cancel`
command is the only public transition into `cancelled`. It may terminalize only the named
failed pre-claim assignment, with an exact revision, active Main Agent claim,
no worker claim, and no active or uncertain worker operation. The coordination
registry lock remains held across the orchestration transition so claim
acquisition cannot race cancellation. Operators MUST NOT synthesize
cancellation by editing the private registry.

### Supervision and primitive recovery

`worker diagnose` reads assignment, exact worker identity, provider activity,
claim, active/uncertain operation counts, and clean-worktree progress without
mutation. `worker supervise` is the repeatable bounded macro over the same
evidence. Its closed classifications are `healthy_progress`,
`startup_dialog_failure`, `pre_claim_failure`, `uncertain_mutation`,
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
