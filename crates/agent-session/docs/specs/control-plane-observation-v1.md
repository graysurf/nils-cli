# Control-plane observation v1

Status: implemented for `sympoies/nils-cli#1409`.

Ownership: normative contract for the `agent-session.observation.v1` event plane,
its bounded spool, and the `agent-session diagnose` bundle. The writer library is
`nils-common::observation`; producers are `agent-hook` and `agent-session`.

## Purpose and boundary

Before this plane existed, control-plane evidence was split across surfaces that
individually could not explain a degraded session:

- per-session activity journals recorded provider turns, not hook dispatch;
- `activity.diagnostic.json` retained only the current runtime's latest hook
  failure and was removed after a later success, so transient failures vanished;
- `agent-hook --trace` was opt-in and the installed provider ingress never passed
  it, and its success-path append happened after early dispatch failures;
- `agent-session logs` is a provider-pane or one-shot run view;
- `/healthz` proved only that an HTTP handler answered;
- daemon stderr in journald carried volume without correlation.

This plane records **classification only** for every terminal control-plane
outcome, including outcomes that occur before a provider payload can be
normalized or a policy can be loaded.

It is not a transcript, an audit log of user intent, or a metrics pipeline. It
does not replace the coordination registry, the activity journal, or the
governed recovery audit record.

## Failure-domain independence

Writers append directly to bounded local spool segments. Appending requires no
serve daemon, no coordination broker, no policy bundle, and no capability.
Recovery-critical logging must not sit behind the subsystem it diagnoses.

Appending is best-effort by contract. A spool failure is recorded nowhere and
changes no decision: logging must not become a new way to block a session.
Aggregation and indexing happen on read.

## Privacy budget

Events carry classification only. Every string field is validated against a
bounded character set **before** any write is attempted, and a field that does
not validate refuses the entire append rather than writing unchecked bytes.

The plane therefore cannot carry prompts, transcripts, command bodies,
filesystem paths, raw provider or session identities, capability values, tokens,
or provider error text.

| Field | Rule |
| --- | --- |
| `stage`, `code`, `disposition`, `product` | lowercase kebab-case token, `[a-z0-9-.]`, ≤128 bytes |
| `event` | canonical provider event name, ASCII alphanumeric, ≤128 bytes |
| `binary_version`, `peer_version` | release string, `[A-Za-z0-9.+_-() ]`, ≤64 bytes |
| `runtime_generation` | opaque token, same rule as `stage` |
| `correlation` | exactly `sha256:<64 hex>` |
| `recovery_action` | `[A-Za-z0-9 \-_.<>=]`, ≤256 bytes; path separators are rejected |

`correlation` is a digest over a fixed domain, the product, and the provider's
own turn identity (`turn_id`, else `prompt_id`, else the session identity). It is
stable for repeated deliveries of one turn, which is what makes a degraded prompt
replayable at most once, and it never echoes the identity it was derived from.

## Event schema

Literal `schema_version = "agent-session.observation.v1"`, strict unknown-field
handling.

Required: `schema_version`, `recorded_at` (RFC3339 UTC), `recorded_at_epoch`,
`component`, `stage`, `code`, `severity`, `binary_version`.

Optional: `product`, `event`, `disposition`, `duration_ms`, `peer_version`,
`runtime_generation`, `correlation`, `recovery_action`.

`component` is `agent-hook`, `agent-session`, or `launcher`. The `launcher`
component exists so an immutable launcher or supervisor can record exec failure
when the target binary never starts, which keeps an exit `127` observable — that
failure happens before either product binary executes, so logging inside them
cannot cover it.

`severity` is `info`, `warn`, `error`, or `critical`. A policy block is `info`:
the policy worked. Only a degraded lane, a crossed release boundary, or a failed
stage is `warn` or worse, because those are the states an operator must act on.

## Spool, rotation, and retention

- Root: `<state-dir>/observation/spool`, directories `0700`, segments `0600`.
- Segment names are `segment-<12-digit index>.jsonl`, so lexical order is
  chronological.
- One segment retains at most 256 KiB; at most 16 segments are retained; a
  segment older than 14 days is dropped.
- Paths are validated without following symlinks before any create, chmod, or
  write. A symlinked spool root, a non-directory, foreign ownership, group or
  world permissions, or a relative state root refuses the append.
- Appends serialize on an exclusive `flock` over `<spool>/.lock`.
- A truncated tail line from a concurrent rotation is skipped on read rather than
  failing the whole read.

## Producer requirements

`agent-hook dispatch` records exactly one terminal event per invocation:

| Stage | When |
| --- | --- |
| `stdin` | the provider payload could not be read |
| `normalize` | the payload could not be normalized (pre-policy failure class) |
| `recovery` | recovery capability consumption or emergency evaluation failed |
| `policy` | config/policy load or rule preparation failed |
| `coordination` | the coordination projection or transaction failed |
| `evaluate` | rule evaluation failed |
| `trace` | the opt-in redacted trace could not be appended |
| `dispatch` | a decision was produced (`code = dispatch-completed`) |

A failing stage records the stable `HookError` code as its `code`, `error` as its
disposition, and the classified recovery action when the fault is a known
recoverable one. Additional non-terminal events are recorded for a degraded lane,
a terminal Stop exit, a crossed broker release boundary, and a discarded helper
override.

## Diagnostic bundle

`agent-session diagnose [--limit N] [--format text|json]` returns
`agent-session.diagnostic-bundle.v1` in the workspace envelope.

- `binary_version`: producing release, long form.
- `health`: `healthy`, `degraded`, or `critical`. Any `critical` event is
  critical; any `warn`/`error` event or any crossed broker release boundary is
  degraded; otherwise healthy.
- `runtime.executable_state`: `live`, `replaced`, or `unknown` from
  `/proc/<pid>/exe`. Checking an installation symlink is insufficient: an upgrade
  can leave the symlink correct while a live process references a deleted inode.
  Platforms without procfs report `unknown` rather than guessing.
- `runtime.release_skew`: live broker records whose published release crosses a
  minor or major generation from this binary, each with a bounded
  `recovery_action`. A broker that published no release is compatibility state,
  not drift. A patch-level difference is not drift.
- `observation.event_count`, `first_seen_epoch`, `last_seen_epoch`.
- `observation.summary`: per `(component, code)` counters with severity, count,
  and first/last-seen window, ordered most severe then highest volume. Repeated
  display is rate-limited by keeping counters instead of reprinting occurrences.
- `observation.recent`: the newest events, oldest first, bounded by `--limit`
  (default 20, maximum 200).

The command reads only the filesystem. The daemon being down is one of the states
it exists to diagnose, so it must never depend on it; an unreadable coordination
registry yields an empty `release_skew` rather than an error.

## Broker release publication

`agent-session.coordination-registry` broker records carry an optional
`binary_version`, which is the release that created the record. The field is
additive: a record written before it existed deserializes with `None` and is
treated as compatibility state.

A registry body that parses in the
`agent-session.coordination-registry.` family but declares an unimplemented
version is release drift, reported as a distinct read error. A body that does not
parse, or that belongs to another schema family, remains corruption. Keeping the
two apart is what lets a consumer offer a bounded upgrade recovery instead of a
dead end.
