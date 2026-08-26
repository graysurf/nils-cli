# Finish-line Acceptance V1

## Purpose

The finish-line acceptance provider gives an authenticated DSH runtime one
durable, deterministic verdict for named completion requirements. It extends
the existing repository generation and contained Bash validation authority;
it does not grant a model a way to report success or replace CI.

The public RPCs are `register`, `admit`, `observe`, and `verdict`. Requests are
strict JSON on standard input and use the JSON-default finish-line output
format. All four require the private capability minted by `finish-line open`.
That bearer must remain inside the runtime provider and must never enter a
prompt, tool definition, log, session projection, or persistent configuration.

## Wire contracts

Every request is a duplicate-free JSON object with exactly these common
members: literal `schema_version`, `product = "dsh"`, bounded non-space ASCII
`session_id` and `turn_id`, absolute canonical Git-root `cwd`, and the private
`runner_capability`. The literal request schemas and command-specific members
are:

| Command | Schema | Additional members |
| --- | --- | --- |
| `register` | `agent-hook.finish-line.register.v1` | `requirements` and `invalidators` arrays described below |
| `admit` | `agent-hook.finish-line.admit.v1` | `contract_digest`, `operation_id`, private `attempt_token`, and strict tagged `operation` |
| `observe` | `agent-hook.finish-line.observe.v1` | `operation_id` and strict tagged `observation` |
| `verdict` | `agent-hook.finish-line.verdict.v1` | `contract_digest` and optional `completion_reservation = {operation_id}` |

`register.requirements[]` is exactly `{name, validators}`. Each validator is
exactly `{id, tool_name, definition_digest, execution}`. `execution` is either
`{"kind":"host-observed"}` or
`{"kind":"contained-bash","intent":...,"command":...}`.
`register.invalidators[]` is exactly `{tool_name, definition_digest}`.

`admit.operation` is one of:

```json
{"kind":"mutation","tool_name":"...","definition_digest":"sha256:..."}
```

```json
{"kind":"validator","requirement":"...","validator_id":"...","tool_name":"...","definition_digest":"sha256:...","source_operation_id":"..."}
```

`source_operation_id` is required only for a `contained-bash` validator and is
forbidden for `host-observed`. It reserves the exact future `finish-line run`
operation before that run exists. The source cannot predate admission, be
shared by two acceptance operations, or be reused after a terminal result.

`observe.observation` is exactly
`{"kind":"host-observed","status":"..."}` or
`{"kind":"contained-bash","operation_id":"..."}`. The contained form has
no caller-controlled status.

Successful command data uses the matching
`agent-hook.finish-line.<command>-result.v1` schema inside
`cli.agent-hook.finish-line-<command>.v1`. `register` returns `status`,
`contract_digest`, and `requirement_count`; `admit` returns `status`,
`operation_id`, `operation_kind`, `generation`, and `contract_digest`;
`observe` returns `status`, `operation_id`, `generation`, and normalized
`observation`; `verdict` returns `action`, `aggregate`, `generation`,
`contract_digest`, ordered `reason_codes`, and lexically ordered requirement
records. Every result also carries the opaque repository/session
`correlation_id`. Unknown or cross-variant fields are rejected.
When requested, `verdict.completion_reservation` is `null` for a blocking
verdict or exactly `{operation_id,status}` for an all-satisfied reservation;
status is `reserved` or `duplicate`.

## Registration

`register` accepts one bounded contract for the exact repository and DSH
session:

- 1 through 128 unique ASCII requirement names;
- 1 through 16 unique validator IDs per requirement;
- exact validator tool name and lowercase SHA-256 ToolDefinition digest;
- validator execution kind `host-observed` or `contained-bash`;
- for `contained-bash`, one valid intent and command that match an exact
  current agent-docs DSH validation target; and
- at most 128 unique invalidating tool-definition bindings.

Nils canonicalizes maps and sets, hashes tool-definition bindings, hashes Bash
commands, and returns the canonical contract digest. It persists requirement
and validator IDs because they are the public names in a detached verdict, but
does not retain tool names, definition digests, Bash command text, tool
arguments, output, model text, session IDs, operation IDs, attempt tokens, or
the runner capability.

An exact registration retry is idempotent. Duplicate names within a request or
a different contract for the same active session fail closed. Released session
records are evicted oldest-first only under the bounded session limit and only
when the authoritative finish-line state no longer has that session active.
Eviction loses evidence rather than manufacturing success.

## Admission and generation ordering

`admit` binds an operation ID and private attempt token to one exact registered
definition, contract, repository, session, turn, and capability incarnation.
It accepts either:

- `mutation`, which must match a registered invalidator; or
- `validator`, which must name an exact registered requirement and validator.

A mutation reservation is persisted first, the existing authoritative
finish-line generation is advanced and persisted second, and the admission is
marked durable last. The RPC returns only after all three writes. Therefore no
tool body can run with prior-generation evidence after successful admission.
An exact retry reconciles a crash between those writes only when the reserved
generation is the current generation or its exact successor; every
non-terminal reservation remains repository-verdict-relevant, and all other
states remain infrastructure-blocked even if an earlier client later advances
the generation. Failure, cancellation, or uncertainty after admission never
decrements the generation or restores older evidence. A denial before
admission changes neither generation nor evidence.

