# agent-hook v1 contract

Status: frozen for `graysurf/agent-runtime-kit#686` Lane A and extended with
the native WorkspaceLease v1 boundary for `sympoies/dsh-runtime-kit#56`.

Ownership: crate-local canonical specification for `nils-agent-hook` and the
`agent-hook` binary.

## Purpose and boundary

`agent-hook` is the sole owner of nils-cli-managed Codex and Claude hook
registrations and the policy engine behind the external DSH runtime bundle.
Provider-native hooks remain lifecycle ingress, but their owned commands
contain only `agent-hook dispatch --product <provider>`; policy rules never
live in provider configuration. DSH is registered by `dsh-runtime-kit`, not by
`agent-hook setup`. The CLI does not manage unrelated hooks, execute
config-defined programs, weaken lower-level transaction/privacy rules, or claim
native setup ownership for providers without a compatible runner.

## Files and limits

The CLI resolves only absolute XDG roots and rejects relative roots.

| Role | Default | Limit / mode |
| --- | --- | --- |
| user config | `${XDG_CONFIG_HOME:-$HOME/.config}/agent-hook/config.toml` | 64 KiB, regular file, no symlink |
| policy bundle | config-selected absolute path, normally below `${XDG_DATA_HOME:-$HOME/.local/share}/agent-hook/policies/` | 1 MiB, regular file, no symlink |
| runtime state | `${XDG_STATE_HOME:-$HOME/.local/state}/agent-hook/` | directories `0700`, secret files `0600` |
| provider payload | standard input | 1 MiB |
| workspace-lease request | standard input | 256 KiB, strict duplicate-free JSON |
| workspace-lease state | state-owned per-worktree JSON | 512 KiB, `0600`, keyed private identity |
| trace | state-owned JSONL | 256 entries, 256 KiB, redacted metadata only |
| rule inventory | one policy bundle | 512 rules, unique IDs |
| reason metadata | one aggregate decision | 64 reasons, 256 bytes per code/message |

Unknown fields are rejected in every persisted/input schema. Service response
envelopes are the compatibility exception: consumers accept additive envelope,
error, and command-result metadata within the same schema version while still
requiring the exact schema version, success state, and mandatory result fields.
Identifiers are ASCII kebab-case, at most 128 bytes. Policy/config digests are
lowercase `sha256:<64 hex>` values over canonical serialized bytes.

## Versioned schemas

Every persisted/input schema has a literal `schema_version` and uses strict
unknown-field handling. Service responses use the additive compatibility rule
above.

- `agent-hook.config.v1`: selected policy path and digest, per-provider mode,
  and rule overrides. It cannot contain commands, bearer material, environment
  assignments, executable paths, or recovery state.
- `agent-hook.policy.v1`: bundle ID/version and ordered typed rules. A rule has
  stable ID, products, events, optional matcher, priority, mode, failure
  posture, override class, and one built-in capability binding. The concrete
  TOML serialization is frozen below.
- `agent-hook.normalized-request.v1`: request ID, product, canonical event,
  optional matcher, bounded target/command/snapshot digests, and public boolean
  facts. Raw prompts, tool input, command strings, paths, session IDs, mailbox
  bodies, environment values, and provider payload fragments are never stored.
  The provider's own Stop re-entry marker (`stop_hook_active`) is projected into
  one public boolean fact. It is trusted in exactly one direction: `true` may end
  a turn that is already looping, and it can never grant authority, release a
  claim, or downgrade a proven owner.
- `agent-hook.dsh-ingress.v1`: strict DSH-to-policy transport for one native
  extension event. Version 1 carries exactly `event`, bounded `call_id`, an
  absolute `cwd`, and a `tool` object with bounded `name` and object-valued
  `arguments`. Unknown fields are rejected at the root and nested tool object,
  so v1 explicitly forbids `subject`.
- `agent-hook.dsh-ingress.v2`: the current DSH transport. It retains the v1
  fields and adds one strict `subject` containing bounded `session_id`, positive
  `turn` and `step`, an absolute `agent_docs_state_home`, and an optional
  absolute `agent_docs_home`; v2 requires this complete subject. V1 remains
  accepted for compatibility, but typed
  identity-dependent DSH policy groups fail closed without the v2 subject.
- `agent-hook.dsh-ingress.v3`: the DSH lifecycle transport. It accepts exactly
  `agent/pre-step` with a required UTF-8 prompt of at most 64 KiB, a positive
  step, and an optional closed-enum session-start source. The accepted values
  are `startup`, `resume`, `clear`, `compact`, and `observed`; the first four
  are rc.7 lifecycle sources and `observed` is the adapter-derived value for a
  late or hot-reloaded attachment. It also accepts
  `agent/turn-stopping` with neither prompt nor step/source. Both shapes retain
  the v2 subject identity and absolute agent-docs roots. Unknown or cross-event
  fields are rejected. The runtime bundle continues to use v2 for tool ingress.
- `agent-hook.dsh-ingress.v4`: the DSH post-tool transport. It carries the same
  strict call, subject, cwd, and object-valued tool identity as v2 plus exactly
  `result.is_error: boolean`. `false` normalizes to `PostToolUse`; `true`
  normalizes to `PostToolUseFailure`. Candidate value, content, error objects,
  stdout, and stderr are neither accepted nor persisted.
- `agent-hook.finish-line.{open,begin,run,stop,status}.v1`: strict DSH lifecycle
  requests for the native execution-owned finish line. Their
  command result schemas use the matching
  `agent-hook.finish-line.<command>-result.v1` name and the normal
  `cli.agent-hook.finish-line-<command>.v1` service envelope.
- `agent-hook.finish-line.{quiesce,release}.v1`: strict internal DSH cleanup and
  session-retirement requests. Their matching result and service schemas follow
  the same naming rule, but both commands are intentionally absent from public
  help and completion.
- `agent-hook.workspace-lease.{bind,begin,complete,renew,release}.v1`: strict
  native WorkspaceLease provider requests. Matching results use
  `agent-hook.workspace-lease.<command>-result.v1` inside the normal
  `cli.agent-hook.workspace-lease-<command>.v1` service envelope.
- `agent-hook.normalized-decision.v1`: aggregate action, ordered reason codes,
  optional bounded context or replacement, shadow observations, and config /
  policy digests.
- `agent-hook.trace.v1`: timing, rule IDs, disposition classes, and digests;
  never raw payload, paths, identities, message content, or capabilities.
- `agent-hook.setup-plan.v2`: product, install/remove operation, owned
  event/matcher groups, role-tagged before/after digests or file absence for
  every provider source, exact plan digest, drift state, and whether apply is
  permitted. Provider hook argv content is omitted.
- `agent-hook.doctor.v1`: product status (missing, compatibility-only, `dual`,
  `drifted`, `converged`, `unsupported`, or `unrelated`), owned counts,
  compatibility residue count, digests, policy availability, recovery health,
  registration owner, and whether direct dispatch is supported.
- `agent-hook.inventory.v1`: ordered public rule metadata and effective mode;
  capability parameters and private paths are omitted.
- `agent-hook.recovery-challenge.v1`: random challenge ID, exact product/event,
  target/command/snapshot digests, requested rule IDs, scope, issue/expiry time,
  state revision, and a digest-bound `agent-hook.recovery-manifest.v1` projection
  of every enforceable rule for that product/event.
- `agent-hook.recovery-capability.v1`: capability ID, challenge digest, exact
  binding, scope, expiry, nonce, state revision, and authorization proof digest.
  The capability file is the bearer and is never printed.
- `agent-hook.owner-liveness.v1`: only classification (`active`, `stale`,
  `orphaned`, `unknown`, or `unclaimed`), semantic conflict class, and
  content-free reason codes.
