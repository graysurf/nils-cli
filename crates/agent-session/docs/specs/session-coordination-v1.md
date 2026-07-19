# Session Coordination V1

## Status and ownership

- Status: implementation contract for `agent-session` coordination.
- Schema family: `agent-session.coordination.v1`.
- Owner: `nils-agent-session`.
- Compatibility: additive to `agent-session.session.v1`; clients that do not
  use coordination retain the current start, run, resume, list, glance, send,
  delete, activity, and serve contracts.

This specification defines privacy-preserving coordination for managed agent
sessions. Work context is explicit metadata. It is never inferred as
authoritative from a title, working directory, prompt, transcript, log, glance,
or assistant response. A mailbox is available only when metadata cannot resolve
a material uncertainty. Formal delegated implementation still uses the
provider-backed dispatch workflow.

## Threat and trust model

- Session IDs, incarnation IDs, claim IDs, message IDs, revisions, and public
  work-context fields are selectors and fences. They are not credentials.
- Every owner, sender, recipient, lease, and mailbox mutation is authenticated
  by a private per-incarnation capability created by the session broker.
- A capability is projected to the managed process through a 0600 file. CLI
  examples use `--capability-file`; the managed runtime may instead provide the
  trusted `AGENT_SESSION_CAPABILITY_FILE` path. Secrets are never accepted as
  public identifiers or emitted in argv, JSON, errors, logs, or provider data.
- Peer summaries and mailbox bodies are authenticated as peer-supplied data but
  remain untrusted. They cannot authorize commands, approvals, scope changes,
  credential access, or secret disclosure.
- The optional HTTP server has separate operator authentication. Knowing its
  bearer token does not manufacture a session capability, and knowing a
  session capability does not grant server-operator authority.

## Storage and locking

The private coordination root is `<state-dir>/coordination`, mode 0700. Regular
files containing coordination state or credentials are mode 0600. The root must
be owned by the current user, must not be a symlink, and must remain canonically
below the selected state directory. An untrusted owner, symlink escape, or
unrepairable permission drift fails before mutation.

One bounded registry lock serializes claim evaluation, claim acquisition,
operation leases, mailbox transitions, notification receipts, expiry, and
cleanup. Default lock timeout is 2 seconds. No command waits indefinitely.
Writes use atomic replace and fsync ordering suitable for crash recovery.

The private store may contain bodies and capability digests. Public projections
are separately constructed and never serialize those fields.

## Versioned schemas

### Work context

Public claims use `agent-session.work-context.v1`:

```json
{
  "schema_version": "agent-session.work-context.v1",
  "session_id": "managed-session",
  "session_incarnation": "runtime-launch-id",
  "claim_id": "uuid",
  "revision": 1,
  "state": "active",
  "intent": "implementation",
  "tier": "L2",
  "repositories": ["owner/repository"],
  "worktrees": ["hmac-sha256:epoch:digest"],
  "provider_refs": [
    {"kind": "issue", "repository": "owner/repository", "number": 123}
  ],
  "plan_refs": ["docs/plans/2026-07-19-topic/topic-plan.md"],
  "scopes": [
    {"kind": "path-prefix", "repository": "owner/repository", "value": "src"}
  ],
  "summary": "Implement session coordination",
  "updated_at": "2030-01-01T00:00:00Z",
  "expires_at": "2030-01-01T00:30:00Z"
}
```

Input omits controller-owned fields: `session_id`, `session_incarnation`,
`claim_id`, `revision`, `state`, and timestamps. The broker binds those fields
to the authenticated live session. Unknown fields and unsupported schema
versions fail closed.

### Conflict result

Conflict evaluation uses `agent-session.conflict-evaluation.v1` and returns one
of `conflict`, `potential_conflict`, `unknown`, `no_known_conflict`, or `clear`.
Reasons and peers are stably sorted. Peer projections contain only public work
context and never machine-local paths, credentials, messages, or activity text.

Precedence is:

1. `conflict`
2. `potential_conflict`
3. `unknown`
4. `no_known_conflict`
5. `clear`

