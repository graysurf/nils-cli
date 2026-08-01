# Session maintenance v1

## Purpose

`agent-session serve` owns the diagnosis and safe repair of lifecycle failures.
Consumers render this contract and request one advertised action; they do not
infer process ownership, signal processes, or delete retained session metadata.

The contract is additive. An older daemon returns 404 for these routes, so a
consumer must retain its existing Resume, Delete, and Attach behavior.

This contract is frozen against new action IDs. A v1 consumer rejects an
unrecognized action rather than ignoring it, so an action added here would break
existing clients instead of degrading for them. New recovery actions live in
[Session maintenance v2](session-maintenance-v2.md), which a consumer opts into
with an explicit `schema_version`. A v2-capable daemon still returns exactly the
payloads described below to any consumer that does not ask for the successor.

## Authentication and endpoints

Both endpoints require the daemon Bearer token:

- `GET /sessions/{id}/maintenance?operation=resume|delete|attach|inspect`
- `POST /sessions/{id}/maintenance/actions`

The GET response is wrapped in `cli.agent-session.serve.v1` at
`data.maintenance`. The POST response uses `data.maintenance_result`. The
nested object in either response uses
`schema_version: agent-session.session-maintenance.v1`.

## Preview

A preview contains only an allowlisted projection:

```json
{
  "schema_version": "agent-session.session-maintenance.v1",
  "session_id": "20260716-120000-codex",
  "operation": "resume",
  "state": "repairable",
  "session_incarnation": "opaque-runtime-id",
  "session_generation": 1,
  "issue": {
    "kind": "process_boundary_live",
    "operation": "resume",
    "retryable": true,
    "preserves_session_metadata": true,
    "message": "the recorded runtime process boundary is still live"
  },
  "boundary": {
    "kind": "managed_scope",
    "safe_process_count": 2
  },
  "preservation": {
    "session_metadata_retained_until_success": true,
    "provider_conversation_retained": true,
    "destructive_scope": "verified_recorded_runtime_only"
  },
  "actions": [
    {
      "id": "retry_resume",
      "label": "Retry resume",
      "destructive": false,
      "requires_confirmation": false,
      "preserves_session_metadata": true
    },
    {
      "id": "terminate_runtime_then_resume",
      "label": "Terminate recorded runtime, then resume",
      "destructive": true,
      "requires_confirmation": true,
      "preserves_session_metadata": true
    }
  ],
  "preview_digest": "sha256:8d5dece08afdf22b39fcbba1d1bc33dd6d967e81a4728a715be06995d9db7f24"
}
```

Allowed `state` values are `healthy`, `repairable`, and `blocked`. Allowed
issue kinds are:

- `process_boundary_live`
- `runtime_identity_changed`
- `runtime_identity_unavailable`
- `session_still_running`
- `startup_failed`
- `unknown`

Allowed boundary kinds are `none`, `tmux_session`, `managed_scope`,
`process_group`, and `unknown`. `safe_process_count` is descriptive; it is not
a process identifier.

Only a live, identity-verified managed scope may produce a destructive runtime
termination action. A live process-group-only boundary is `blocked`, advertises
only the matching retry action, and is never signalled by the maintenance API.
`retry_delete` is also destructive because it may remove durable session
metadata, even though it never expands the safe process boundary.

The canonical producer fixture is
[`../tests/fixtures/maintenance/session-maintenance-v1-repairable.json`](../../tests/fixtures/maintenance/session-maintenance-v1-repairable.json).

## Actions and stale fencing

The action request is strict and rejects unknown fields:

```json
{
  "operation": "resume",
  "action": "terminate_runtime_then_resume",
  "expected_session_incarnation": "opaque-runtime-id",
  "expected_session_generation": 1,
  "expected_preview_digest": "sha256:8d5dece08afdf22b39fcbba1d1bc33dd6d967e81a4728a715be06995d9db7f24",
  "confirmed": true
}
```

Allowed actions are `retry_resume`, `retry_delete`, `retry_attach`, `inspect`,
`terminate_runtime_then_resume`, and `terminate_runtime_then_delete`. An action
must match its operation. Every destructive action must have been advertised by
the same preview and the request must send `"confirmed": true`; the producer
rejects an omitted or false confirmation before mutation.

The daemon serializes the action with session-record lifecycle operations,
reloads the record, and recomputes all three preconditions. Any incarnation,
generation, digest, boundary, or action-availability change returns HTTP 409
with `maintenance-preview-stale` before mutation. Consumers must fetch a new
preview and must not automatically repeat a destructive action.

HTTP maintenance lock admission is bounded. If another lifecycle mutation owns
the session record for longer than the admission budget, the daemon returns
HTTP 409 with `maintenance-session-busy` and `retryable: true`; requests do not
queue indefinitely behind one session.

Successful actions return a safe result:

```json
{
  "schema_version": "agent-session.session-maintenance.v1",
  "session_id": "20260716-120000-codex",
  "operation": "resume",
  "action": "terminate_runtime_then_resume",
  "outcome": "resumed",
  "session_incarnation": "new-opaque-runtime-id",
  "session_generation": 2,
  "status": "running",
  "cleanup_pending": false
}
```

`outcome` is `resumed`, `deleted`, or `inspected`. Runtime identity fields are a
pair: they are either both populated or both `null`; mixed nullability is
invalid. A successful `resumed` result normally carries the new incarnation and
generation, but an already-running record that predates runtime metadata may
validly return both fields as `null`. A successful delete always encodes both
identity fields as `null` and reports `status: deleted`. Deletion atomically removes the complete session directory
from the live namespace before returning success. `cleanup_pending: true` means
the quarantined tombstone still needs producer-owned physical cleanup; the
daemon starts a bounded tombstone janitor in the background after its listener
has bound. The session is already logically deleted and a same-id replacement
is never touched by that cleanup. A symlinked or non-directory quarantine root
fails closed before live metadata moves.

## Failure and privacy contract

An unsafe or failed maintenance action retains metadata and returns a typed,
safe error. `session-maintenance-failed` details contain only session id,
operation, one allowed issue kind, retryability, and the preservation flag.

Preview, result, and error payloads must never contain argv, environment,
prompt or response text, terminal output, transcript data, provider content,
credentials or tokens, raw filesystem paths, tmux names/ids, process ids, or
cgroup paths. `preview_digest` is a domain-separated opaque SHA-256 digest over
the current producer-owned maintenance facts; consumers compare it only for
equality.
