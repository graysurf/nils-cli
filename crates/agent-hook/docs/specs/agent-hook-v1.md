# agent-hook v1 contract

Status: frozen for `graysurf/agent-runtime-kit#686` Lane A.

Ownership: crate-local canonical specification for `nils-agent-hook` and the
`agent-hook` binary.

## Purpose and boundary

`agent-hook` is the sole owner of nils-cli-managed Codex and Claude hook
registrations. Provider-native hooks remain lifecycle ingress, but their owned
commands contain only `agent-hook dispatch --product <provider>`; policy rules
never live in provider configuration. The CLI does not manage unrelated hooks,
execute config-defined programs, weaken lower-level transaction/privacy rules,
or claim native enforcement for providers without a compatible runner.

## Files and limits

The CLI resolves only absolute XDG roots and rejects relative roots.

| Role | Default | Limit / mode |
| --- | --- | --- |
| user config | `${XDG_CONFIG_HOME:-$HOME/.config}/agent-hook/config.toml` | 64 KiB, regular file, no symlink |
| policy bundle | config-selected absolute path, normally below `${XDG_DATA_HOME:-$HOME/.local/share}/agent-hook/policies/` | 1 MiB, regular file, no symlink |
| runtime state | `${XDG_STATE_HOME:-$HOME/.local/state}/agent-hook/` | directories `0700`, secret files `0600` |
| provider payload | standard input | 1 MiB |
| trace | state-owned JSONL | 256 entries, 256 KiB, redacted metadata only |
| rule inventory | one policy bundle | 512 rules, unique IDs |
| reason metadata | one aggregate decision | 64 reasons, 256 bytes per code/message |

Unknown fields are rejected in every persisted/input schema. Identifiers are
ASCII kebab-case, at most 128 bytes. Policy/config digests are lowercase
`sha256:<64 hex>` values over canonical serialized bytes.

## Versioned schemas

Every schema has a literal `schema_version` and uses strict unknown-field
handling.

- `agent-hook.config.v1`: selected policy path and digest, per-provider mode,
  and rule overrides. It cannot contain commands, bearer material, environment
  assignments, executable paths, or recovery state.
- `agent-hook.policy.v1`: bundle ID/version and ordered typed rules. A rule has
  stable ID, products, events, optional matcher, priority, mode, failure
  posture, override class, and one built-in capability binding.
- `agent-hook.normalized-request.v1`: request ID, product, canonical event,
  optional matcher, bounded target/command/snapshot digests, and public boolean
  facts. Raw prompts, tool input, command strings, paths, session IDs, mailbox
  bodies, environment values, and provider payload fragments are never stored.
- `agent-hook.normalized-decision.v1`: aggregate action, ordered reason codes,
  optional bounded context or replacement, shadow observations, and config /
  policy digests.
- `agent-hook.trace.v1`: timing, rule IDs, disposition classes, and digests;
  never raw payload, paths, identities, message content, or capabilities.
- `agent-hook.setup-plan.v1`: product, action, owned event/matcher groups,
  before/after digests, exact plan digest, drift state, and whether apply is
  permitted. Provider hook argv content is omitted.
- `agent-hook.doctor.v1`: product status (`missing`, `legacy`, `dual`,
  `drifted`, `converged`, `unsupported`, or `unrelated`), owned counts,
  legacy residue count, digests, policy availability, and recovery health.
- `agent-hook.inventory.v1`: ordered public rule metadata and effective mode;
  capability parameters and private paths are omitted.
- `agent-hook.recovery-challenge.v1`: random challenge ID, exact product/event,
  target/command/snapshot digests, requested rule IDs, scope, issue/expiry time,
  and state revision.
- `agent-hook.recovery-capability.v1`: capability ID, challenge digest, exact
  binding, scope, expiry, nonce, state revision, and authorization proof digest.
  The capability file is the bearer and is never printed.
- `agent-hook.owner-liveness.v1`: only classification (`active`, `stale`,
  `orphaned`, `unknown`, or `unclaimed`), semantic conflict class, and
  content-free reason codes.

Service JSON uses the workspace envelope: `schema_version`, `command`, `ok`,
then `result`/`results`; failures contain `error.code`, `error.message`, and
optional redacted `error.details`. Text is the human default except `dispatch`,
whose default is provider output so the documented ingress command is usable.
`--format json` always selects the service envelope.

## Provider normalization and rendering

Products are `codex`, `claude`, and `hermes`. Codex and Claude are enforceable;
Hermes validates and evaluates shared policy but reports `unsupported` for
native setup until a compatible runner exists.