`clear` is valid only when the complete relevant live-session universe was
enumerated and every peer was comparable. `no_known_conflict` is the explicit
permissive projection for an incomplete comparison; it is never promoted to
`clear`.

### Operation lease

Mutation admission uses `agent-session.operation-lease.v1`. A lease includes a
random lease ID, owning claim and revision, operation kind, canonical target
set, controller-observed activity revision, exact persisted runtime identity
digest, state, revision, start/expiry timestamps, and an execution token digest.
Public views omit all private proof material.

### Message

Messages use `agent-session.message.v1`. Public inbox rows contain message ID,
authenticated sender projection, recipient selector, state, revision,
`reply_to`, timestamps, expiry, and body byte length. Only authenticated
recipient `message show` and `message wait` success results contain a `body`
field. The field is explicitly classified as `untrusted_peer_data`.

### Broker status

Broker projections use `agent-session.coordination-broker.v1` and expose only
state (`starting`, `ready`, `degraded`, `lost`, `stopped`), generation,
capability availability, heartbeat freshness, claim summary, and operation
summary. They never expose a PID as authority, a credential path, or a token.

## Scope grammar and canonicalization

Repositories are canonical lowercase `owner/name` values. Provider references
are `(kind, repository, numeric id)`. Plan references are normalized
repository-relative paths without `..`, absolute roots, NUL, or control bytes.

V1 scope kinds are closed:

| Kind | Value | Overlap rule |
| --- | --- | --- |
| `repository` | fixed value `.` | Conflicts with every scope in the same repository. |
| `path-exact` | normalized repo-relative file/path | Conflicts with the same exact path and a covering prefix. |
| `path-prefix` | normalized repo-relative path without a trailing `/` | Conflicts with equal, ancestor, or descendant prefixes and contained exact paths at `/` boundaries. |

Unknown kinds are rejected. Empty, absolute, host-qualified, home-relative,
symlink-escaped, and dot-segment path values are rejected. Canonicalization is
byte-stable across CLI and HTTP.

Worktree values are non-reversible HMAC-SHA256 fingerprints using a private
registry key and a public key epoch. Raw checkout paths never enter the
registry projection. An unknown epoch is incomparable rather than clear.

### Conflict truth table

| Candidate versus peer | Result |
| --- | --- |
| Same active worktree fingerprint | `conflict` |
| Same provider ref | `conflict` |
| Same plan ref | `conflict` |
| Same repository with overlapping closed scopes | `conflict` |
| Same repository with omitted, broad, or incomparable scopes | `potential_conflict` |
| Relevant live peer without valid/supported context | `unknown` |
| Complete relevant universe, all comparable and disjoint | `clear` |
| Incomplete universe with permissive projection requested and no known overlap | `no_known_conflict` |

The authenticated subject is excluded by exact session ID plus incarnation.
An explicit candidate is not removed merely because its fields resemble the
subject. Conflicting selectors or a missing subject for a self check fail.

## Relevant-peer universe

The authoritative claim transaction reads every non-expired managed session in
the selected registry snapshot. A peer is relevant when it is live or the
registry cannot safely establish terminality. Replaced, released, or expired
incarnations are retained for bounded audit but are not active conflicts.
Corrupt, oversize, future-schema, or partially upgraded peer records make the
view incomplete; they do not disappear from classification.

Standalone `check` is advisory. Only `claim` combines evaluation and acquisition
under the same registry lock. Two concurrent definite contenders cannot both
receive an admitted claim.

## Authentication and authorization matrix

| Operation | Required authority |
| --- | --- |
| work-context show/session check/candidate check | public registry read; HTTP additionally requires the server operator token |
| self check/claim/renew/release | matching session capability and incarnation |
| operation admit/complete/reconcile | matching session capability, active claim, and execution token/proof |
| message send | matching sender capability |
| inbox/show/ack/reply/wait | matching recipient capability |
| broker status | public registry read; HTTP additionally requires the server operator token |
| broker adopt/reconcile | local lifecycle lock plus proof selectors matching an unchanged, live, exact persisted runtime whose broker is demonstrably lost |
| HTTP registry-wide candidate check | server operator token; explicit subject/candidate rules still apply |

