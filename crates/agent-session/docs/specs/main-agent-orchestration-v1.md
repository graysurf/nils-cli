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

- `agent-session.orchestration-registry.v1`
- `agent-session.orchestration-run.v1`
- `agent-session.orchestration-assignment.v1`
- `agent-session.session-orchestration.v1`
- `main-agent.objective-packet.v1`
- `main-agent.assignment-input.v1`
- `main-agent.checkpoint-input.v1`

Unknown fields, schema versions, and lifecycle states reject registry reads and
therefore reject mutations. Every identity reference is fenced by public
session ID, runtime incarnation, and original session `created_at`; `machine`
is an advisory routing/display hint only.

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
reference. This continuity projection is read-only metadata and grants no
claim, operation, or repository authority.

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
main-agent rehydrate --format json|markdown
main-agent status --format json
main-agent checkpoint --file FILE --if-revision N --idempotency-key KEY --format json
main-agent bootstrap --idempotency-key KEY --format json
main-agent worker start --assignment-file FILE [--if-run-revision N] [--await-ready D] --idempotency-key KEY --format json
main-agent worker start --batch DIR --idempotency-key KEY --format json
main-agent worker list|show ...
main-agent worker wait [ASSIGNMENT_ID | --any] --until submitted|blocked|terminal [--timeout D] --format json
main-agent worker message|accept|release|delete ...
main-agent worker retire ID --if-revision N --idempotency-key KEY --format json
main-agent collaborate|borrow|handoff|adopt ...
main-agent close --if-revision N --idempotency-key KEY --format json
main-agent quick --assignment-file FILE [--tier L0|L1|L2|L3] --idempotency-key KEY --format json
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
`ok`. `main-agent quick --assignment-file FILE` is the L0/L1 fast-path: it
synthesizes an ephemeral run and work-context claim from the assignment (the
packet MUST declare a `repository`), launches the single worker in one call, and
marks the run ephemeral so it auto-closes once that worker is torn down — no
explicit `close`. A session that already controls a run must use the granular
`init` + `worker start` path instead.

`worker start --await-ready D` folds the readiness proof into launch: after the
worker is bound it waits up to a bounded `D` (0-5m; `0` = launch-only) for the
worker's authenticated, revision-fenced, incarnation-matched checkpoint to
advance the assignment past `starting`, then returns a typed `readiness`
(`ready` once it advanced, else `readiness_failed` with a `safe_state`). That
checkpoint advancing is the readiness + newer-turn + identity proof, so the Main
Agent branches on one typed result instead of hand-running the verified-startup
sequence. `readiness.delivery.state: confirmed` requires that checkpoint and
names `authenticated-worker-checkpoint` as its proof. While a fresh Codex or
Claude launch remains `starting`, the runtime rechecks its exact incarnation
and live tmux status, sends at most one recovery Enter, and keeps waiting inside
the original deadline. It never resends the prompt. The additive
`submit_key_recovery` projection reports eligibility, attempt count, and
result; a recovered checkpoint uses
`delivery.transport_state: submit-key-recovery-succeeded`. Existing/replayed
sessions, Hermes, stopped or replaced sessions, and any second recovery
keypress are ineligible. A final timeout reports `delivery.state: unverified`,
`automatic_retry_safe: false`, and explicitly forbids duplicate prompt or
further Enter injection. A successful terminal submit command alone is not
provider acceptance. The wait takes no registry lock, so it never blocks the
worker's own checkpoint; `--await-ready 0` preserves the launch-only
`pending-worker-checkpoint` result. `worker retire ID` is the teardown macro: it
composes release -> delete and reports the worker's absence in one call,
replacing the hand-run
release -> delete -> confirm sequence. An accepted assignment is released first;
an already-terminal one goes straight to delete. Per-step idempotency keys are
derived from the retire key so a retry converges through each step's receipt.

The generated worker prompt starts with one deterministic,
worker-authenticated `main-agent bootstrap` command. The prompt names the exact
running `main-agent` executable rather than relying on the worker's `PATH`, so
bootstrap uses the same release that created the assignment. Bootstrap resolves
only the caller's bound assignment, reads that worker's private assignment
packet, derives the coordination claim from the packet's
repository/worktree/scopes, and records the initial `working` checkpoint. Its
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
state. Accept/release are explicit Main Agent transitions. The ordinary public
V1 terminal path is `submitted -> accepted -> released`. `cancelled` is a
reserved terminal state retained for compatible registry reads and a possible
future transition; V1 exposes no `worker cancel` command or other public
transition into it. Operators MUST NOT synthesize cancellation by editing the
private registry.

Collaborators and bounded borrowers are visible routing metadata, not write
authority. Borrow expiry does not change primary ownership. Handoff requires a
live target Main Agent and a quiescent current manager; adoption requires the
old manager reference to be stale. Delete requires a released/cancelled
assignment, released worker claim, no active or uncertain operation, and the
producer's exact logical-delete/tombstone checks. Physical cleanup failure is a
maintenance record and cannot restore the live session projection.

### Daemon-owned group cleanup

The bearer-protected serve route
`GET /sessions/{id}/orchestration/group-cleanup` previews deletion of one exact
Main Agent group. Its `agent-session.main-agent-group-cleanup.v1` response
contains the exact Main Agent reference, active run ID and revision, a sorted
list of assignments still primarily managed by that Main Agent, whether each
requires force, and a digest over the complete plan. Collaborators, borrowers,
workers handed off to another primary manager, and assignments from another
run are outside the plan.

`POST` accepts only
`agent-session.main-agent-group-cleanup-request.v1` with the previewed Main
Agent incarnation, run revision, plan digest, `safe` or `force` mode, and an
idempotency key. Any identity, revision, or plan drift fails before cleanup.
Safe mode rejects a plan containing nonterminal assignments. Force mode
terminalizes exactly those assignments as `cancelled`; accepted assignments
advance to `released`.

Execution is deliberately ordered: delete or confirm absence of every planned
worker, close the run, then delete the Main Agent. Worker identity is checked
again before deletion. A worker failure returns a typed
`agent-session.main-agent-group-cleanup-result.v1` partial result and preserves
both the active run and Main Agent. A failure after worker deletion but before
or during Main deletion still reports `main_deleted: false`; clients reconcile
the per-worker outcomes and keep the Main Agent available for recovery. Exact
idempotent replay returns the original result.

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
