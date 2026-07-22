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
main-agent worker start --assignment-file FILE --if-run-revision N --idempotency-key KEY --format json
main-agent worker list|show ...
main-agent worker message|accept|release|delete ...
main-agent collaborate|borrow|handoff|adopt ...
main-agent close --if-revision N --idempotency-key KEY --format json
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
the worker's authenticated `self show`, revision-fenced checkpoint, and the
task-specific review evidence before `worker accept`.

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