- `agent-session.observation.v1`: the centralized control-plane event plane.
  Dispatch records exactly one terminal event per invocation — including
  outcomes that fail before normalization or policy load — plus non-terminal
  events for a degraded lane, a terminal Stop exit, a crossed broker release
  boundary, and a discarded helper override. Recording is unconditional rather
  than gated behind `--trace`, is independent of any daemon, broker, or
  capability, and is best-effort so it can never block a session. Schema,
  privacy budget, spool bounds, and the diagnostic bundle are normative in
  `agent-session/docs/specs/control-plane-observation-v1.md`.

Service JSON uses the workspace envelope: `schema_version`, `ok`, then `data`;
failures contain `error.code`, `error.message`, and
optional redacted `error.details`. Text is the human default except `dispatch`,
whose default is provider output, and `finish-line` plus `workspace-lease`,
whose default is service JSON. `--format json` always selects the service
envelope.

## Provider normalization and rendering

Products are `codex`, `claude`, `dsh`, and `hermes`. Codex, Claude, and DSH are
enforceable. DSH setup reports `unsupported` because the out-of-tree runtime
bundle owns registration. Hermes validates and evaluates shared policy but
reports `unsupported` for native setup until a compatible runner exists.

Supported canonical events are:

- Codex: `SessionStart`, `UserPromptSubmit`, `PermissionRequest`, `PreToolUse`,
  `PostToolUse`, `PostToolUseFailure`, `PreCompact`, `PostCompact`,
  `SubagentStart`, `SubagentStop`, and `Stop`.
- Claude: `SessionStart`, `UserPromptSubmit`, `PermissionRequest`, `PreToolUse`,
  `PostToolUse`, `PostToolUseFailure`, `PreCompact`, `Stop`, `StopFailure`,
  `Notification`, `SubagentStart`, `SubagentStop`, `Elicitation`, and
  `ElicitationResult`.
- DSH: `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `UserPromptSubmit`,
  and `Stop`, normalized respectively from native `tools/pre-execute`,
  `tools/post-execute`, `agent/pre-step`, and `agent/turn-stopping` ingress.

The Codex/Claude/Hermes adapter accepts the event from `--event` or the
provider's documented `hook_event_name`/`event` field and rejects a mismatch.
The DSH adapter accepts strict `agent-hook.dsh-ingress.v1`, v2, v3, or v4; the
runtime bundle uses v2 before tools, v4 after tools, and v3 for agent lifecycle
events. An optional
`--event` must equal its native event string before that string is mapped to
the canonical policy event. Matcher values are normalized only from documented
fields:

| Provider events | Matcher input field |
| --- | --- |
| Codex/Claude `SessionStart` | `source` |
| Codex/Claude `PermissionRequest`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure` | `tool_name` |
| DSH `PreToolUse`, `PostToolUse`, `PostToolUseFailure` | `tool.name` |
| Codex/Claude `PreCompact`; Codex `PostCompact` | `trigger` |
| Codex/Claude `SubagentStart`, `SubagentStop` | `agent_type` |
| Claude `Notification` | `notification_type` |
| Claude `Elicitation`, `ElicitationResult` | `mcp_server_name` |
| Claude `StopFailure` | `error` |

Correlation identities are read from the provider's own fields and projected
into opaque local ids; raw provider values never leave the adapter. The session
identity comes from `session_id`, or `session_key` when that is the provider's
name for it. The turn identity comes from `turn_id`, falling back to `prompt_id`
for Claude, which is the field Claude actually emits and which stays stable
across the turn's own `Stop` and idle `Notification` events.

Path-bearing mutation inputs are normalized before rule evaluation. `Write`,
`Edit`, and `MultiEdit` require exactly one non-empty `path` or `file_path`;
`NotebookEdit` requires `notebook_path`; and Codex `apply_patch` requires its
native `tool_input.command` string to contain a bounded, structurally complete
patch whose `Add File`, `Update File`, `Delete File`, and `Move to` directives
are all mapped. The undocumented `tool_input.patch` alias is not accepted.
Relative targets require an absolute provider execution directory. Missing,
ambiguous, malformed, oversized, or only partially mapped targets fail closed
as `provider-target-untrusted` rather than falling back to the execution
checkout. Existing targets and the nearest existing ancestor of a new target
are resolved through one fail-closed binding resolver before ownership and
recovery evaluation. This includes final and intermediate symlinks: a path in
one checkout that resolves into another checkout is classified and bound to
the effective checkout, while dangling, cyclic, or otherwise ambiguous links
are rejected. Multi-target requests retain every distinct effective target for
owner-liveness evaluation and bind recovery to a deterministic target-set
digest. The v2 binding domain and ordinary canonical non-symlink single-target
digests remain unchanged. Raw target paths are never serialized in the
normalized request, decision, trace, or recovery artifact.
Owner-liveness evaluates every distinct target checkout plus the execution
checkout and returns the strongest result; an active foreign owner therefore
cannot be masked by a self-owned or unclaimed target.

DSH tool ingress preserves the complete object-valued tool arguments for policy
normalization. Native `write` and `edit` map `file_path`, and mutating
`str_replace_editor` commands map `path`, to an exact checkout-bound target.
Opaque Bash mutation uses the canonical repository target and the
`agent-session` shell-coverage contract; an exact file claim remains narrow.
Policy validation supports native allow/block admission and bounded model
context. DSH does not support policy argument transforms or retired file
handlers. Identity-dependent `dsh.policy.v1` groups require the complete
v2/v3/v4 subject.

Task 3.4 implements `agent-activity` and `operation-lifecycle`. Activity is
derived only from the normalized DSH subject and canonical event: it sends
provider/session/turn metadata, event kind, confidence, and digests to
`agent-session activity event`; prompt and tool arguments are never parsed for
that event. A managed activity identity requires both session and runtime ID.
When Main Agent policy replaces the DSH ingress subject with its authenticated
coordination owner, the runtime transports the original DSH provider session
only through the private `DSH_RUNTIME_KIT_PROVIDER_SESSION_ID` subprocess
environment. Activity normalization uses that value for provider correlation
while every other policy group continues to use the owner subject. The public
ingress schema is unchanged, and `agent-session` still rejects a value that
does not match the provider session already bound to the active runtime.

Operation lifecycle is one locked after-policy capability. An unmanaged process
does not run it. A managed process requires session ID, an absolute capability
file, and an absolute state root; the presence of any forwarded managed-session
selector makes an incomplete identity fail closed. Before a covered mutation, it obtains the
active claim, atomically writes owner-private targets, execution token, hashed
provider correlation, and deterministic idempotency keys, then invokes the
same-release sibling `agent-session work-context admit`. The post-tool v4 fact
drives the matching `complete --outcome pass|fail`. A repeated pre-tool identity
never authorizes a second execution; terminal post retries reauthenticate the
same idempotent completion and retain the original admitted revision. A known
denial removes its complete provisional directory; an ambiguous child/transport
result remains pending. Stop calls hidden authenticated `agent-session broker
status` and permits exit only when the exact incarnation reports zero active and
zero uncertain operations. Local state is a retry cache and cannot satisfy
Stop. State never contains raw
arguments, output, prompt, capability content, or the provider call ID.
Read-only and unmatched post calls create no state. A private session lock
serializes capacity changes, successful terminal retry records carry a durable
monotonic completion sequence and compact to 64,
and the bridge refuses a new admission once 128 operation directories remain;
active, uncertain, malformed, extra, or currently locked state is never evicted
to make room.