A validator admission records `active` evidence at the current generation. A
new attempt for the same requirement supersedes the earlier attempt. A
mutation may race an active validator by advancing the generation; the older
validator completion then becomes `stale` and cannot satisfy the new
generation. Mutation serialization is repository-wide, not session-local:
acceptance mutations, ordinary supervised Bash, and earlier validation
admission cannot cross an active mutation boundary. A validator or another
mutation cannot start while any acceptance mutation or ordinary supervised
Bash operation remains active in the repository.

An all-satisfied `verdict` may atomically reserve goal completion under the
same repository lock. While that operation remains non-terminal, every
generation-changing acceptance mutation, legacy edit, and ordinary supervised
Bash admission is denied with `finish-line-completion-reserved`. Validators
remain eligible because they cannot advance the generation. The exact owning
capability consumes or cancels the reservation through the existing
host-observed `observe` operation. This keeps the four public RPCs unchanged
while closing the gap between an asynchronous verdict read and DSH's
synchronous goal-state mutation.

## Terminal observation

`observe` is accepted only from the current session capability and only for an
operation admitted by that same capability incarnation. Its strict source is
either `host-observed` with one normalized status, or `contained-bash` with the
operation ID of an exact authoritative `finish-line run`. Supported normalized
statuses are:

- `succeeded`;
- `failed`;
- `cancelled`;
- `timed-out`;
- `signalled`;
- `uncertain`; and
- `infrastructure-blocked`.

Only an applied `succeeded` validator satisfies its requirement. Normal
failure becomes `failed`; cancellation, timeout, signal, and uncertain results
become `uncertain`; provider or supervisor failures become
`infrastructure-blocked`. A stale or superseded terminal record remains
durable but changes no current evidence. An identical terminal retry is
idempotent; a conflicting terminal result fails closed.

For declared Bash validation, admission first reserves one exact future
`finish-line run`, then the runtime invokes that contained executor and cites
the same operation without a caller status. Nils verifies the single-use
reservation, exact session, generation, target digest, validation-contract
digest, applied disposition, and stored execution facts, then derives success,
failure, timeout, signal, or cancellation itself. A returned provider failure
or authenticated quiescence after supervisor loss durably terminalizes that
exact acceptance operation as `infrastructure-blocked`. A host-observed result
cannot satisfy a contained validator. Ordinary Bash continues to advance the
shared generation and never creates validation evidence. Non-shell tools use
the host-observed terminal path after the runtime has bound the exact visible
DSH ToolDefinition and terminal lifecycle result.

## Detached verdict and completion reservation

`verdict` returns every requirement in lexical name order plus an aggregate and
`allow` or `block`. The only statuses are:

| Status | Meaning |
| --- | --- |
| `satisfied` | Exact current-generation evidence succeeded. |
| `missing` | No evidence exists for the current generation and contract. |
| `failed` | The latest exact current validator failed normally. |
| `active` | The latest exact operation is admitted and not terminal. |
| `uncertain` | Cancellation, timeout, signal, or another ambiguous terminal state occurred. |
| `infrastructure-blocked` | Authority, durable admission, provider, or recovery state cannot be proven. |

Aggregate precedence is `infrastructure-blocked`, `uncertain`, `active`,
`failed`, `missing`, then `satisfied`. Only an all-`satisfied` verdict exits
zero and returns `allow`. An uncertain or infrastructure-blocked mutation may
be reconciled by exact successful validation of every requirement at the same
generation with attempts sequenced after that mutation; older validation can
never reconcile it.

The verdict also folds every repository-relevant acceptance mutation and
pending ordinary supervised Bash operation, including operations owned by a
different DSH session. A current pending operation is `active`; an older or
unreconciled authority boundary is `infrastructure-blocked`.

A detached all-satisfied read is diagnostic and stop-eligible but is not by
itself goal-completion authority. When `completion_reservation` is requested,
the provider evaluates the same verdict and persists the reservation before it
returns `allow`, all under the existing repository lock. A competing mutation
therefore either wins first and makes the verdict block, or loses to the
reservation and is denied; both cannot succeed. Exact reservation retries are
idempotent. Runtime disposal and session release terminalize an unconsumed
reservation as infrastructure-blocked, while a successful synchronous goal
assertion consumes it as a host-observed terminal.

The sidecar state is strict, owner-only, symlink-resistant, bounded to 384 KiB,
and serialized under the existing per-repository finish-line lock. It lives
beside, rather than inside, the released finish-line V1 state file. This keeps
the 1.27.9 reader rollback-compatible: rollback ignores the sidecar and sees
only the advanced generation, which fails closed until its own exact
validation contracts are satisfied.

## Recovery and compaction

Every invocation reconstructs the verdict from durable state. Terminal
operations are compacted oldest-first while active operations and completion
reservations,
current-generation mutation blockers, current requirement evidence, claimed
contained sources, and a bounded recent idempotency window remain. Session
pressure never evicts a session with a non-terminal acceptance operation.
Session release refuses any non-terminal mutation or validator operation
regardless of capability incarnation, and terminalizes its own completion
reservation before returning. If crash-orphan recovery removes or replaces the
owning capability, the next locked provider operation terminalizes the orphaned
reservation as infrastructure-blocked before permitting mutation. Reopening a
cleanly released session with a new capability preserves terminal evidence; if
recovery encounters active evidence bound to an older capability incarnation, it reports
`infrastructure-blocked` and release remains blocked. Neither session
projection nor model text is an authority source.

Malformed requests change no state. Corrupt, oversized, wrong-repository,
wrong-schema, untrusted, unavailable, or lock-contended state fails with a
typed error. Unproven quiescence never becomes success.
