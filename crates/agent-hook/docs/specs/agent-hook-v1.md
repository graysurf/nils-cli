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
  `PostToolUse`, `PostToolUseFailure`, `PreCompact`, `PostCompact`,
  `SubagentStart`, `SubagentStop`, and `Stop`.
- Claude: `SessionStart`, `UserPromptSubmit`, `PermissionRequest`, `PreToolUse`,
  `PostToolUse`, `PostToolUseFailure`, `PreCompact`, `Stop`, `StopFailure`,
  `Notification`, `SubagentStart`, `SubagentStop`, `Elicitation`, and
  `ElicitationResult`.

The adapter accepts the event from `--event` or the provider's documented
`hook_event_name`/`event` field and rejects a mismatch. Matcher values are
normalized only from documented provider fields:

| Provider events | Matcher input field |
| --- | --- |
| Codex/Claude `SessionStart` | `source` |
| Codex/Claude `PermissionRequest`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure` | `tool_name` |
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
return raw provider content.

Policy validation also checks the complete provider/event/capability binding,
not only each component in isolation. `decision.warn.v1` and
`decision.context.v1` require an event with native model-context semantics;
`decision.block.v1` requires native block, continuation, feedback, or decline
semantics; and `decision.transform.v1` is limited to Codex `PreToolUse` plus
Claude `PreToolUse`, `PermissionRequest`, and `PostToolUse`.
`agent-session.owner-liveness.v1` and
`agent-session.semantic-conflict.v1` require both context and block semantics
because their result is data-dependent. Neutral `decision.allow.v1`, metadata
side effects through `agent-session.activity.v1`, and trusted provider-native
`runtime-kit.handler.v1` remain valid for every supported event. This preserves
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
limit. Identical replacements coalesce. Different replacements are an explicit
`transform-conflict` block; transforms never compose implicitly. Failure
posture is typed per rule (`open`, `warn`, or `closed`), while locked privacy,
writer, transaction, and recovery rules must be `closed`.

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
