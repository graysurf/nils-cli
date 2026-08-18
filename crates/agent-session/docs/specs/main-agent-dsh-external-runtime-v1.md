# Main Agent DSH external-runtime provider (v1)

This spec defines how the `main-agent` orchestration facade supports
`launch.agent = "dsh"` workers whose runtime is owned by an external harness
(the DeepSeek Harness `dsh-runtime-kit` bundle) instead of a tmux-managed
provider CLI session. The durable store, revision fencing, idempotency
receipts, claims, and acceptance protocol are unchanged; only the transport
and liveness evidence differ.

Tracking: sympoies/dsh-runtime-kit#6 (M1). The consuming plugin work is M2 in
that issue.

## Division of responsibility

| Concern | Owner |
|---|---|
| Run/assignment/checkpoint records, revision fencing, idempotency | `main-agent` (this crate, unchanged) |
| Worker session record + capability/checkpoint file minting | `main-agent worker start` (dsh arm) |
| Spawning the worker agent, delivering the bootstrap prompt | dsh-runtime-kit plugin |
| Per-lane broker heartbeat process | dsh-runtime-kit plugin (spawns `agent-session broker heartbeat`) |
| Liveness/turn evidence | plugin-maintained sidecar file (schema below) |
| Worker-side bootstrap/checkpoint | worker agent runs the existing `main-agent bootstrap` / `main-agent checkpoint` verbs |
| Stop/interrupt of a lane runtime | plugin (`worker stop-runtime` returns a typed refusal for dsh) |

## `capabilities --provider dsh`

`main-agent capabilities --provider dsh --format json` returns
`main-agent.capabilities.v1` with:

- `capabilities.runtime_checkpoint_file`: `"main-agent.runtime-checkpoint-file.v1"`.
- `capabilities.runtime_hook_checkpoint_write`: always `null`. DSH policy v1
  admits only native allow/block decisions; there is no hook checkpoint-write
  admission layer, and none is required — the store fences every checkpoint.
- `capabilities.run_wide_closeout`: `"main-agent.run-wide-closeout.v1"`.
- `capabilities.external_runtime`: `"main-agent.external-runtime.v1"` — the
  worker transport contract in this spec.
- `compatible`: true iff the same-directory sibling `agent-hook` reports a
  doctor record for product `dsh` with `dispatch_supported: true` and
  `registration_owner: "dsh-runtime-kit"`. For codex/claude the existing
  predicate (hook checkpoint-write admission) is unchanged.

## `worker start` external arm

For `launch.agent = "dsh"` the shared path (validation, claims, digests,
replay, assignment record, packet storage, fences) is unchanged. At the
launch boundary the arm diverges:

- The session record is created through the normal record path with
  `runtime.kind = "dsh_external"`, an empty `tmux_session`, and a CLI-minted
  `launch_id` (the worker incarnation). No process is spawned and no prompt
  is pasted.
- `--await-ready` must be `0s`; a non-zero value is `invalid-input`
  (`dsh-await-ready-unsupported`). Readiness is observed with
  `worker wait`/`worker show` after the plugin delivers the prompt; the
  authenticated-checkpoint readiness proof is store-only and unchanged.
- Provider-stop canary, account handoff, and submit-key recovery remain
  codex/claude-only; the existing gates already exclude dsh.
- The result stays `main-agent.worker-start-result.v1` and additionally
  carries (dsh only):

