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
  posture, override class, and one built-in capability binding. The concrete
  TOML serialization is frozen below.
- `agent-hook.normalized-request.v1`: request ID, product, canonical event,
  optional matcher, bounded target/command/snapshot digests, and public boolean
  facts. Raw prompts, tool input, command strings, paths, session IDs, mailbox
  bodies, environment values, and provider payload fragments are never stored.
- `agent-hook.normalized-decision.v1`: aggregate action, ordered reason codes,
  optional bounded context or replacement, shadow observations, and config /
  policy digests.
- `agent-hook.trace.v1`: timing, rule IDs, disposition classes, and digests;
  never raw payload, paths, identities, message content, or capabilities.
- `agent-hook.setup-plan.v2`: product, owned event/matcher groups, role-tagged
  before/after digests for every provider source, exact plan digest, drift
  state, and whether apply is permitted. Provider hook argv content is omitted.
- `agent-hook.doctor.v1`: product status (missing, compatibility-only, `dual`,
  `drifted`, `converged`, `unsupported`, or `unrelated`), owned counts,
  compatibility residue count, digests, policy availability, and recovery health.
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

Service JSON uses the workspace envelope: `schema_version`, `ok`, then `data`;
failures contain `error.code`, `error.message`, and
optional redacted `error.details`. Text is the human default except `dispatch`,
whose default is provider output so the documented ingress command is usable.
`--format json` always selects the service envelope.

## Provider normalization and rendering

Products are `codex`, `claude`, and `hermes`. Codex and Claude are enforceable;
Hermes validates and evaluates shared policy but reports `unsupported` for
native setup until a compatible runner exists.

Supported canonical events are:

- Codex: `SessionStart`, `UserPromptSubmit`, `PermissionRequest`, `PreToolUse`,
  `PostToolUse`, `PreCompact`, `PostCompact`, `SubagentStart`, `SubagentStop`,
  and `Stop`.
- Claude: `SessionStart`, `UserPromptSubmit`, `PermissionRequest`, `PreToolUse`,
  `PostToolUse`, `PostToolUseFailure`, `PreCompact`, `Stop`, `StopFailure`,
  `Notification`, `SubagentStart`, `SubagentStop`, `Elicitation`, and
  `ElicitationResult`.

The adapter accepts the event from `--event` or the provider's documented
`hook_event_name`/`event` field and rejects a mismatch. Matcher values are
normalized only from documented provider fields. A policy matcher is either
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

Provider rendering maps one normalized aggregate result to the provider's
native allow, warning/context, block, or input-replacement shape and stable exit
behavior. `dispatch --format json` returns the normalized decision envelope and
does not return raw provider content.

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
- `runtime-kit.handler.v1` (`handler_id` from the compiled v1 allowlist)

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
output/exit semantics. One dispatch starts at most 16 executable capabilities,
retains at most 512 KiB of aggregate child output, and shares a two-second
absolute deadline. Each child leads an isolated process group; timeout or
direct-child exit terminates descendants before bounded pipe draining. The
policy cannot specify a path,
interpreter, argv, environment assignment, shell fragment, timeout, or digest.
Shadow mode never invokes it. Adding another handler requires a new nils-cli
release or a new versioned capability ID.

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
plan digest when drift, compatibility, or dual ownership is present. Setup
parses and renders all files before mutation, locks by a stable provider-path
identity independent of `--state-dir`, re-reads every reviewed byte state,
writes atomically, and restores all prior bytes and file-presence states on any
partial or post-replacement failure. Symlinks, non-regular files, unsafe
permissions, concurrent drift, and malformed provider roots fail without
mutation.

For Codex, `config.toml` and compatibility `hooks.json` are one transaction.
The managed `config.toml` representation also owns the authoritative
`agent-session activity notify --agent codex` argv: a safe singular user
notifier is composed by direct argv, and remove restores it byte-for-byte.
Owned groups contain exactly one dispatcher command for each required
event/matcher. Install, upgrade, repair, remove, and rollback preserve
unrelated hooks, comments, formatting, provider metadata, and unsupported
surface truth. A no-op remove preserves both bytes and file presence; removal
deletes only an exact owned representation. Pre-dispatch
`agent-session activity setup` forwards to this API and cannot install a second
managed representation. Compatibility handlers are reported as compatibility-only or `dual`
until the reviewed migration removes them.

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

## Exit codes

- `0`: successful operation or provider allow/warn/context result.
- `1`: runtime failure or provider block.
- `64`: invalid CLI use.
- `65`: invalid config, policy, provider input, drift, or recovery data.
- `69`: required provider/setup resource or lock is temporarily unavailable.
- `75`: concurrency/lock contention suitable for bounded retry.
