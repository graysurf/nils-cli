# agent-hook

`agent-hook` is the shared policy control plane for Codex and Claude native
hooks and the out-of-tree DeepSeek Harness (DSH) runtime bundle. Provider
configuration contains only one dispatcher command per required event/matcher
group; versioned policy, deterministic aggregation, diagnostics, and governed
recovery live behind that ingress.

## Package and binary

| Field | Value |
| --- | --- |
| Package | `nils-agent-hook` |
| Binary | `agent-hook` |

## Configuration

The default user config is
`${XDG_CONFIG_HOME:-$HOME/.config}/agent-hook/config.toml`. It selects one
absolute, digest-pinned policy bundle:

```toml
schema_version = "agent-hook.config.v1"

[policy]
path = "/absolute/path/to/agent-hook/policies/current/policy.toml"
digest = "sha256:<64-lowercase-hex>"
```

Config and policy files are strict TOML. Unknown fields, untrusted paths,
unsupported capability IDs, invalid override authority, and digest drift are
rejected.

## Common commands

```bash
agent-hook validate --format json
agent-hook inventory --format json
agent-hook doctor --all --format json
agent-hook setup --product codex --dry-run --format json
agent-hook setup --product codex --apply --format json
agent-hook setup --product claude --remove --dry-run --format json
agent-hook setup --product claude --remove \
  --expected-plan-digest sha256:<digest> --format json
printf '%s' "$PROVIDER_HOOK_JSON" | agent-hook dispatch --product codex
printf '%s' "$DSH_INGRESS_JSON" | agent-hook dispatch --product dsh --format json
printf '%s' "$FINISH_LINE_OPEN_JSON" | agent-hook finish-line open
printf '%s' "$FINISH_LINE_JSON" | agent-hook finish-line begin --format json
printf '%s' "$FINISH_LINE_RUN_JSON" | agent-hook finish-line run --format json
printf '%s' "$FINISH_LINE_JSON" | agent-hook finish-line stop --format json
printf '%s' "$FINISH_LINE_JSON" | agent-hook finish-line status --format json
agent-hook completion zsh
```

Apply requires the preview `plan_digest` when compatibility or drifted managed state
is present. Setup preserves unrelated hooks and provider metadata, migrates
recognized pre-dispatch `agent-session`/runtime-kit handlers, and removes only exact
owned dispatcher entries. `--remove --dry-run` returns the
`remove-dry-run` action without writes; its digest binds the remove operation
and each provider file's exact before/after content or absence. DSH policy
dispatch is enforceable, but setup truthfully reports `unsupported` because the
external `dsh-runtime-kit` bundle owns the Cordis registration. Hermes policy
can be validated and inspected, but native setup also reports `unsupported`
until Hermes exposes a compatible runner.

DSH doctor output names `dsh-runtime-kit` as `registration_owner` and reports
`dispatch_supported: true` even though file-based setup remains unsupported.