Capabilities rotate on resume/replacement and are revoked on delete/target exit.
Wrong principal, stale incarnation, wrong revision, wrong operation token, or
cross-principal idempotency reuse fails without revealing the expected value.
Recovery proof files contain only schema, incarnation, and generation selectors;
they never embed the server/operator token.

## Claim state machine

States are `active`, `stale`, `released`, and `expired`.

- `claim` validates the candidate, authenticates the subject, expires stale
  records, evaluates the complete snapshot, and creates one active 30-minute
  claim only when no `conflict` exists.
- `potential_conflict`, `unknown`, and `no_known_conflict` are returned as
  advisories in v1 and do not independently hard-block acquisition.
- `renew` requires claim ID, current revision, same incarnation, and a live
  broker. Heartbeat occurs before half the TTL.
- `release` is idempotent for the same principal/request and never affects a
  different incarnation. Release or replacement is rejected while a bound
  operation remains `active`, `completing`, or `reconcile_pending`.
- Broker loss marks the owner unavailable for new operations and eventually
  stales the claim. Pane liveness alone cannot renew a claim.

## Operation lease state machine

States are `active`, `completing`, `reconcile_pending`, `completed`, `failed`, and `abandoned`;
`expired` is accepted only as a retained backward-compatible terminal state.

- `admit` re-evaluates peers atomically and proves every canonical filesystem
  scope and provider reference is a subset of the authenticated active claim
  before creating a 30-minute lease. Filesystem targets bind each repository to
  a canonical checkout whose `origin` matches the declared repository.
- Opaque repository effects require an explicit repository scope. Symlink,
  multi-target, and normalized path checks apply to every target.
- A 30-minute claim does not release a known long operation. Reaching the
  operation safety TTL moves `active` to fail-closed `completing`; it never
  asserts terminality or removes the bound claim's exclusion.
- A matching working activity identity or exact live descendant renews the
  operation. Pane/process-group liveness by itself never renews it.
- `complete` is idempotent from `active`, `completing`, or
  `reconcile_pending` and records the terminal tool result without raw
  stdout/stderr.
- `reconcile` repairs a missed completion only when the token digest matches and
  controller-owned state proves the unchanged exact persisted runtime stopped,
  or when two quiescent activity/no-descendant observations at least five
  seconds apart move `reconcile_pending` to terminal. Caller-supplied idle or
  descendant booleans are not accepted as proof.
- Uncertain heartbeat or proof blocks later owner operations and competing
  admission until validated recovery; it does not silently expire an active
  mutation.

## Idempotency

Every mutation requires an idempotency key of 8 through 128 printable ASCII
bytes. Receipts bind principal, incarnation, operation, canonical request
digest, and outcome for 24 hours.

- Same key and same digest returns the original outcome.
- Same key with a different request under the same principal, incarnation, and
  operation returns `idempotency-key-reused` with no request content. The same
  raw key in another principal/incarnation/operation namespace is independent.
- Receipt cleanup is bounded and never removes a live claim, operation, or
  unread message needed to explain the retained outcome.

## Mailbox limits and state machine

Limits are normative:

- body: 16 KiB UTF-8 maximum;
- expiry: 24 hours default, 7 days maximum;
- per session: 256 messages and 4 MiB stored bytes;
- per registry: 64 MiB stored bytes;
- send rate: 30 messages per sender-recipient pair per minute, burst 10;
- inbox page: 50 default, 100 maximum;
- wait: 60 seconds maximum;
- reply depth: 16 maximum.

Message states are `unread`, `read`, `acknowledged`, `expired`, and `deleted`.
Send, ack, and reply are idempotent. Inbox ordering is `(created_at,
message_id)` and cursors are opaque, query-bound, principal-bound, and bounded.
Wait is cancellable, bounded, and returns on state/revision change
without busy looping. Cleanup never evicts live unread mail to admit new data;
quota exhaustion returns a typed error.