```jsonc
"delivery": { "state": "external-launch-pending", "proof": "external-runtime-transfer" },
"external_launch": {
  "schema_version": "main-agent.external-launch.v1",
  "launch_id": "<runtime.launch_id>",
  "prompt": "<byte-stable dsh bootstrap prompt; contains no environment values>",
  "worker_env": {
    "AGENT_SESSION_ID": "<worker session id>",
    "AGENT_SESSION_STATE_DIR": "<state dir>",
    "AGENT_SESSION_RUNTIME_ID": "<launch_id>",
    "AGENT_SESSION_CAPABILITY_FILE": "<capability path for (session, launch_id)>",
    "AGENT_SESSION_CHECKPOINT_FILE": "<checkpoint path for (session, launch_id)>"
  },
  "broker_heartbeat_argv": ["<agent-session bin>", "--state-dir", "<state dir>", "broker", "heartbeat", "--session", "<id>", "--incarnation", "<launch_id>", "--generation", "1", "--capability-file", "<capability path>", "--format", "json"],
  "broker_stop_argv": ["<agent-session bin>", "--state-dir", "<state dir>", "broker", "stop", "--session", "<id>", "--capability-file", "<capability path>", "--format", "json"],
  "liveness_file": "<session dir>/dsh-runtime-liveness.json"
}
```

The plugin must deliver `prompt` verbatim as the worker's initial message,
run `broker_heartbeat_argv` as a supervised background process for the lane's
lifetime (worker authentication reads the capability file that the heartbeat
maintains), run `broker_stop_argv` at lane end, and arrange the worker's
`main-agent`/`agent-session` invocations to carry `worker_env`. The prompt is
deliberately environment-free so it stays a byte-stable replay contract; env
delivery is a plugin responsibility (per-child instruction context or a
wrapping native tool).

The dsh prompt is a fifth byte-stable replay variant alongside the four
existing prompts; `ensure_worker_launch_matches` accepts it for replay
matching exactly like the others.

## Liveness sidecar — `main-agent.dsh-runtime-liveness.v1`

Path: `<session dir>/dsh-runtime-liveness.json`, written and refreshed only by
the plugin (owner-only file mode). Fields:

```jsonc
{
  "schema_version": "main-agent.dsh-runtime-liveness.v1",
  "launch_id": "<must equal runtime.launch_id>",
  "harness": {
    // process identity of the DSH harness process, TmuxRuntimeIdentity-shaped:
    "pane_pid": <harness pid>,
    "process_group_id": <pgid or null>,
    "process_session_id": <sid or null>,
    "control_group": "<cgroup path or null>",
    "pid_namespace": "<pid ns identity or null>",
    "pane_start_time": <starttime ticks or null>
  },
  "lane": { "state": "open" | "terminated" },
  "updated_at": "<epoch seconds>"
}
```

Consumption:

- `session_status` for a `dsh_external` record: `missing` when the sidecar is
  absent; `stopped` when `lane.state == "terminated"`, when `launch_id` does
  not match the record, or when the harness process identity proves stopped;
  otherwise `running`. An idle-but-resumable lane is *running* — cold resume
  is always available while the harness lives and the lane is open.
- `coordination_runtime_evidence` for a `dsh_external` record builds the
  runtime identity from `harness` (validated against `launch_id`) and reuses
  the existing process-liveness proof. Unknown stays conservative exactly as
  for tmux runtimes.

## Diagnose/supervise evidence

Store, claim, packet, worktree, and coordination evidence are provider-neutral
and unchanged. For `dsh` workers the activity evidence is derived from the
sidecar (`lane.state` + harness liveness) instead of the provider hook spool:
an open lane with a live harness reports a waiting/working turn phase at
observed confidence, so a healthy dsh lane classifies on the same table as the
managed products without the provider-hook activity pipeline. The
classification table itself is not modified.

## Typed refusals

- `worker stop-runtime`, `worker stop-claimed-runtime`: `dsh-runtime-plugin-owned`
  — the plugin interrupts the lane; `worker reconcile-stopped` then proceeds
  store-side unchanged once the sidecar proves the lane stopped.
- `agent-session resume`/provider resume surfaces: dsh sessions are not
  CLI-resumable (same typed-refusal pattern as Hermes).

## Non-goals (v1)

- No provider hook activity pipeline for dsh (`agent-session activity` events
  may arrive later via the agent-hook DSH ingress work; this spec does not
  depend on it).
- No `AssignmentInput` schema change (worker-start digests must stay stable).
- No provider-stop canary, account handoff, or submit-key recovery for dsh.