DSH sends strict `agent-hook.dsh-ingress.v2` pre-tool JSON, v4 post-tool JSON,
and v3 lifecycle JSON.
V2 extends the retained v1 transport with the adapter-bound session, turn,
step, and absolute agent-docs state/config roots. V3 adds strict
`agent/pre-step` and `agent/turn-stopping` shapes, normalized to
`UserPromptSubmit` and `Stop`; the prompt is bounded to 64 KiB and only the
pre-step form may carry a known session-start source. The runtime bundle maps
allow/block decisions to tool admission and delivers bounded context through
native pre-step, post-tool, or steering messages. DSH rules cannot select
retired `runtime-kit.handler.v1` file handlers. The typed `dsh.policy.v1`
capability accepts the eleven Task 3.2 groups and nine Task 3.3 privacy,
memory, portable-output, health, skill, and pre-PR groups on their exact native
events. Task 3.4 adds metadata-only activity plus a deferred, locked operation
lifecycle. V4 carries the same call/tool identity as v2 and exactly one result
fact, `is_error`; candidate values, content, errors, and output never cross the
policy boundary. Managed mutations persist hashed correlation and private
retry material before invoking the same-release `agent-session work-context
show/admit/complete` commands. Exact retries do not duplicate admission or
execution; terminal post retries reauthenticate the idempotent completion.
Any partial managed selector set and uncertain operation fails closed. Stop
uses capability-authenticated `agent-session broker status` and permits exit
only when authoritative active and uncertain counts are both zero; local
records are retry caches, never Stop authority. An entirely unmanaged DSH
session is unchanged. Post-only lookups create no state; certain denials remove
their whole provisional directory. Terminal retries compact to 64 by a durable
monotonic completion sequence behind a private session lock, and a
128-directory ceiling blocks new admission rather than evicting active or
uncertain work. The default-delivery
evaluator pins the primary/default
branch consensus in owner-private state before admitting Bash or native file
mutation, rejects later Git metadata drift, and protects Git metadata from
native write/edit tools. Raw commit-producing rewrites are denied on the
default branch while exact recovery and owned `git-cli`, `forge-cli`, and
`semantic-commit` routes remain usable. Path-backed semantic commit messages
and ambiguous repeated scalar options fail closed.

The separate `finish-line` service exposes only five public operations: `open`,
`begin`, `run`, `stop`, and `status`. On a supported Linux containment host,
`open` requires a caller-generated private attempt token and derives a private
runner capability bound to that token, the exact repository, and the DSH
session. An exact retry returns the same capability and renews its 24-hour
lease; a different attempt cannot replace a live session capability. It fails
closed before state activation on other platforms. Each accepted incarnation
also receives a durable monotonic sequence in its capability derivation, so
tombstone compaction can never make an old bearer valid again. `begin`
durably advances the repository generation before an edit. `run` resolves the
caller's exact `intent` and
command against the current `agent-docs` `product = "dsh"` contracts. The DSH
integration probes every foreground Bash command with `run` before execution
and rejects background Bash at the plugin boundary. A probe omits `execution`:
an exact validation target returns `ready`, while any other command returns
`ordinary-ready`.

The follow-up `run` carrying `execution.kind = "bash-v1"` is the only executor.
For an exact validation target, nils reserves the attempt, launches the exact
command, observes its exit, signal, timeout, and bounded output, and records
only nils-derived validation evidence. For an ordinary foreground command,
nils first advances the shared repository generation, then launches the
command in its canonical requested working directory and returns
`ordinary-applied`; this execution never creates validation evidence. Exact
terminal retries are idempotent and do not re-execute. The caller cannot submit
or override an outcome.

On Linux, every exact or ordinary execution uses the trusted fixed
`/usr/bin/systemd-run` and `/usr/bin/systemctl` paths and a transient user unit.
`open` first verifies those binaries, unified cgroup v2, enabled unprivileged
user namespaces, and a responsive systemd user manager.
Nils serializes the runner configuration into an immutable sealed memfd.
Systemd `OpenFile` hands the unit the exact current agent-hook executable inode
as descriptor 3, the sealed config as descriptor 4, and an unlinked control
memfd as descriptor 5. A verified root-owned ELF interpreter loads the runner
through `/proc/self/fd/3`; the contained runner checks the exact systemd
descriptor names/count and the config's metadata and memfd seals before reading
it. The runner marks descriptor 5 close-on-exec and makes itself non-dumpable,
then emits a nonce-bound ready record. The supervisor acknowledges only after it
has closed the config, changed control mode to `0400`, and dropped its writable
control descriptor. It also becomes non-dumpable before acknowledging; only
then may the runner launch the provider. The runner clears the acknowledgement,
writes and immediately seals the strict terminal result. After unit quiescence,
the supervisor reads the already immutable result. Provider exits 134 and 204
therefore remain ordinary exits, while provider signals remain canonical
`NodeJS.Signals` facts rather than internal runner sentinels.
It also holds a pidfd for the original nils supervisor and kills its workload if
that supervisor disappears.

