# macos-agent journal v2

## Purpose

Journal v2 records enough structural evidence to reproduce and review bounded
desktop automation without turning raw desktop content into a durable log.
Redaction happens before persistence. A redaction, integrity, or publication
failure is a hard journal error.

## Files

| File | Contract |
| --- | --- |
| `manifest.json` | Run identity, adapter/Peekaboo provenance, backend digest, effective runtime, transport, evidence mode, optional tool profile, lifecycle state. |
| `steps.jsonl` | One append-only, synced `macos-agent.journal-step.v2` object per line. |
| `artifacts/index.json` | Allowlist of transferred/published artifacts with digest, MIME, sensitivity, redaction, retention, producing step, and confined relative path. |
| `summary.json` | Deterministic counts, normalized failure signatures, replay/defect candidates, assertions, and interrupted-tail state. |
| `redaction.json` | Applied rule families and suppression counters; it never contains suppressed values. |
| `review.json` | Failure clusters, significance, proposed owner, and step IDs. |

The private `.sequence` counter is internal append state. It is reconstructed
from validated steps on open and is never published or transferred as evidence.

All files and directories are user-private. Paths are relative and may not
contain absolute, parent-traversal, prefix, or symlink escapes.

## Step contract

Each step includes a monotonically increasing sequence, run-scoped correlation
ID, optional parent, timestamp, sanitized intent and expected postcondition,
command and argv shape, backend digest, runtime, transport, duration, retry
count, status, normalized failure class, pre/postcondition references, optional
snapshot lineage, replay class, and indexed artifact references. MCP steps also
retain bounded structural `mcp_method` and optional `mcp_tool` identifiers; raw
protocol envelopes and payloads remain excluded.

Statuses are `passed`, `failed`, `unknown`, and `policy_blocked`. A mutating
timeout or signal termination is always `unknown`; callers must inspect current
state and may not blindly retry it.

## Evidence modes

- `minimal` retains structural metadata and redacts home/user path shapes and
  credential patterns.
- `debug` additionally permits sanitized result artifacts registered in the
  artifact index.
- `sensitive` suppresses all string payloads. Typed text, clipboard data,
  titles, values, host/user identities, SSH material, provider errors, and raw
  requests/results are not retained.

Suppressed values are represented only by the constant `<suppressed>` marker.
No exact length, deterministic digest, or cross-run correlation token is
retained.

## Append recovery

Earlier newline-complete records remain authoritative after interruption. Only
an incomplete final line is copied into a private `quarantine/` file, removed
from the active `steps.jsonl`, and reported through
`summary.recovered_tail`. A newline-complete malformed record, whether in the
middle or at the end, fails closed and is never truncated automatically.
Sequence allocation uses constant-size private state; appending does not rescan
or rewrite prior journal history. A private shared transaction marker brackets
append and sequence-state commit. After interruption or commit failure, the
next writer—including one opened before the failure—reconstructs the counter
from validated steps under the journal lock before appending. Summary
generation validates the full log at session close or on an explicit
summarize/review operation.

## Remote transfer merge

A remote session journal is per-call scratch. The endpoint allocates a fresh
session root for every request, so a transferred `steps.jsonl` always restarts
at sequence 1 with its own `run_id`. The controller therefore merges a transfer
into the durable journal in `--out-dir` instead of installing it: artifact
payloads are written directly, while `manifest.json`, `steps.jsonl`,
`summary.json`, `redaction.json`, `review.json`, and `artifacts/index.json` are
verified, parsed, and merged under the journal lock.

Merging reassigns each transferred step a local sequence, id, and correlation
id, remaps a parent named inside the same transfer, rewrites the producing step
of each merged artifact row, and rebuilds the summary and redaction report from
the merged log. The local manifest keeps its own `run_id` and `started_at`; a
transfer may only continue a session whose `schema_version`, `backend_digest`,
`runtime`, `transport`, `evidence_mode`, and `tool_profile` are unchanged, and
a drifted tuple fails closed without losing the steps already recorded. The
512-step rotation bound applies to the merged log.

## Replay

- `safe`: read-only observation/setup with retained sanitized argv.
- `conditional`: bounded mutation with explicit pre-action snapshot lineage;
  requires `--confirm-conditional`, a matching `--current-snapshot`, and a
  fresh caller-supplied `--expected` observable postcondition.
- `never`: secrets, typed/pasted values, policy blocks, unknown mutations,
  destructive/external actions, or any step without reconstructable argv.

`replay-plan` does not execute. `replay-step` requires the effective verified
backend digest to match the manifest, recomputes command, argv shape, replay
class, and normal policy from retained argv, reruns backend/runtime checks under
one shared backend lifecycle lease, and appends a child step. Stale snapshots,
mismatched state, changed backends, tampered derived metadata, unguarded
mutations, and missing fresh postconditions are refused. The fresh postcondition
is suppressed on persistence like every other expected value. A journal whose
manifest transport is not `local` is never eligible for local replay: the
current interface cannot supply and re-verify a fresh explicit remote target,
so both planning and execution fail closed.

## Review and ownership

Repeated normalized failures and mandatory significant classes become review
candidates. Significant classes include privacy/redaction, wrong target, false
success, unknown mutation, refused mutation, held input, remote cleanup,
journal/replay integrity, backend drift, and permission drift. Proposed
ownership is one of adapter, Peekaboo/adapter, runtime skill policy, or the TCC
environment. Review does not create a provider issue by itself.

When the upstream result publishes a v4 outcome envelope, classification reads
it rather than inferring from the process exit code alone. A refusal becomes
`upstream_refused` and is always significant; an upstream that reports its own
failure at exit zero becomes `false_success`; and an envelope proving no
mutation dispatched resolves an otherwise unknown mutation. An upstream result
without an envelope keeps the exit-code rules unchanged.

Raw upstream payloads and screenshots are never promoted automatically.
