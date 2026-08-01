# agent-session Documentation

Use the crate [README](../README.md) for the non-normative product overview,
common commands, and document routing. Use the documents below when operating
or integrating a specific subsystem.

## Operator runbooks

- [Work coordination](runbooks/work-coordination.md): coordination modes,
  declared paths, authority boundaries, advisory flow, and enforce flow.
- [Main Agent orchestration](runbooks/main-agent-orchestration.md): complete
  operator lifecycle, input packets, retry fences, interactive worker
  acceptance, relationship transfer, recovery, and cleanup.
- [Serve daemon operations](runbooks/serve-daemon.md): safe startup,
  authentication boundaries, HTTP session creation, and restart survival.

## Stable contracts

- [Serve API v1](specs/serve-api-v1.md): HTTP and WebSocket endpoints,
  response/authentication rules, launch profiles, and session survival.
- [Session coordination v1](specs/session-coordination-v1.md): normative
  schemas, state machines, authorization, routes, limits, and failure codes.
- [Turn-state contract](turn-state-contract.md): runtime-bound activity state,
  privacy projection, replay, and provider setup behavior.
- [Activity stream v1](specs/activity-stream-v1.md): SSE stream, replay,
  reset, flow control, and privacy contract.
- [Session maintenance v1](specs/session-maintenance-v1.md): repair and
  maintenance operation contract.
- [Session maintenance v2](specs/session-maintenance-v2.md): successor contract
  adding record-only removal for a runtime with no safe signal boundary.
- [Main Agent orchestration v1](specs/main-agent-orchestration-v1.md): durable
  run/assignment schemas, authenticated facade, rehydration, and relationship
  lifecycle.

## Evidence and migration reports

- [Provider turn-signal evidence](provider-turn-signal-evidence.md): provider
  versions and lifecycle-signal evidence behind the turn-state integration.
- [Completion migration contract](reports/agent-session-completion-migration-contract.md):
  clap-first completion coverage and verification record.