The unit uses a private user namespace, control-group kill semantics with
immediate `SIGKILL`, a bounded `RuntimeMax`, no `AF_UNIX`, and denied localhost
access. Nils does not record the result until `systemctl` reports the unit
inactive or failed.
This tracks descendants that call `setsid` or double-fork and denies the tested
parent-cgroup/user-manager escape paths. It is not a general network namespace
or a guarantee against every network or IPC delegation mechanism. Authoritative
execution fails closed on non-Linux platforms or when the trusted systemd
boundary is unavailable.

For DSH cancellation or failed-execution cleanup, a pending operation durably
retains its exact transient unit identifier; a `run` error does not discard that
handle. The integration may call the hidden internal `finish-line quiesce` RPC
with the same private capability and operation binding. Nils performs bounded
`systemctl` stop/status calls plus an exact `list-jobs` barrier. It requires
three consecutive observations, 25 ms apart, with no pending job and either an
absent unit or an inactive/failed unit whose extant cgroup reports
`populated 0`. Only then does it clear pending state. `quiesce` is absent from
public help and completion and cannot report a validation outcome. After
`agent/disposed`, the integration calls the hidden authenticated `finish-line
release` RPC. Release refuses pending execution, removes terminal state for the
exact session, retains a bounded capability-digest tombstone so a recent
ambiguous response can be retried, and advances the persisted monotonic
incarnation sequence before any later open.
The tombstone retires that capability incarnation, not the stable DSH session
identity: a later rc.7 resume can open a new private incarnation, while an old
release retry stays duplicate and cannot delete the new session state. The
attempt token is private idempotency material for a live incarnation, not an
authorization bearer or permanent revocation identity. Reusing it after
release therefore opens a new sequence-bound capability whose bytes differ
from the retired bearer; the old capability cannot authorize that session even
after its bounded duplicate-release tombstone ages out.
Coordinator crashes cannot permanently consume the live-session bound:
under live-session capacity pressure, one expired quiescent capability lease
and its terminal evidence may be conservatively reclaimed, which makes stop
block until revalidation. Lease expiry alone never removes session or pending
state. A persisted cursor rotates each bounded recovery window so busy older
entries cannot permanently starve a later reclaimable entry. A busy expired
crash orphan is reclaimed only when every operation
retains an exact valid transient-unit identity and bounded stop/status,
`list-jobs`, and cgroup checks prove those units stably quiescent. Active, indeterminate,
unbound, or migrated pending sessions remain protected. Both capability open
and a new session's first edit admission use this bounded recovery path.

`stop`
exits `1` with a successful service envelope and `action = "block"` until every
exact command target succeeded for the current repository generation and
session; `status` exposes only bounded redacted state. There is no `complete`,
`waive`, `approve`, or `revoke` finish-line operation, and no waiver path or
ambient waiver environment variable.

Finish-line commands default to service JSON. Both `open` and `begin` require a
caller-generated unpredictable `attempt_token`; responses never echo it and
state stores neither token. An exact retry safely recovers a lost success
response. The capability returned by `open` is the only private bearer
accepted by `run`, `quiesce`, and `release`; it must stay out of logs and
persistent configuration.

Every request must name the exact canonical Git root resolved for the running
process; nested and unrelated repositories are rejected. Every successful
response carries an opaque repository/session `correlation_id` that the DSH
consumer must keep constant across one lifecycle. Terminal history is
deterministically compacted while pending attempts, current enforcement facts,
and a bounded recent idempotency window remain intact.

Finish-line state lives below
`${XDG_STATE_HOME:-$HOME/.local/state}/agent-hook/finish-line/` (or below the
global `--state-dir` override). It contains only digests and normalized facts:
never raw commands, output, repository paths, session/turn/operation IDs,
tokens, or runner capabilities. A pending execution retains only its generated
unit identifier needed for cleanup. Contained runner configuration and the
nonce-bound control result are delivered through anonymous memfds rather than
path-backed files. See the v1 contract for the wire shapes, exit codes,
crash-safety rules, and bounds.

