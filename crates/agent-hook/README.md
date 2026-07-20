# agent-hook

`agent-hook` is the shared policy control plane for Codex and Claude native
hooks. Provider configuration contains only one dispatcher command per required
event/matcher group; versioned policy, deterministic aggregation, diagnostics,
and governed recovery live behind that ingress.

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
agent-hook completion zsh
```

Apply requires the preview `plan_digest` when compatibility or drifted managed state
is present. Setup preserves unrelated hooks and provider metadata, migrates
recognized pre-dispatch `agent-session`/runtime-kit handlers, and removes only exact
owned dispatcher entries. `--remove --dry-run` returns the
`remove-dry-run` action without writes; its digest binds the remove operation
and each provider file's exact before/after content or absence. Hermes policy
can be validated and inspected, but native setup truthfully reports
`unsupported` until Hermes exposes a compatible runner.

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
evaluation skips every side-effecting capability.

Recovery uses a private challenge/authorize/consume lifecycle. Capability
files are exact, expiring, state-bound bearers. Each challenge binds a signed
rule manifest, so recovery from an unavailable config or policy still evaluates
all ungranted rules instead of becoming a global allow. Bearers must never be
printed or placed in persistent config.

See the [v1 contract](docs/specs/agent-hook-v1.md) for schemas, limits,
aggregation, setup ownership, liveness, and recovery invariants.