Supported canonical events are:

- Codex: `UserPromptSubmit`, `PermissionRequest`, `PreToolUse`, `PostToolUse`,
  `PostToolUseFailure`, `Stop`, `StopFailure`, and `Notification`.
- Claude: the Codex set plus `SubagentStop`, `Elicitation`, and
  `ElicitationResult`.

The adapter accepts the event from `--event` or the provider's documented
`hook_event_name`/`event` field and rejects a mismatch. Matcher values are
normalized only from documented provider fields. Oversized input, invalid
UTF-8/JSON, unsupported events, duplicate object keys, or unknown normalized
fields fail according to the matching rule's declared failure posture; when no
rule can be loaded they fail closed except for an exact valid recovery
capability.

Provider rendering maps one normalized aggregate result to the provider's
native allow, warning/context, block, or input-replacement shape and stable exit
behavior. `dispatch --format json` returns the normalized decision envelope and
does not return raw provider content.

## Deterministic evaluation

Rules select by product, event, and matcher, then sort by `(priority asc,
rule_id asc)`. A duplicate rule ID is invalid. Decision precedence is
`block > transform-conflict > transform > warn/context > allow`.

Multiple context values concatenate in rule order with a 16 KiB aggregate
limit. Identical replacements coalesce. Different replacements are an explicit
`transform-conflict` block; transforms never compose implicitly. Failure
posture is typed per rule (`open`, `warn`, or `closed`), while locked privacy,
writer, transaction, and recovery rules must be `closed`.

Rule modes are `enforce`, `shadow`, and `disabled`. Shadow evaluation records
only a redacted observation: it cannot affect exit status/output authority,
rewrite input, mutate capability/rule/session state, perform reclaim/adoption,
or consume recovery. Policy order and results are deterministic across
providers for overlapping capabilities.

Override classes:

- `locked`: user config cannot change mode, priority, posture, or parameters.
- `downgrade-only`: config may move `enforce -> shadow -> disabled`, never in
  the other direction and never change typed parameters.
- `free`: config may choose any mode but still cannot add a command/capability
  or change the policy rule identity.

## Setup ownership and rollback

`agent-hook setup --product <provider>` is dry-run-first. `--apply`, `--repair`,
and `--remove` are mutually exclusive. Mutation requires an exact reviewed
plan digest when drift, legacy, or dual ownership is present. Setup parses and
renders all files before mutation, locks provider state, re-reads the reviewed
before digest, writes atomically, and restores the prior bytes on partial
failure. Symlinks, non-regular files, unsafe permissions, concurrent drift, and
malformed provider roots fail without mutation.

Owned groups are marked and contain exactly one dispatcher command for each
required event/matcher. Install, upgrade, repair, remove, and rollback preserve
unrelated hooks, comments, formatting, provider metadata, and unsupported
surface truth. Removal deletes only an exact owned representation. Legacy
`agent-session activity setup` forwards to this API and cannot install a second
managed representation. Legacy handlers are reported as `legacy` or `dual`
until the reviewed migration removes them.

## Governed recovery

Recovery bootstrap resolves and validates state before loading ordinary config
or policy. A challenge binds the exact product, event, target digest, command
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

An exact capability may recover from malformed config or missing policy. It
does not bypass OS/provider authorization, unrelated hooks, nils-cli
transaction/privacy invariants, or a scope not explicitly bound in the
capability. Traces and output expose only a reason code and capability digest,
never the bearer or private identity.

## Coordination and writer liveness

The built-in `owner-liveness` capability reads the #676 coordination registry
and heartbeat evidence read-only. It validates ownership and permissions and
returns only public classifications. Active foreign writers and definite
semantic conflicts block. Stale clean ownership may be reclaimed atomically by
the owning coordination primitive. Dirty ownership requires governed adoption
or exact recovery. Orphaned and unknown evidence remain visible and
conservative. Legacy state without broker evidence uses a documented bounded
compatibility TTL, never the historical eight-hour value as the primary
decision.

Crash, missing Stop, child exit, restart, stale heartbeat, key rotation,
concurrent contenders, and recreated-target evidence must not silently turn an
active/unknown owner into clear. Mailbox contents, raw session identity, host,
checkout path, and capability values never enter dispatch output or trace.

## Exit codes

- `0`: successful operation or provider allow/warn/context result.
- `1`: runtime failure or provider block.
- `64`: invalid CLI use.
- `65`: invalid config, policy, provider input, drift, or recovery data.
- `75`: concurrency/lock contention suitable for bounded retry.

