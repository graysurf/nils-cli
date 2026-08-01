# Session maintenance v2

## Purpose

`agent-session.session-maintenance.v2` is the successor to
[Session maintenance v1](session-maintenance-v1.md). It exists for one reason:
v1 cannot safely gain an action ID.

A v1 consumer normalizes the advertised action set against a closed enum and
rejects an unrecognized action instead of ignoring it. Adding an action to a v1
payload would therefore break existing clients rather than degrade for them. v2
carries the new action, and v1 keeps its exact published shape.

Everything in the v1 spec still applies unless this document overrides it:
allowlisted projections, stale fencing, bounded lock admission, the privacy
contract, and producer ownership of every runtime signal.

## Negotiation

The contract is chosen by the consumer and echoed by the producer:

- `GET /sessions/{id}/maintenance?operation=delete&schema_version=agent-session.session-maintenance.v2`
- `POST /sessions/{id}/maintenance/actions` with
  `"schema_version": "agent-session.session-maintenance.v2"` in the body.

Omitting `schema_version` means v1. An existing consumer that never sends the
field keeps receiving a valid v1 preview from a v2-capable daemon, and never
sees a v2-only action.

An unrecognized `schema_version` fails closed: the GET returns
`invalid-maintenance-query` and the POST returns
`invalid-maintenance-action-request`. The producer never silently downgrades a
version it does not understand.

The nested `schema_version` in `data.maintenance` and `data.maintenance_result`
is always the negotiated contract, so a consumer can assert that it got the
contract it asked for.

## Added action: `remove_console_record`

v2 adds exactly one action, available only for `operation: delete`:

```json
{
  "id": "remove_console_record",
  "label": "Remove from Console only",
  "destructive": true,
  "requires_confirmation": true,
  "preserves_session_metadata": false,
  "terminates_runtime": false,
  "may_leave_runtime_running": true
}
```

`terminates_runtime` and `may_leave_runtime_running` appear **only** on this
action. Their absence on any other action means the v1 default: the action
treats the recorded runtime the way v1 already documented. This is what keeps
every v1 action payload byte-identical.

### What it does

It performs the same atomic logical deletion that ordinary deletion performs —
the session directory is renamed into the producer-owned quarantine, and the
existing tombstone janitor finishes physical cleanup — and **sends no signal of
any kind**. No `kill`, no process group, no `tmux kill-session`.

The result never reports `deleted`:

```json
{
  "schema_version": "agent-session.session-maintenance.v2",
  "session_id": "20260716-120000-codex",
  "operation": "delete",
  "action": "remove_console_record",
  "outcome": "record_removed",
  "session_incarnation": null,
  "session_generation": null,
  "status": "record_removed",
  "cleanup_pending": false
}
```

`outcome` and `status` are `record_removed` precisely because nothing was
stopped. A consumer must not present this as a terminated runtime.

### When it is advertised

Both conditions must hold, and both are re-derived under the session-record
lifecycle lock immediately before the mutation:

1. The evidence class has no safe signal boundary — the recorded runtime
   identity is unavailable, or a recorded boundary is live but its ownership
   cannot be verified. A **changed** runtime identity does not qualify: that
   keeps blocking every destructive action.
2. The exact managed tmux target is proven **absent** by the daemon's own
   exact-target probe. An unverifiable tmux status does not prove absence and
   does not qualify.

This is the retry-only dead end the action exists for: with no tmux session and
an unverifiable process boundary, ordinary deletion and `retry_delete` both fail
closed forever, because no retry can manufacture the missing proof that the
remaining boundary still belongs to the recorded runtime.

If either condition stops holding between preview and action, the request
returns `maintenance-preview-stale` and nothing moves.

### Consumer obligations

The confirmation must state, before the record is removed, that:

- an unverified process may still be running, and
- removing the record ends normal Console management of it.

The label is **Remove from Console only**. Do not present it as "force kill" or
"force delete": it stops nothing, and that wording would misrepresent the
guarantee.

## Fencing

`preview_digest` is domain-separated by the negotiated contract, so a v1 digest
can never authorize a v2-only action and a v2 digest is not accepted for a v1
preview. A v2 digest additionally binds whether record-only removal was
available, so a preview taken while nothing was reachable cannot be replayed
once a boundary reappears.

Every v1 fence still applies to the new action: session incarnation, generation,
preview digest, lifecycle lock, orchestration assignment admission, exact record
path validation, same-id reuse, and quarantine-root safety are all revalidated
before the record moves.

## Actions not added

`adopt_exact_tmux_then_delete` is deliberately **not** part of this contract.
Ordinary deletion already adopts an exact, environment-verified live tmux
identity when no identity was persisted: `terminate_tmux_session_with_timeouts`
captures the identity, validates `AGENT_SESSION_ID`, `AGENT_SESSION_STATE_DIR`,
and `AGENT_SESSION_RUNTIME_ID` against the record, persists it, and then
terminates that exact runtime. A separate action would duplicate that path and
add a second signalling surface without closing a reachable gap.

## Compatibility matrix

| Producer | Consumer | Result |
| --- | --- | --- |
| v1-only daemon | v1 consumer | unchanged v1 behavior |
| v1-only daemon | v2 consumer | `schema_version` is an unknown field on both the query and the body, so the daemon rejects it with `invalid-maintenance-query` / `invalid-maintenance-action-request`. A v2 consumer must treat either code as "producer is v1-only" and retry without the field. |
| v2 daemon | v1 consumer | valid v1 preview; no v2-only action is ever advertised |
| v2 daemon | v2 consumer | v2 preview, record-only removal advertised when justified |
| any | unknown version | fails closed |
