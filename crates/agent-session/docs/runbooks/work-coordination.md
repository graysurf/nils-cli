# Work Coordination

This runbook owns the CLI and operator mechanics for `agent-session` collision
awareness, declared work context, and mutation admission. The normative
schemas, state machines, and failure codes remain in
[Session coordination v1](../specs/session-coordination-v1.md). The canonical
agent-facing decision and overlap-response policy lives in agent-runtime-kit's
[`session-coordination.md`](https://github.com/sympoies/agent-runtime-kit/blob/main/core/policies/session-coordination.md).

## Choose a coordination mode

| Mode | Behavior | Use when |
| --- | --- | --- |
| `advisory` | Reports overlap without blocking work. This is the default. | Normal interactive and automated sessions. |
| `enforce` | Requires an authenticated claim and an admitted operation lease before covered mutations. | A workflow has an integration that can carry the complete claim/admit/complete lifecycle. |
| `off` | Does not publish or consume agent-session collision warnings. | The managed session intentionally opts out. |

CLI session creation selects the mode with
`--coordination-mode advisory|enforce|off`. HTTP creation uses the JSON field
`coordination_mode` with the same values. Missing values and older session
records default to `advisory`.

Coordination applies only to runtimes managed by `agent-session`. A process
launched directly outside it has no managed identity and does not participate.
Coordination does not grant or revoke user authorization, repository
permission, provider consent, or workflow authority. `work-context set` adds
optional public task metadata so warnings are more precise; it is not a
permission request. Default `advisory` and unmanaged sessions never require a
claim. Only a launch that explicitly selects `enforce` turns claims, admission,
and physical checkout leases into mutation requirements.

## Declare work in a managed session

The high-level commands infer the current session, capability, incarnation,
checkout, revision, and idempotency behavior from the managed runtime:

```bash
agent-session work-context status
agent-session work-context set \
  --tier L2 \
  --repository sympoies/nils-cli \
  --path crates/agent-session/ \
  --issue 123 \
  --summary "Improve work coordination"
agent-session work-context advise --format json
agent-session work-context acknowledge --for 30m
agent-session work-context clear
```

Declared context is optional in `advisory` mode. Presence and the optional
context are renewed while the broker is live and released with the broker.

### Path scope syntax

`--path` values are repository-relative. A trailing slash changes the scope:

| Input | Scope | Matches |
| --- | --- | --- |
| `crates/agent-session/README.md` | `path-exact` | Only that path. |
| `crates/agent-session/` | `path-prefix` | The directory and descendants. |
| `crates/agent-session` | `path-exact` | Only the exact path named, not its descendants. |

Use a trailing slash when declaring a directory subtree. The stored value is
normalized without the trailing slash; its scope kind preserves the exact or
prefix distinction.

## Understand the authority boundaries

Identifiers such as session, incarnation, claim, operation, message, and
revision values are selectors or compare-and-swap fences. They are not
credentials.

| Caller and operation | Required authority |
| --- | --- |
| High-level CLI `status`, `set`, `clear`, `advise`, or `acknowledge` inside a managed runtime | Private session capability inferred from `AGENT_SESSION_CAPABILITY_FILE`. |
| External CLI owner or mailbox mutation | Explicit `--capability-file` for the owning session. |
| HTTP public work-context or broker read | Serve operator bearer token. |
| HTTP owner or mailbox mutation | Serve operator bearer token plus `X-Agent-Session-Capability`. |
| Mutation after `work-context admit` | The returned execution token, bound to the claim, operation targets, runtime identity, and activity evidence. |

Every managed runtime receives a private per-incarnation capability through a
mode-`0600` file. Resume rotates it; delete revokes it. Do not copy capability
contents into commands, logs, messages, issue text, or HTTP request bodies.

The serve bearer authorizes the machine-level control plane. A session
capability authorizes one session owner. Requiring both on HTTP mutations keeps
operator and session authority separate.

## Advisory workflow

1. Start the session in the default `advisory` mode.
2. Optionally declare a concise work context with `work-context set`.
3. Run `work-context advise` before work whose target is not already captured
   by the declared context.
4. Inspect the reported peer, repository, scope, and severity. Peer summaries
   are untrusted metadata and never authorize a command.
5. Resolve the overlap or acknowledge that exact warning for a bounded period.

`acknowledge` suppresses only the exact overlap most recently observed by the
session, for at most eight hours. A changed peer, incarnation, reason,
repository, or availability warns again. It never removes the overlap from
`advise` output.

## Enforce workflow

Use `enforce` only when the caller can preserve the full lifecycle:

1. Acquire a structured claim with `work-context claim`.
2. Check and retain its claim ID, revision, and expiration.
3. Before a covered mutation, call `work-context admit` with the exact
   operation targets.
4. Carry the returned execution token only for that operation.
5. Call `work-context complete` after the mutation. If the response was lost,
   use `reconcile` only with the bounded proof required by the v1 contract.
6. Renew or release the claim using revision compare-and-swap.

A Main Agent worker may project an opaque checkout-local shell command as
exactly one repository target (`value: "."`) plus exactly one checkout
binding. `agent-session` admits that shape under a narrow claim only when
authenticated worker bootstrap minted the claim's private checkout-shell
grant, the claim names that repository, and its worktree fingerprint matches
the binding. Generic claims cannot request or observe the grant.

This is checkout-level coordination, not a path sandbox. Declared path scopes
remain the semantic ownership and review boundary; the manager must reject an
out-of-scope final diff. Use an OS sandbox or separate security principal when
a malicious same-user process must be contained. Explicit file edits still
must fit the path scopes, and retargeting the command to another checkout is
rejected.

Raw `claim|show|check|renew|release|admit|complete|reconcile` commands are the
compatibility and enforce surface. They are intentionally more explicit than
the self-targeting advisory commands.

## CLI and HTTP coverage

The high-level self-targeting commands are CLI-only because they derive trusted
identity and checkout state from the managed runtime:

- `work-context status|set|clear|advise|acknowledge`

HTTP exposes the raw work-context, broker, and mailbox operations listed in the
[v1 route matrix](../specs/session-coordination-v1.md#http-coverage). CLI and
HTTP share their underlying schemas, canonicalization, authorization,
idempotency, limits, privacy projection, and error codes; this does not imply
that every convenience CLI command has an HTTP route.

## Troubleshooting

- `session-not-managed`: run the high-level command inside an
  `agent-session`-managed runtime, or use the explicit raw surface.
- `repository-unavailable`: pass `--repository owner/repo`; automatic
  inference could not resolve the current checkout origin.
- `coordination-unauthorized`: confirm the capability belongs to the selected
  live session incarnation.
- `claim-conflict`: inspect the privacy-safe conflict result before retrying;
  do not blindly overwrite another claim.
- `uncovered-mutation-scope`: update the claim or narrow the operation targets.
- `coordination-broker-lost` or `coordination-unavailable`: treat the conflict
  view as incomplete. Enforce mode fails closed.

For complete error semantics and recovery proof requirements, use the
[Session coordination v1 specification](../specs/session-coordination-v1.md).