Self-recursive/cyclic reply chains, invalid UTF-8, controls forbidden by the
JSON contract, stale target incarnation, permission drift, corrupt state,
symlink escape, lock timeout, and quota/rate violations have distinct
content-free errors.

## Notification ownership

After a successful send, the coordination controller may attempt one optional
notification when the exact target incarnation is idle and supports the
structured prompt-v2 route. The bytes are generated solely from this template:

```text
Coordination message <message-id> is available; run agent-session message show --session <session-id> --message <message-id>.
```

The body, reply body, summary, title, prompt, or peer text is never interpolated.
Before any external submission, the controller persists a
`notification_attempting` receipt while holding the same session lifecycle
lock used for its final idle/incarnation recheck and prompt submission. No retry occurs after that transition,
including an unknown crash result. Limit is one attempt per target per minute.
Busy, rate-limited, replaced, unmanaged, unsupported, and failed targets remain
queue-only. Ack, reply, forwarding, and notifications do not recursively
notify.

## Managed launch and broker boundary

Start, run, resume, provider-import, and HTTP create follow one transaction:

1. reserve the session record and hold its lifecycle lock;
2. create the tmux pane in a held state that cannot exec the agent;
3. persist and read back the exact tmux/runtime identity;
4. start the runtime-owned heartbeat sidecar before the held gate;
5. create the private per-incarnation capability under the registry lock and
   wait at most 2 seconds for exact identity-bound readiness;
6. only then release the held pane to exec the agent.

Failure at any boundary revokes credentials, stops the broker, terminates only
the exact held runtime, and preserves bounded startup diagnostics. Launcher exit
does not stop an established broker. Resume creates a replacement incarnation
and capability. Broker loss blocks new coordination operations. `broker adopt`
requires an unchanged, live, exactly matched runtime and never trusts a PID or
pane name alone. Natural target exit immediately removes its
incarnation-specific capability; a replacement uses a different path, so a
stale runtime can never read the new credential. Delete also releases terminal
coordination state before session removal is reported complete.

The optional HTTP server is not the heartbeat owner and is not required for
coordination after launch.

## CLI contract

All commands support the global `--state-dir` and command-local `--format
text|json`. Owner commands accept `--session`; managed self calls may default it
from trusted runtime projection. `--capability-file` defaults only from the
trusted managed environment.

Every leaf command has its own CLI envelope identity, for example
`cli.agent-session.message-inbox.v1`, `cli.agent-session.broker-status.v1`, and
`cli.agent-session.work-context-admit.v1`.

```text
agent-session work-context claim --session ID --file JSON --capability-file FILE --idempotency-key KEY [--if-revision N]
agent-session work-context show --session ID
agent-session work-context check (--self --capability-file FILE | --session ID | --candidate JSON) [--allow-incomplete]
agent-session work-context renew --session ID --claim UUID --if-revision N --capability-file FILE --idempotency-key KEY
agent-session work-context release --session ID --claim UUID --if-revision N --capability-file FILE --idempotency-key KEY
agent-session work-context admit --session ID --claim UUID --if-revision N --targets-file JSON --operation KIND --execution-token-file FILE --capability-file FILE --idempotency-key KEY
agent-session work-context complete --session ID --lease UUID --if-revision N --execution-token-file FILE --outcome pass|fail --capability-file FILE --idempotency-key KEY
agent-session work-context reconcile --session ID --lease UUID --if-revision N --proof-file JSON --capability-file FILE --idempotency-key KEY

agent-session broker status --session ID [--capability-file FILE]
agent-session broker adopt --session ID --proof-file JSON --idempotency-key KEY
agent-session broker reconcile --session ID --proof-file JSON --idempotency-key KEY

agent-session message send --from ID --to ID --body-file FILE --capability-file FILE --idempotency-key KEY [--reply-to UUID] [--expires-in DURATION]
agent-session message inbox --session ID --capability-file FILE [--state unread] [--cursor CURSOR] [--limit N]
agent-session message show --session ID --message UUID --capability-file FILE
agent-session message ack --session ID --message UUID --if-revision N --capability-file FILE --idempotency-key KEY
agent-session message reply --session ID --message UUID --if-revision N --body-file FILE --capability-file FILE --idempotency-key KEY
agent-session message wait --session ID --message UUID --if-revision N --timeout DURATION --capability-file FILE
```