Built-in semantic-conflict and owner-liveness admission applies only to a
managed process. When `AGENT_SESSION_ID`, `AGENT_SESSION_RUNTIME_ID`,
`AGENT_SESSION_STATE_DIR`, `AGENT_SESSION_COORDINATION_MODE`,
`AGENT_SESSION_CAPABILITY_FILE`, and `AGENT_SESSION_CHECKPOINT_FILE` are all
absent, both capabilities return `allow` with `coordination-unmanaged` without
loading or classifying registry owners, and the coordination transaction is not
invoked. Presence of any of those selectors without a complete session ID and
runtime incarnation is not unmanaged; it retains the conservative managed
failure posture. Environment-only advisory or off hints never downgrade an
untrusted managed identity.

A matcher on any other product/event pair is rejected during policy validation.
A policy matcher is either
one literal token or an anchored alternation expression such as
`Write|Edit|NotebookEdit|MultiEdit|apply_patch`; each atom is compared to the
entire normalized matcher. The only expression operator is `|`. Empty atoms,
duplicates, more than 64 atoms, and regex/glob/grouping/escape constructs are
invalid. Setup renders the original validated expression as one provider group
matcher, preserving registration grouping and order; it does not explode the
expression into per-tool rules. Oversized input, invalid UTF-8/JSON,
unsupported events, duplicate object keys, or unknown normalized fields fail
before rule evaluation. When ordinary config cannot be loaded, an exact valid
recovery capability evaluates its signed rule manifest and preserves every
ungranted rule instead of producing a global allow.

Provider rendering maps one normalized aggregate result to the event-native
output algebra. `PreToolUse` transforms render `permissionDecision = "allow"`
with `updatedInput`; Claude `PermissionRequest` transforms use
`decision.updatedInput`; Claude `PostToolUse` transforms use
`updatedToolOutput`; `PermissionRequest` decisions render
`decision.behavior = "allow" | "deny"`; Claude elicitation blocks render
`action = "decline"`; and other blocking events use the provider's documented
event-appropriate decision shape. Provider payloads that cannot be normalized
safely produce no stdout, a concise stderr diagnostic, and exit `2` so the
provider applies its native blocking fallback. Runtime or service-format
failures use exit `1`; successful provider-native decisions use exit `0`.
`dispatch --format json` returns the normalized decision envelope and does not
return raw provider content. DSH ingress requires this service JSON form; the
runtime bundle maps `block` to a `tools/pre-execute` denial, delegates `allow`,
and fails closed on every malformed, truncated, signaled, exit-mismatched,
replayed, timed-out, oversized, or unsupported response.

## Native DSH workspace lease

`agent-hook workspace-lease <command> --format json` reads one strict JSON
object from standard input, capped at 256 KiB. Duplicate keys, unknown fields,
unsupported versions, non-absolute supplied cwd values, invalid lifecycle
enums, and oversized or control-bearing identity strings fail with exit `65`.
The public surface is exactly `bind`, `begin`, `complete`, `renew`, and
`release`; every command defaults to its versioned service JSON envelope.

`bind` accepts the runtime-owned WorkspaceLease v1 facts: request, session,
optional parent, optional cwd, and DSH session-start source. The provider
canonicalizes cwd without inherited Git repository-selection state. A managed
identity binds the physical Git top-level and its device/inode identity, the
per-worktree Git directory, and the shared Git directory. Thus nested and
symlink-equivalent path spellings converge while two linked worktrees remain
distinct. A missing or non-Git cwd produces an `unmanaged` binding and never a
mutation lease.

Managed state permits one active binding generation per physical worktree. A
clean unowned checkout becomes `owned`; a live other owner returns
`foreign-active`; dirty state returns `dirty`; and an expired binding with an
active operation returns `uncertain`. An expired or explicitly released clean
binding with no active operation may be replaced atomically. Replacement mints
a new binding ID and generation and tombstones the prior generation so delayed
release cannot affect the new owner. No expired exact bind replay can revive
old authority.

An explicit `resume` or `compact` may also replace a dirty released or expired
generation when the host-authenticated session and optional parent lineage are
exactly the same and every operation is terminal. This is recovery of the same
owner's work, not reassignment: it still mints a new binding ID and generation.
A startup, clear, foreign session, changed parent, or unterminated operation
remains fail-closed.

`begin` binds the exact call/root/tool/arguments/nesting facts to one operation
ID and unpredictable fence before tool dispatch. Known read-only DSH tools
return `not-required`; unknown tools are conservatively fenced. `complete`
requires the exact live binding, generation, operation ID, fence, and
call/root/tool projection, and records one of `succeeded`, `failed`, or
`cancelled`. `renew` extends only a still-live exact generation. `release`
retires only its exact generation and fails unavailable while any operation
lacks a terminal outcome.

Active bind/begin retries and terminal complete/release retries are idempotent.
Reusing a request ID for different facts fails closed. State is stored below
the private `workspace-leases/` state root using bounded owner-only files,
atomic replacement, and a bounded per-workspace cross-process lock. A clean
expired owner or the exact explicit same-session recovery above can be
recovered; foreign dirty state and active-operation, malformed, identity-
mismatched, untrusted, busy, or unknown state are never guessed through.

Provider-visible results contain opaque IDs, renewal timing, and stable
`owned`, `unmanaged`, `foreign-active`, `stale-clean`, `dirty`, or `uncertain`
states with bounded stable codes/reasons. Results and diagnostics never expose
canonical paths, Git directories, session/parent IDs, tool arguments, command
output, or persisted digests. Canonical paths exist only in the private state
needed to revalidate physical identity and run a trusted, hooks-disabled Git
dirty-state probe.

## Native DSH finish line

`agent-hook finish-line <command> --format json` reads exactly one strict JSON
object from standard input. Version 1 accepts only `product = "dsh"`; every
request carries bounded non-space ASCII `session_id` and `turn_id` values plus
an absolute, already-canonical, non-symlink `cwd`. That value must be the exact
Git top-level resolved independently from both the request and the running
process; nested directories, nested repositories, unrelated repositories, and
non-repositories fail closed. Input is capped at 64 KiB; duplicate keys and
unknown fields fail with exit `65`. Every successful response carries the same
opaque `correlation_id` for one product/repository/session binding; consumers
must reject lifecycle responses whose correlation changes.
Finish-line commands default to the versioned service JSON envelope; text is
available only through an explicit `--format text` selection. Recoverable exit
`69`/`75` errors include typed `retryable`, `next_action`, and bounded
`recovery` details.

The DSH plugin routes every foreground Bash request through a non-executing
`run` probe before any execution. It rejects background Bash before invoking
the finish-line service. This is an integration boundary: the public
finish-line request does not accept a caller assertion that a command was
foreground, background, completed, or successful.

The public CLI surface is exactly `open`, `begin`, `run`, `stop`, and `status`.
Hidden `quiesce` and `release` commands exist only for DSH cancellation,
failed-execution cleanup, and authenticated disposed-session retirement. Both
are deliberately absent from public help and completion.

The request and response contracts are:

