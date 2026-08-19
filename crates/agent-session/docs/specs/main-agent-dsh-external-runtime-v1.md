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
| --- | --- |
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
the plugin. The reader requires an owner-only (`0600`-class), single-link,
non-symlink regular file at exactly that path, at most 64 KiB, and rejects
unknown fields at every level. Fields:

```jsonc
{
  "schema_version": "main-agent.dsh-runtime-liveness.v1",
  "launch_id": "<must equal runtime.launch_id>",
  "harness": {
    "pid": <harness pid>,
    // Linux /proc starttime ticks. Required to prove the exact incarnation:
    // without it liveness is undecidable and destructive operations refuse.
    "start_time": <starttime ticks, optional>
  },
  "lane": { "state": "open" | "terminated" },
  // Optional turn evidence; absent means diagnosis degrades conservatively.
  "turn": {
    "phase": "working" | "waiting",
    "phase_changed_at": "<epoch seconds>",
    "current_turn": { "started_at": "<epoch seconds>", "last_progress_at": "<epoch seconds, optional>" },
    "last_turn": { "completed_at": "<epoch seconds>", "outcome": "completed" | "failed" | "interrupted" }
  },
  "updated_at": "<epoch seconds>"
}
```

Consumption. The reader resolves one of four dispositions:

| Disposition | When | `session_status` |
| --- | --- | --- |
| never attached | no sidecar exists | `missing` |
| running | valid sidecar, lane `open`, pinned harness proven live | `running` |
| proven stopped | lane `terminated` with the harness not proven live, or the pinned harness proven gone (ESRCH, or a starttime mismatch proving pid reuse) | `stopped` |
| unproven | sidecar unreadable, malformed, oversized, wrongly owned, or launch-mismatched; or the harness state is undecidable (see the platform rule below) | `unknown` |

Platform rule for the incarnation pin: where a starttime source exists (Linux
`/proc`), a liveness claim without a verified `start_time` match is *undecided*
rather than live, so a recycled pid can never masquerade as a live harness —
including when `kill` reports `EPERM`. On platforms with no starttime source the
pid signal is the only liveness evidence that exists, so it is decisive there
and pid reuse remains an accepted residual risk, exactly as it is for the tmux
process-group probe on the same platforms. Plugins should always write
`start_time` where the platform can read it.

- Destructive operations require positive evidence: record deletion is
  admitted only for a never-attached or proven-stopped lane. A running lane
  refuses, and an unproven lane refuses with
  `coordination-runtime-unverified` — a corrupted sidecar must never authorize
  destroying a live lane's record.
- An idle-but-resumable lane is *running*: cold resume is always available
  while the harness lives and the lane is open.
- `lane.state == "terminated"` is a **plugin assertion**, deliberately weaker
  than the tmux kernel-backed proof: the sidecar lives in the same-uid state
  dir a lane's own worker can reach. It is therefore corroborated against the
  pinned harness identity, and a still-live harness makes the assertion
  unproven rather than proven.
- `coordination_runtime_evidence` builds the runtime identity from `harness`
  (validated against `launch_id`) and reuses the existing process-liveness
  proof. Unproven stays conservative exactly as for tmux runtimes.

## Diagnose/supervise evidence

Store, claim, packet, worktree, and coordination evidence are provider-neutral
and unchanged. For `dsh` workers the activity evidence is derived from the
sidecar instead of the provider hook spool: a valid open lane under a live
harness whose sidecar carries a known `turn.phase` reports that phase with
`source.kind: "runtime"`, `provider: "dsh"`, and authoritative confidence
(the harness observes its own agent directly, so the signal is authoritative
rather than inferred). Any other state yields no activity evidence, and
diagnosis degrades conservatively. The classification table itself is not
modified.

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