JSON uses the existing `cli.agent-session.<command>.v1` success/error envelope
convention. Errors never echo body, capability, request JSON, local private
paths, or peer summary.

## HTTP parity

The loopback server exposes the same library operations and schemas:

```text
GET  /sessions/{id}/work-context/v1
POST /sessions/{id}/work-context/check/v1
POST /coordination/work-context/check/v1
POST /sessions/{id}/work-context/claim/v1
POST /sessions/{id}/work-context/renew/v1
POST /sessions/{id}/work-context/release/v1
POST /sessions/{id}/work-context/admit/v1
POST /sessions/{id}/work-context/complete/v1
POST /sessions/{id}/work-context/reconcile/v1
GET  /sessions/{id}/broker/v1
POST /sessions/{id}/broker/adopt/v1
POST /sessions/{id}/broker/reconcile/v1
GET  /sessions/{id}/messages/v1
POST /sessions/{id}/messages/v1
GET  /sessions/{id}/messages/{message_id}/v1
POST /sessions/{id}/messages/{message_id}/ack/v1
POST /sessions/{id}/messages/{message_id}/reply/v1
GET  /sessions/{id}/messages/{message_id}/wait/v1
```

CLI and HTTP share one implementation, canonicalization, authorization,
idempotency, error codes, limits, and privacy projection. HTTP public reads
require only the server operator bearer; owner/mailbox mutations additionally
require the exact session capability. Conflicting selectors are rejected. Wait
cancellation closes without changing message state.

## Public list and glance additions

List and glance may add only:

- `work_context_state`;
- `claim_id`;
- `claim_expires_at`;
- `unread_message_count`;
- `coordination_conflict_severity`;
- `coordination_available`.

Existing fields, including `cwd`, do not change. New fields never embed
the full context, body, capability, incarnation, host/user, checkout path, or
private store location.

## Stable failure codes

The v1 surface distinguishes at least:

- `coordination-unavailable`, `coordination-broker-start-timeout`,
  `coordination-broker-lost`, `coordination-lock-timeout`;
- `coordination-unauthorized`, `session-incarnation-conflict`,
  `claim-revision-conflict`, `message-revision-conflict`;
- `unsupported-work-context-version`, `invalid-work-context`,
  `invalid-scope`, `uncovered-mutation-scope`, `incomplete-conflict-view`;
- `claim-conflict`, `idempotency-key-reused`, `operation-in-progress`,
  `operation-reconcile-pending`, `broker-replacement-grace`,
  `quota-exceeded`, `rate-limited`,
  `cursor-invalid`, `wait-timeout`, `wait-cancelled`;
- `mailbox-body-invalid`, `mailbox-body-too-large`, `reply-depth-exceeded`,
  `message-expired`, `message-not-found`;
- `coordination-store-untrusted`, `coordination-store-corrupt`.

Usage errors exit 64, data/contract errors use the workspace data exit code,
and runtime/storage failures use the runtime exit code.

## Validation matrix

Release readiness requires:

- table/property coverage for canonicalization, closed scopes, peer selection,
  conflict precedence, and keyed fingerprint epochs;
- concurrent process coverage proving exactly one definite claimant;
- capability, incarnation, revision, idempotency, and target-subset negatives;
- fake-clock/process coverage for long operations, broker loss/adoption,
  missed completion, replacement, and cleanup;
- CLI/HTTP parity for every operation, selector combination, error, wait, and
  cancellation outcome;
- mailbox permissions, limits, rate, pagination, retention, flood/restart, and
  privacy canaries;
- held-launch crash injection at record, pane, identity, credential, broker
  spawn/readiness, and exec boundaries;
- notification exact-byte golden, body non-interference, rate, busy, replaced,
  unsupported, failure, and crash windows;
- unchanged established lifecycle/list/send/server regression suites and completion
  freshness/parity checks.