- `open` adds a caller-generated unpredictable `attempt_token`. It
  deterministically derives and returns one unpredictable `runner_capability`
  bound to that token, the exact repository, and the DSH session, while storing
  only the capability digest and a 24-hour lease expiry. Retrying the exact
  binding returns `status = "duplicate"`, the same capability, and a renewed
  lease, so a lost success response is recoverable. A different attempt for a
  live session returns `finish-line-session-active` and cannot rotate or learn
  its capability. The capability derivation includes a persisted monotonic
  incarnation sequence. After authenticated release, a new attempt may open a new
  capability incarnation for the same stable session identity, including an
  rc.7 resumed session. The old capability remains tombstoned and cannot affect
  the new incarnation. The attempt token is private idempotency material for
  the current live incarnation, not an authorization bearer or permanent
  revocation identity. Reusing it after release therefore starts a new
  persisted incarnation and derives a byte-distinct capability; it cannot
  resurrect the retired bearer. Bounded tombstone compaction may age out the
  old release's duplicate receipt but does not roll back the monotonic sequence
  or make the old capability valid. The
  caller must keep both attempt token and bearer
  private; `run` rejects a missing, stale, or cross-session value.
  Containment-host validation precedes identity and state mutation; non-Linux
  activation fails with exit `69` and
  `finish-line-containment-unavailable`. Linux activation also requires the
  trusted fixed systemd binaries, unified cgroup v2, enabled unprivileged user
  namespaces, and a responsive user manager.
- `begin` adds `operation_id`, a caller-generated unpredictable
  `attempt_token`, and `operation = {"kind":"edit"}`. It atomically advances
  the repository's monotonic integer generation before the edit proceeds. The
  token is never echoed or persisted; only its digest is stored. Retrying the
  exact request with the same binding returns `status = "duplicate"`, so a lost
  success response is recoverable. A reused operation ID with a different
  binding is invalid.
- `run` adds `operation_id`, `intent`, the exact command string, the private
  `runner_capability`, an optional `execution`, and a timeout from 1 ms through
  60 minutes (30 minutes by default). The intent and command are compared
  byte-for-byte with the current `agent-docs` validation contracts resolved for
  DSH. Omitting `execution` performs the required non-executing capability and
  target probe: an exact current validation target returns `status = "ready"`
  with its target and contract digests, while a non-target returns
  `status = "ordinary-ready"`. Neither probe runs the command or creates
  execution evidence.

  With `execution.kind = "bash-v1"`, nils is the executor for both branches. An
  exact target is reserved before launch and may create validation evidence. A
  non-target first advances the shared repository generation, is registered as
  an ordinary shell operation, and then runs; the first terminal response has
  `status = "ordinary-applied"` even when its observed exit is nonzero. An
  ordinary operation never creates target validation evidence, and its
  generation advance makes prior validation evidence stale. Exact retries of a
  terminal exact or ordinary binding return `duplicate` without re-execution or
  output replay.

  Exact validation execution requires the authoritative repository root as its
  work directory. Ordinary execution preserves its canonical requested work
  directory while advancing the generation of the authoritative repository
  named by the common request identity. Each output stream is bounded to at
  most 64 KiB. `unsandboxed` and
  `danger-full-access` runners use the trusted fixed
  `/bin/bash -c <exact-command>` path. A `confined` runner executes the
  provider-supplied bounded argv only when it ends in the exact
  `-- bash -c <exact-command>` tuple; its declared mode, enforcement strength,
  denial signatures, and runner-failure rules are validated and returned as
  bounded sandbox facts. Git environment overrides are removed and stdin is
  closed.

  Authoritative execution is Linux-only and fails closed elsewhere. On Linux,
  nils verifies the fixed `/usr/bin/systemd-run` and `/usr/bin/systemctl`
  executables as root-owned, executable, non-symlink regular files without
  group/world write bits. Before launch, nils serializes the bounded config into
  an anonymous memfd, sets mode `0400`, and requires the write, grow, shrink,
  and further-seal locks. No path-backed runner config exists.

  Systemd `OpenFile` opens `/proc/<supervisor-pid>/exe` as named descriptor 3,
  the sealed config memfd as named descriptor 4, and an unlinked mode-`0600`
  control memfd as named descriptor 5 for the unit. Nils reads the current
  runner's ELF program headers, verifies its dynamic interpreter as a root-owned
  executable without group/world write bits, and invokes that interpreter with
  `/proc/self/fd/3`. The contained runner accepts exactly the three expected
  systemd listen descriptors, verifies the config is an unlinked sealed regular
  file, and then parses the strict config. This binds execution to the current
  runner inode and immutable config rather than re-opening mutable path names.

  The config carries a random control nonce. Before spawning the provider, the
  contained runner validates descriptor 5, sets `FD_CLOEXEC`, makes its process
  non-dumpable, and publishes a nonce-bound `ready` record. The supervisor
  acknowledges only after that proves systemd opened the descriptors: it closes
  the config, changes control mode to `0400`, drops its writable control view,
  makes itself non-dumpable, and publishes the matching acknowledgement. The
  runner requires that exact acknowledgement and mode, then clears the
  acknowledgement before provider spawn. Provider code therefore cannot reopen
  a nonce-bearing config or writable control memfd through the outer or runner
  procfs descriptors; a forced stop before terminal publication leaves the
  intended empty, unsealed control state.

  After observing the child, the runner writes exactly one bounded strict-schema,
  nonce-bound provider exit, provider signal, or infrastructure-failure result
  and immediately seals descriptor 5 against all further writes. After the
  transient unit and cgroup are quiescent, the supervisor accepts only an
  already sealed result with the exact schema and nonce. Internal runner state
  therefore does not reserve provider exit codes or signals: exits 134 and 204
  remain exits, and Linux signals are returned as canonical `NodeJS.Signals`.
  An unrecognized signal fails closed.

  The config also binds the supervisor PID. The contained runner obtains a
  pidfd for that process and monitors it while the workload runs; supervisor
  disappearance triggers immediate workload process-group termination and a
  runner failure. Nils launches the runner in a transient user unit with
  `PrivateUsers=yes`, `Delegate=no`, `KillMode=control-group`, immediate
  `SIGKILL` stop/final-kill behavior, and a bounded `RuntimeMaxSec`. The unit
  admits only `AF_INET` and `AF_INET6` and denies localhost. Timeout or
  cancellation stops the whole unit. After the runner exits, nils queries the
  unit through the trusted `systemctl` and records execution facts only when
  `ActiveState` is `inactive` or `failed`.

  The transient cgroup boundary covers descendants that change process groups,
  call `setsid`, or double-fork. Private user-namespace capability reduction,
  the lack of `AF_UNIX`, and localhost denial close the tested parent-cgroup and
  user-manager delegation routes. These settings are not a general network
  namespace and do not claim to prevent every possible network or IPC
  delegation mechanism.

  Nils derives success or failure solely from the observed exit status, signal,
  timeout, and cancellation state. The caller cannot report an outcome,
  stdout, stderr, or completion decision. Initial execution returns the bounded
  observed output and execution facts. For an exact target, a later edit,
  changed contract, or newer attempt returns `stale` or `superseded` and cannot
  satisfy the current generation. A provider-runner or control failure does not
  become validation evidence and retains the pending unit binding until
  authenticated quiescence proves cleanup.
- `stop` resolves every exact command of every current DSH validation contract.
  It returns `action = "allow"` and exit `0` only when the current session has
  success evidence for every target at the current repository generation and
  exact contract digest. Missing, pending, failed, stale, or drifted evidence
  returns `action = "block"`, bounded reason and remediation arrays, and exit
  `1` while retaining `ok = true` in the service envelope. A repository at
  generation zero remains allowed. Once any session advances the shared
  repository generation, every session must supply its own success evidence at
  that current generation; an otherwise untouched session blocks rather than
  borrowing another session's evidence.
- `status` returns only the generation, current contract digest, correlation,
  target intent/digest/status, and optional attempt generation. It never returns
  raw commands, paths, identities, capabilities, tokens, or child output.