The public `DshCapabilityGroup` schema and its checked-in
`agent-hook.dsh-policy-capability-groups.v1` fixture freeze the 23 deterministic
policy groups selected for the DSH migration. `dsh.policy.v1` now implements
the eleven Task 3.2 Git, delivery, scope, ownership, lease, and edit-admission
groups, nine Task 3.3 privacy/context groups, and the two Task 3.4 activity and
operation-lifecycle groups without executing retired handler files. Policy
child processes clear ambient credentials; the DSH adapter restores only the
managed session ID, runtime ID, helper path, capability-file path, and state
path needed by these two capabilities, never an ambient bearer. The scope-lock
companion also uses a fixed trusted Git
with repository helpers, hooks, pagers, external diff, and text conversion
disabled. Command-dependent gates fail closed after shell state changes that
can retarget cwd, exported variables, tracing hooks, command lookup, or aliases,
and on Git options/subcommands that consume nested commands. Protected default
delivery also classifies fetch destinations and denies stdin/server Git
ref-update plumbing. Planning-only groups stay invalid until their typed
evaluators and lifecycle contracts are delivered.

Before `doctor` reports an enforceable provider as `converged`, it resolves
every script-backed handler selected by that product's policy and applies the
same regular-file, executable, effective-user-owner, and non-group/world-writable
trust check used immediately before dispatch. Missing or unsafe handlers return
typed `handler-unavailable` or `handler-untrusted` errors.

Codex `config.toml`, compatibility `hooks.json`, the managed dispatcher, and
the authoritative `agent-session activity notify --agent codex` argv are one
reviewed transaction. A singular safe user notifier is composed without a
shell; rollback and remove restore the exact prior bytes and file-presence
state. An audited Codex Computer Use `turn-ended --previous-notify` wrapper
that reaches the exact owned notifier is also composed. Accumulated alternating
wrappers are drift: dry-run keeps `apply_allowed: false`, and only the matching
plan digest may normalize them to one Computer Use wrapper plus one owned
notifier. The successful repair reports `apply_allowed: true`, repeat repair is
a no-op, and remove restores one semantic Computer Use base notifier.

The locked `agent-session.coordination.v1` capability runs inside that same
dispatcher ingress. Ordinary policy aggregation runs first; only an allowed
mutation is admitted, while terminal PostTool/Stop delivery still completes or
preserves reconciliation for an already admitted operation. Its fixed
`session-coordination-guard.py` consumer cannot be replaced through config,
shadow never invokes it, and governed recovery cannot bypass the underlying
issue #676 transaction.

`agent-session.activity.v1` emits only a normalized metadata event to
`agent-session activity event`; it never forwards raw provider JSON. Shadow
evaluation skips every side-effecting capability. If that activity update is
temporarily unavailable, only the finite, shell-uncomposed Main Agent
rehydration, recovery, status, rebind, and bootstrap shapes may defer the
activity failure to a selected locked `agent-session.coordination.v1`
transaction. This deferral is shape evidence, not authority: the fixed
coordination consumer must still authenticate the exact release, private
capability, owner/claim state, and command, and any missing or failed consumer
keeps the request fail-closed. Ordinary shell commands and near-miss recovery
forms never receive this deferral.

Recovery uses a private challenge/authorize/consume lifecycle. Capability
files are exact, expiring, state-bound bearers. Each challenge binds a signed
rule manifest, so recovery from an unavailable config or policy still evaluates
all ungranted rules instead of becoming a global allow. Bearers must never be
printed or placed in persistent config.

See the [v1 contract](docs/specs/agent-hook-v1.md) for schemas, limits,
aggregation, setup ownership, liveness, and recovery invariants.