- The internal `release` request carries the common identity and exact
  session-bound `runner_capability`. It refuses release while any operation for
  that session is nonterminal or retains an active unit. Once quiescent, it
  removes the session and its terminal operation records. A bounded
  capability-digest tombstone makes an exact lost-response retry return
  `status = "duplicate"`. The tombstone binds that released capability
  incarnation rather than permanently retiring the stable session identity.
  A later open may create a new incarnation; retrying the old release remains
  duplicate while its bounded tombstone remains and cannot remove the new one.
  Reusing an old attempt token after release creates a new sequence-bound
  capability rather than resurrecting the old one. A defensive live-state
  check takes precedence over the duplicate path if persisted state ever
  contains both the tombstone and its matching live capability.

The internal `quiesce` request carries the common identity, exact
`operation_id`, and session-bound `runner_capability`. Pending validation and
ordinary operation records retain their generated `active_unit`. Cleanup
validates that binding and performs bounded trusted-`systemctl` stop and
inspection loops. Every loop also runs an exact-unit `list-jobs` barrier. A
single absent-unit observation is insufficient: cleanup requires three
consecutive observations 25 ms apart with no pending job and either a missing
unit or an inactive/failed, dead/failed unit whose extant cgroup reports
`populated 0`. Only after that stabilized proof does it remove the pending
operation and target attempt. Execution/control errors retain `active_unit` and
pending state so authenticated cleanup remains possible. The idempotent result is
`status = "quiescent"`; it cannot create terminal execution or validation
evidence.

There is no public `complete`, `waive`, `approve`, or `revoke` finish-line operation.
There is no caller-reported result path, review authority, waiver artifact, or
ambient waiver environment variable. Only a `run` execution observed by nils
can create success evidence.

The engine holds one transaction lock per repository for every state read or
mutation. Repository generation is shared across sessions, while target facts
remain product/session isolated. State keys bind the canonical
repository top-level and Git common-directory device/inode identities, exact
contract set, contract context, and exact command target. State never uses file
mtime as authority. The persistent repository lock is also an initialization
anchor: once created, disappearance of the paired state record fails closed
instead of resetting the generation. A safe prior marker is reported only as unresolved
transitional evidence; an unsafe marker is reported separately and neither can
satisfy native generation state.
Terminal operations are compacted deterministically by ascending sequence at a
256-record trigger down to a 192-record recent-idempotency window. Pending
operations are never compacted. Obsolete-generation target facts and empty
sessions without a runner capability are removed without changing current
generation enforcement. Authenticated release tombstones retain only digested
session and capability bindings plus sequence, are keyed by capability
incarnation, and compact to the most recent 64 entries; they do not consume the
live 64-session limit. Attempt tokens are not persisted. The monotonic
incarnation sequence makes every post-release capability byte-distinct even
when the caller reuses an old attempt token. Exact live `open` replay renews a
capability lease. Lease expiry alone
never removes a session, terminal evidence, or pending state. At the hard
live-session bound, a bounded recovery scan may reclaim an expired crash orphan
or quiescent expired session. It retires only the one entry required for the
new admission. A persisted deterministic cursor rotates the eight-candidate
window across eligible sessions, so an older busy window cannot indefinitely
hide a later reclaimable candidate. Every busy operation must retain an exact valid transient-unit identity,
passive status shows no live unit or pending job, and bounded stop/status,
`list-jobs`, and cgroup checks then prove stable quiescence. The store is locked
again and the exact capability digest, lease, operation keys, sequences, and
unit identities must still match before the session is tombstoned and removed.
Active, indeterminate, unbound, oversized, or migrated no-lease sessions remain
protected. A reclaimed stable identity may open a new lease with fresh or
reused caller-held retry material. Every post-release open advances the
persisted incarnation sequence and therefore derives a byte-distinct
capability; the retired bearer remains invalid after its duplicate-release
tombstone is compacted. An excess of nonterminal
operations still fails at the hard
512-operation bound. Both `open` session admission and `begin` edit admission
may invoke this recovery, so a new DSH session does not need to execute Bash
before its first edit can reclaim provably quiescent orphan capacity.

Finish-line directories must be private and owner-controlled. Lock and state
files are opened with `O_NOFOLLOW`, checked through their open descriptors as
owner-controlled regular files, and bounded to 384 KiB. Each update writes a
private create-new temp file, calls `fsync`, atomically renames it, and requires
the parent-directory `fsync` to succeed before reporting success. Lock/open
unavailability exits `69`; bounded lock contention exits `75`. Pending
operations may persist the generated active-unit identifier needed by internal
cleanup. Raw validation output is returned only in the initial bounded `run`
response and is never persisted. Contained config and nonce-bound control state
are held only in anonymous memfds; the trusted runner seals their contents
before either is accepted. No Python handler, shell command rewrite, `EXIT` trap,
caller-reported outcome, or waiver state participates in the engine.

Policy validation also checks the complete provider/event/capability binding,
not only each component in isolation. `decision.warn.v1` and
`decision.context.v1` require an event with native model-context semantics;
`decision.block.v1` requires native block, continuation, feedback, or decline
semantics; and `decision.transform.v1` is limited to Codex `PreToolUse` plus
Claude `PreToolUse`, `PermissionRequest`, and `PostToolUse`. DSH does not
transform arguments; it accepts native allow/block and context capabilities at
the three supported boundaries.
`agent-session.owner-liveness.v1` and
`agent-session.semantic-conflict.v1` require both context and block semantics
because their result is data-dependent. Neutral `decision.allow.v1`, metadata
side effects through `agent-session.activity.v1`, and trusted provider-native
`runtime-kit.handler.v1` remain valid for supported Codex/Claude events. DSH
rules reject retired file handlers so policy cannot fall back to the archived
runtime-kit implementation. This preserves
notification and failure logging without pretending that events such as
Claude `Notification` or `StopFailure` can enforce a decision.
`agent-session.coordination.v1` is limited to enforceable
`PreToolUse`, `PostToolUse`, `PostToolUseFailure`, and `Stop` events and must be
an `enforce`, fail-closed, locked rule.

## Serialized policy and capability registry

Policy bundles are strict TOML. This is the canonical serialized v1 shape;
unknown keys at every level are errors. Products/events are arrays so the same
stable rule can cover overlapping provider ingress. Capability-specific keys
live in the inline `capability` table and are rejected for a different ID.

```toml
schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "runtime.session-start-healthcheck"
products = ["codex", "claude"]
events = ["SessionStart"]
priority = 100
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "runtime-kit.handler.v1", handler_id = "session-start-healthcheck" }
```

The stable built-in capability ID set for policy v1 is closed:

- `decision.allow.v1` (`reason_code`)
- `decision.warn.v1` (`reason_code`, `message`)
- `decision.block.v1` (`reason_code`, `message`)
- `decision.context.v1` (`reason_code`, `text`)
- `decision.transform.v1` (`reason_code`, `replacement`)
- `agent-session.activity.v1` (`reason_code`)
- `agent-session.owner-liveness.v1` (`reason_code`, optional
  `legacy_ttl_seconds`, maximum 900)
- `agent-session.semantic-conflict.v1` (`reason_code`)
- `agent-session.coordination.v1` (`reason_code`)
- `execution.read-only.v1` (`reason_code`, optional `fallback_handler_id`)
- `runtime-kit.handler.v1` (`handler_id` from the compiled v1 allowlist)
- `dsh.policy.v1` (`group` from the eleven implemented Task 3.2, nine
  implemented Task 3.3, and two implemented Task 3.4 `DshCapabilityGroup`
  values)

The separate `agent-hook.dsh-policy-capability-groups.v1` Rust/JSON schema
freezes the 23 deterministic groups selected by the DSH migration inventory.
The matching fixture records whether each group was delivered by finish-line
Task 2.3 or is owned by policy Tasks 3.2, 3.3, or 3.4. `dsh.policy.v1` makes
the Task 3.2 through Task 3.4 IDs executable only on their declared DSH
`PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `UserPromptSubmit`, or `Stop`
event; adding a fixture ID alone cannot activate it. Task 3.3 tool and lifecycle
rules may add bounded model
context, while neither task can transform tool arguments.
These evaluators use normalized exact
targets, bounded command parsing, trusted same-release nils companions, and
private session-bound lease state. The DSH parity verifier compares its ordered
nils-capability set to this fixture before either repository can claim inventory
parity.

Task 3.3 Bash privacy checks classify output redirection with quote-aware shell
syntax and resolve adjacent quoted target fragments before comparing paths.
Known writers expose literal destinations, destination-directory options are
joined with source basenames, dynamic or malformed destinations fail closed,
and an otherwise unknown executable with a protected-path operand is
indeterminate rather than read-only. A small closed set of non-writing tools
remains readable when no output redirection is present. MCP native writes and
partial edits scan maintained credential shapes, generic credential labels,
and structural sensitive JSON keys. Edit and str-replace old/new value pairs
also preserve the sensitive old-value context: replacement is allowed only for
removal or a strict `$NAME`/`${NAME}` environment reference whose name matches
`[A-Za-z_][A-Za-z0-9_]*`. Fragments that cannot be parsed safely remain blocked
when they name a sensitive key. Startup memory is validated against the exact
companion schema and byte count, redacted across both shaped credentials and
generic `token`, `authorization`, or `private_key` labels, and rendered only as one escaped
`SHARED_AGENT_MEMORY_JSON` string so recalled content cannot terminate its
untrusted container.

For `block-unsafe-default-delivery`, the engine records an owner-private
projection of the canonical common Git directory identity and the unambiguous
cached remote HEAD before admitting Bash or native file mutation. The primary
checkout's current branch is not default-branch evidence and may differ from
the remote default. Multiple remotes must advertise the same default; missing,
contradictory, or drifted evidence is fail-closed, and native
write/edit/str-replace targets inside Git metadata are denied. The structured
`runtime_kit_governed_commit` tool is admissible only from its exact linked
worktree binding and has no repository-routing argument. Raw merge, pull,
cherry-pick, rebase, revert, am, reset, update-ref,
branch, push, and fetch shapes are classified against the resolved `git -C` or
semantic `--repo` target. `fetch --update-head-ok`, protected fetch
destinations, and stdin/server-driven ref-update plumbing are denied. A shell
builtin or assignment that can retarget cwd, exported variables, tracing hooks,
command lookup, or aliases before a later command makes every
command-dependent group fail closed. Git command-consuming forms such as
rebase exec and submodule foreach, plus explicit transport-helper selection,
are rejected rather than partially parsed.
Exact abort/quit recovery and owned delivery CLIs remain admissible.
`semantic-commit --message-file` and repeated scalar message/repository options
are not accepted at this transcript boundary.

Trusted policy companions receive a cleared environment. The scope-lock
companion resolves a fixed trusted Git binary and disables repository
fsmonitor, hooks, pager, untracked-cache, external-diff, and textconv behavior
for every policy-time Git probe.

`execution.read-only.v1` evaluates only `PreToolUse` requests through the
same-release operation-effect verifier. Enforce mode admits only an exact,
fresh descriptor whose producer, cwd, target, argv, provider effect, and
OS-enforcement contract verify; missing, malformed, unsupported, mismatched,
or mutation evidence blocks. Shadow mode runs the same verification but records
only the redacted observation and cannot change admission.

An enforce rule may set `fallback_handler_id = "pre-edit-intent-gate"` only
when it is fail-closed and locked and each product/event/matcher binding has
exactly one later-priority enforce, fail-closed, locked
`runtime-kit.handler.v1` rule for that handler. A verified read-only operation
then bypasses only that paired rule; every other selected rule still evaluates.
Missing, malformed, unsupported, mismatched, or mutation evidence falls through
to the paired project-development handler instead of creating an aggregate
block. The paired handler's native result remains authoritative. No other
handler ID or shadow/downgradable pairing is valid.

`runtime-kit.handler.v1` is not arbitrary execution. The binary maps the
handler ID through a compiled allowlist to one exact runtime-kit-owned handler
basename and resolves it only below the active provider's owned hook directory.
The v1 allowlist is:

`agent-scope-lock-guard`, `block-claude-coauthor-trailer`,
`block-direct-git-commit`, `block-direct-git-worktree`,
`block-direct-pr-create`, `block-direct-python`,
`block-project-memory-write`, `block-unsafe-default-delivery`,
`checkout-lease-guard`, `finish-line-record`, `forge-label-reminder`,
`mcp-secret-scan`, `memory-write-principle-reminder`,
`portable-paths-scan`, `pre-edit-intent-gate`,
`semantic-commit-body-gate`, `session-start-healthcheck`,
`skill-usage-reminder`, `stop-finish-line-gate`, `stop-pre-pr-reminder`,
`user-prompt-agent-docs`, and `user-prompt-agent-memory`.

Handler resolution selects the compiled `.py` or `.sh` suffix, rejects
symlinks/non-regular files and owner/permission drift, passes the original
bounded provider JSON on standard input, and preserves the handler's provider
output/exit semantics. One dispatch starts at most 17 executable capabilities,
retains at most 512 KiB of aggregate child output, and shares a five-second
absolute deadline aligned with the per-handler timeout. Each child leads an
isolated process group; timeout or
direct-child exit terminates descendants before bounded pipe draining. The
policy cannot specify a path,
interpreter, argv, environment assignment, shell fragment, timeout, or digest.
Shadow mode never invokes it. Adding another handler requires a new nils-cli
release or a new versioned capability ID.

For Codex and Claude, `doctor` resolves every policy-referenced
`runtime-kit.handler.v1` and `agent-session.coordination.v1` handler before an
otherwise converged provider is reported healthy. Activation health and
immediate pre-execution validation share one trust predicate: the resolved
handler must be a regular file, executable by its effective-user owner, and
have no group/world write bits. Any failures are collected before returning a
typed `handler-unavailable` or `handler-untrusted` error; the diagnostic names
each affected handler without exposing its private resolved path.

`agent-session.coordination.v1` is a separate typed after-policy capability,
not another configurable handler ID. It resolves only the exact
`session-coordination-guard.py` consumer below the active provider's owned hook
directory, applies the same type/owner/mode/output protections, and passes the
original bounded provider payload on standard input. A dispatch can select at
most one such rule. The fixed consumer has a 55-second process bound under the
owned provider ingress's 60-second timeout so the #676 admit/complete/reconcile
transaction is not forced through the ordinary five-second handler budget. The
policy cannot select another command, path, arguments, environment assignment,
or timeout through this capability.

## Deterministic evaluation

Rules select by product, event, and matcher, then sort by `(priority asc,
rule_id asc)`. A duplicate rule ID is invalid. Decision precedence is
`block > transform-conflict > transform > warn/context > allow`.

Multiple context values concatenate in rule order with a 16 KiB aggregate
limit. Provider-native rendering uses that complete aggregate even when the
first context rule supplied an otherwise reusable native envelope. Identical
replacements coalesce. Different replacements are an explicit
`transform-conflict` block; transforms never compose implicitly. Failure
posture is typed per rule (`open`, `warn`, or `closed`), while locked privacy,
writer, transaction, and recovery rules must be `closed`.

An executable capability is judged by its exit status and its output, never by
how much of its stdin it chose to read. A capability that exits before draining
the delivered payload closes the read end, and the runner's remaining write
returns `EPIPE`; that is the child's decision and is not a capability failure.
The runner returns promptly with the child's own result instead of blocking to
the timeout, and a capability that stops reading and then fails still takes its
rule's failure posture. Scoring the broken pipe as a failure made a `closed`
plus `locked` rule deny at random, because whether the child won the race
depended on machine load, and a `locked` rule offers no override to recover
with.

`agent-session.activity.v1` retains one terminal-only degradation boundary.
Failure to record a `Stop` observation returns the stable warning
`activity-stop-reconciliation-required` instead of denying provider
termination. This prevents a missing, stale, or temporarily unavailable
activity store from creating a non-terminating Stop-hook loop. The same
capability remains fail-closed for `UserPromptSubmit`, `PreToolUse`, and every
other non-terminal event, and the terminal warning does not release or alter
claims, leases, operations, brokers, worktrees, or session state. Any retained
activity uncertainty therefore remains visible for typed external
reconciliation after the provider runner exits. Codex `Stop` renders this
normalized warning as the provider-native neutral `{}` response because that
event does not support additional context; Claude `Stop` retains its supported
warning context.

## Interaction lanes and degradation

A capability that cannot prove itself must not admit arbitrary mutation, and it
must also not take the whole session with it. Every canonical event belongs to one
lane, and a typed runtime fault is handled per lane:

| Lane | Events | Behavior on a typed runtime fault |
| --- | --- | --- |
| conversation | `UserPromptSubmit`, `SessionStart` | degrade to read-only with `coordination-degraded-read-only` |
| terminal | `Stop`, `StopFailure` | terminal warning with reconciliation-pending evidence |
| mutation | every other event | unchanged fail-closed posture |

Only faults with a known recovery path are degradable:
`coordination-unavailable`, `coordination-untrusted`, `coordination-invalid`,
`runtime-version-skew`, and `activity-helper-unresolvable`. An unrecognized error
keeps the existing fail-closed handling rather than being degraded on a guess.

A degraded decision carries the fault code first, then the lane code, so
diagnostics keep the precise cause. Its human context states one primary
diagnosis and one safe next action instead of a comma-separated list of every
selected policy; the full reason list remains available in `--format json` and on
the `agent-session.observation.v1` plane.

Degrading the conversation lane acquires no new authority: the prompt is admitted
as text, every mutation stays gated, and the original prompt is neither lost nor
executed more than once because the turn correlation digest bounds replay.

The terminal lane has two degradation boundaries. A coordination transaction
that cannot run at all returns the compatibility classification
`coordination-stop-reconciliation-required`, but its diagnostic makes no
retained-state or gate claim and routes to read-only `agent-session broker
status`; without a warning boundary the first Stop delivery deadlocks before
provider re-entry metadata can help. Separately, when the provider reports Stop
re-entry and the aggregate still blocks, the decision becomes a warning:
re-entry proves the previous block changed nothing the gate awaits, so blocking
again only consumes the provider's consecutive-block budget.

Stop re-entry carries the typed coordination result into the terminal renderer;
the runtime handler supplies `runtime-kit.session-coordination-result.v1` with
one of `not-run`, `clean`, `pending`, or `unavailable`. Untyped provider payloads
are treated as unavailable, never as broker-state proof. The renderer does not
infer transaction state from generic provider actions or normalized reason
strings:

| Coordination result | Stable re-entry code | Disposition and recovery |
| --- | --- | --- |
| transaction explicitly reports a pending operation | `stop-reentry-reconciliation-pending` | `reconciliation-pending`; prescribe `agent-session broker reconcile` |
| transaction reports clean | `stop-reentry-terminal-exit` | `terminal-exit`; no recovery mutation |
| transaction did not run | `stop-reentry-terminal-exit` | `terminal-exit`; no coordination-state or gate claim |
| transaction result unavailable | `stop-reentry-terminal-exit` | `terminal-exit`; read-only `agent-session broker status`, with no retained-state or gate claim |

Every original reason is retained and no degradation releases or alters claims,
leases, operations, brokers, worktrees, or session state. A gate assertion is
made only for a reported pending operation in effective enforce mode; advisory,
off, skipped, and unavailable outcomes do not manufacture one.

## Release compatibility

A coordination registry written by a different release generation of the same
schema family is recoverable version drift, reported as `runtime-version-skew`
with a bounded `recovery_action` in its error details. A body that does not parse,
or that belongs to another schema family, remains `coordination-invalid`. Mutation
still fails closed on drift; the conversation lane degrades so the recovery can be
requested.

Broker records publish the release that created them. A minor or major difference
from the dispatching binary is recorded as `broker-release-skew` on the
observation plane with the peer release and its recovery action. A patch-level
difference and an unpublished release are not drift.

## Activity helper resolution

`AGENT_SESSION_BIN` outranks `PATH`, so a stale value inherited from a
long-lived tmux server survived the relocation the `PATH` pin was added to
prevent. An override that does not resolve to an executable regular file is
therefore treated as absent, and the daemon-pinned `PATH` decides instead. This is
a deliberate change to a fail-closed boundary rather than a downgrade: anyone able
to set `AGENT_SESSION_BIN` can already set `PATH`, and the pinned `PATH` is
daemon-controlled. The discarded override is recorded as
`activity-helper-unresolvable` so it cannot silently mask a misconfiguration, and
an empty assignment is the normalized "resolve through the pinned `PATH`" value.

Ingress registration is not capability health. When the selected policy binds
`agent-session.activity.v1`, `doctor` resolves the helper to an executable file
through the override or `PATH` before reporting an otherwise converged provider as
healthy, and fails with `activity-helper-unresolvable` when it cannot.

Rule modes are `enforce`, `shadow`, and `disabled`. Shadow evaluation records
only a redacted observation: it cannot affect exit status/output authority,
rewrite input, mutate capability/rule/session state, perform reclaim/adoption,
or consume recovery. Policy order and results are deterministic across
providers for overlapping capabilities.

Ordinary selected rules are evaluated and aggregated before the typed session
coordination phase. A blocking `PreToolUse` aggregate returns without invoking
coordination; an allowing aggregate invokes the exact #676 consumer and merges
its typed result without weakening the earlier decision. Terminal
`PostToolUse`, `PostToolUseFailure`, and `Stop` deliveries invoke the consumer
after ordinary aggregation even when that aggregate blocks, because an already
admitted operation must persist its outcome and complete or remain explicitly
reconcile-pending. Shadow evaluation never invokes the consumer.

One bounded pre-claim exception preserves that ordering without deadlocking a
managed worker. When a selected coordination rule exists, a Bash request is
exactly one canonical bare or absolute `main-agent bootstrap --idempotency-key
KEY --format json` invocation, and `owner-active-foreign` is the only blocking
ordinary result, the typed coordination consumer runs while that block remains
intact. Only its strict
`runtime-kit.session-coordination-bootstrap-authorization.v1` affirmative
authorization may replace that specific owner-liveness result. Empty or generic
allow output, malformed or unavailable coordination, timeout, explicit denial,
another blocking rule, or a transform preserves the original block. Shell
composition, wrappers, relative executable spellings, invalid keys,
non-canonical quoting or spacing, parameter/command/arithmetic expansion,
redirection, reordered or additional arguments, missing coordination authority,
and every non-bootstrap request remain subject to ordinary owner-liveness.

Override classes:

- `locked`: user config cannot change mode, priority, posture, or parameters.
- `downgrade-only`: config may move `enforce -> shadow -> disabled`, never in
  the other direction and never change typed parameters.
- `free`: config may choose any mode but still cannot add a command/capability
  or change the policy rule identity.

## Setup ownership and rollback

`agent-hook setup --product <provider>` is dry-run-first. `--apply`, `--repair`,
and `--remove` are mutually exclusive. Mutation requires an exact reviewed
plan digest when drift, compatibility, or dual ownership is present. Setup
parses and renders all files before mutation, locks by a stable provider-path
identity independent of `--state-dir`, re-reads every reviewed byte state,
writes atomically, and restores all prior bytes and file-presence states on any
partial or post-replacement failure. Symlinks, non-regular files, unsafe
permissions, concurrent drift, and malformed provider roots fail without
mutation. JSON provider configuration is decoded with recursive duplicate-key
rejection before every plan, apply, repair, or remove path.

For Codex, `config.toml` and compatibility `hooks.json` are one transaction.
The managed `config.toml` representation also owns the authoritative
`agent-session activity notify --agent codex` argv: a safe singular user
notifier is composed by direct argv, and remove restores it byte-for-byte.
An exact Computer Use `turn-ended --previous-notify <JSON argv>` wrapper is
treated as composed only when its bounded chain reaches that exact owned argv
and the helper is the executable regular file at the fixed path below the
active Codex config root, with no symlink in any relative path component.
Alternating Computer Use/agent-session wrapper growth is drift: preview does
not authorize mutation, and an exact reviewed plan digest is required to
normalize it to one Computer Use wrapper plus one owned notifier. This
normalization is semantic rather than byte-reversible. Remove performs no
helper execution and may structurally strip exact owned layers at the fixed
path even when the helper is missing or no longer executable; unknown shapes
retain the ordinary foreign-notifier rules.
Prior `agent-runtime-kit:hooks` marker lines are validated as an exact ordered
pair outside TOML multiline values. When Codex has saved trailing `[projects]`
or `[hooks.state]` trust tables inside that marker pair, setup classifies the
boundary as drift and an exact reviewed plan digest may move only the closing
marker ahead of the byte-preserved trust-table suffix. Ambiguous marker layouts,
noncanonical trust headers that cannot be moved byte-for-byte, and non-trust
TOML following that suffix fail closed before mutation.
The `agent-hook:provider-ingress:v1` ownership markers are recognized only as
an exact ordered pair of standalone lines outside basic and literal multiline
TOML values. Setup validates the complete foreign-manager marker layout before
the first-install no-owned path or any owned-span overlap classification; each
foreign owner may identify exactly one balanced range. An owned block wholly
inside a valid foreign-manager range is drifted and requires the exact reviewed
plan digest before it is moved outside that range. A foreign range inside or
partially crossing the owned span cannot be preserved by regeneration and fails
closed, as do orphaned, reversed, crossed, partial, or duplicate foreign markers
anywhere in the document.
Owned groups contain exactly one dispatcher command for each required
event/matcher. Install, upgrade, repair, remove, and rollback preserve
unrelated hooks, comments, formatting, provider metadata, and unsupported
surface truth. A no-op remove preserves both bytes and file presence; removal
deletes only an exact owned representation. The `--remove --dry-run` setup
combination is the removal-specific no-write preview. It returns
`action: "remove-dry-run"`, `changed: false`, `would_change`, `apply_allowed`,
and the reviewed `plan_digest`; `--remove --expected-plan-digest <digest>`
accepts that digest only while the exact before/after bytes and file-presence
plan remains current. Pre-dispatch
`agent-session activity setup` forwards to this API and cannot install a second
managed representation. Compatibility handlers are reported as compatibility-only or `dual`
until the reviewed migration removes them.
The exact historical `session-coordination-guard.py` command at its fixed
60-second timeout is recognized as compatibility state only when the selected
policy contains the typed coordination capability. The reviewed setup plan
removes that sibling handler and renders one 60-second `agent-hook dispatch`
entry in each required event/matcher group; arbitrary lookalikes remain
unrelated.

## Governed recovery

Recovery state paths are validated without following symlinks before any
chmod, creation, or write. A challenge loads the current strict policy and
binds the exact product, event, target digest, command
digest, snapshot digest, requested rules, scope, state revision, and expiry.
Authorization requires the same effective OS user, a private regular challenge
file, explicit digest confirmation, and a fresh state revision. It produces a
private capability file and a redacted audit record.

One-shot scope is consumed atomically once. Repair-window scope binds one
session principal, explicit target set, permitted locked rule IDs, a maximum
15-minute duration, and a monotonic state revision. It can be revoked early.
Capabilities fail closed on absence, ambiguity, expiry, replay, revocation,
permission/owner mismatch, product/event/session/target/command/snapshot drift,
target recreation, or key rotation. No environment variable or persistent
config enables bypass.

An exact capability may recover from malformed config or missing policy by
evaluating its signed, versioned rule manifest. Unknown/non-recoverable IDs are
rejected before authorization, and ungranted rules retain their action or
failure posture. It
does not bypass OS/provider authorization, unrelated hooks, nils-cli
transaction/privacy invariants, or a scope not explicitly bound in the
capability. Traces and output expose only a reason code and capability digest,
never the bearer or private identity.
The typed session coordination rule is present in the signed emergency
manifest but is never a recoverable rule ID. Config-independent recovery still
invokes it after the recovered aggregate allows, so break-glass cannot bypass
the #676 transaction.

## Coordination and writer liveness

The built-in `owner-liveness` capability lazily reads one bounded #676
coordination projection only when a selected enforced rule needs it. The
projection and heartbeat/fingerprint verifier live in `nils-common` and are
shared with the producer. Sidecar heartbeat evidence, not the registry's
projection timestamp, authenticates freshness. It validates ownership and permissions and
returns only public classifications. Active foreign writers and definite
semantic conflicts block. Stale clean ownership may be reclaimed atomically by
the owning coordination primitive. Dirty ownership requires governed adoption
or exact recovery. Orphaned and unknown evidence remain visible and
conservative. Compatibility state without broker evidence uses a documented bounded
compatibility TTL, never the historical eight-hour value as the primary
decision.

Crash, missing Stop, child exit, restart, stale heartbeat, key rotation,
concurrent contenders, and recreated-target evidence must not silently turn an
active/unknown owner into clear. Mailbox contents, raw session identity, host,
checkout path, and capability values never enter dispatch output or trace.

Semantic conflict is never accepted from a Codex or Claude payload field.
`agent-session.semantic-conflict.v1` derives its classification from the
private, owner/mode-validated #676 registry, the current managed principal's
ready/fresh broker incarnation, and active work-context claims. Exact worktree,
provider-reference, or overlapping scope evidence is definite; an incomplete
registry, missing authenticated current claim, stale peer universe, or only
repository-level uncertainty is `unknown`/`potential` and advisory. Only a definite authenticated conflict blocks. A forged provider field named
`semantic_conflict` is ignored and never upgrades or downgrades the derived
classification.

For a managed current principal, both coordination capabilities honor the
durable session mode. `advisory` downgrades coordination blocks to warnings,
`enforce` retains typed denial, and `off` does not participate. A downgrade is
accepted only when the private session record matches the current session and
runtime incarnation, its mode matches the fresh broker projection when one is
available, and any exported `AGENT_SESSION_COORDINATION_MODE` value agrees.
The environment value is a consistency hint, never downgrade authority by
itself. Missing, malformed, stale, or mismatched authority retains the
fail-closed/enforce behavior; a trusted durable advisory/off record may still
apply its non-denying failure posture when the registry is unavailable.

## Exit codes

- `0`: successful operation or provider allow/warn/context result.
- `1`: runtime failure or provider block.
- `64`: invalid CLI use.
- `65`: invalid config, policy, provider input, drift, or recovery data.
- `69`: required provider/setup resource or lock is temporarily unavailable.
- `75`: concurrency/lock contention suitable for bounded retry.
