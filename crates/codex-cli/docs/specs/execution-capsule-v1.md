# Execution Capsule v1

## Purpose

An Execution Capsule packages one operator-reviewable shell script with the
metadata needed to run it either directly or through a Codex supervisor. It is
for cases where an agent cannot perform an authorized local operation in its
current environment, or where the operator wants Codex to monitor, diagnose,
retry, validate, and report the operation.

The two launch paths execute the same `run.sh`:

```sh
# Direct operator run
bash /absolute/path/to/capsule/run.sh

# Governed supervised run
codex-cli agent run --capsule /absolute/path/to/capsule

# Explicit operator-authorized host run
codex-cli agent run \
  --capsule /absolute/path/to/capsule \
  --allow-host-access

# Explicit full-home compatibility when an MCP-backed diagnosis is required
codex-cli agent run \
  --capsule /absolute/path/to/capsule \
  --mcp-mode inherited
```

The host route expands filesystem access only. It does not waive repository
policy, instructions, hooks, signing, delivery rules, or concurrency guards.

## Capsule layout and permissions

Create capsules under a private `agent-out project` directory, never in the
repository:

```text
<capsule>/                 0700
  manifest.json            0600
  run.sh                   0700
```

The runner creates or replaces these owner-only artifacts:

```text
  result.schema.json       0600
  events.jsonl             0600
  final.json               0600
  receipt.json             0600
```

Group or other access is rejected. In particular, mode `0775` is invalid for a
capsule directory or file; it makes prepared instructions and execution logs
mutable or readable outside the operator boundary. Use `0700` for the
directory and script, and `0600` for JSON.

Capsule directories, `manifest.json`, and `run.sh` must be real filesystem
objects owned by the runner's effective user, not symlinks or hardlinks.
`run.sh` must hash to the digest recorded in the manifest. The manifest is
limited to 64 KiB and `run.sh` to 1 MiB. The runner keeps the opened script
object and uses
no-follow, exclusive file creation for artifacts so a path swap cannot redirect
output into another file.

## `manifest.json`

The schema identifier is `execution-capsule.v1`. Unknown fields are rejected.

```json
{
  "schema_version": "execution-capsule.v1",
  "task": "Rewrite the prepared local commits, verify signatures, and report the resulting heads.",
  "cwd": "/absolute/path/to/repo",
  "entrypoint": "run.sh",
  "entrypoint_sha256": "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
  "access": "workspace",
  "allowed_paths": [
    "/absolute/path/to/repo"
  ],
  "expected_git": {
    "head": "0123456789abcdef0123456789abcdef01234567",
    "branch": "main"
  },
  "validation": [
    {
      "name": "verify-head-signature",
      "argv": ["git", "verify-commit", "HEAD"]
    }
  ]
}
```

Fields:

| Field | Contract |
| --- | --- |
| `schema_version` | Exactly `execution-capsule.v1`. |
| `task` | Non-empty supervisor objective, at most 8 KiB. Do not include secrets. |
| `cwd` | Existing absolute working directory. |
| `entrypoint` | Exactly `run.sh`. |
| `entrypoint_sha256` | `sha256:` plus 64 lowercase hexadecimal digits. |
| `access` | `workspace` or `host`. |
| `allowed_paths` | Existing absolute paths. Must contain `cwd`; workspace paths must remain under `cwd`. |
| `expected_git` | Optional exact `HEAD` and/or branch precondition, checked before Codex starts. |
| `validation` | Optional ordered commands represented as non-empty argv arrays and run in `cwd` through attested sandbox helpers. |

Neither `task`, `validation.argv`, nor `run.sh` should embed credentials. Refer
to an existing protected environment or secret store instead.

## Script contract

`run.sh` is the operator-reviewable source of authority. Capsule authors must
make it:

- start with `set -euo pipefail`;
- change to or verify the exact target directory;
- check expected state before mutating;
- use repository-governed commands such as `semantic-commit` where required;
- be idempotent where practical, or stop clearly when rerun is unsafe;
- perform its own essential validation so the direct route is complete;
- avoid network/provider mutation unless that effect was explicitly requested;
- never disable signing, hooks, approval gates, or policy checks.

The direct route does not read `manifest.json` and does not create a supervised
receipt. Therefore every safety check required for a direct operator run must
also live in `run.sh`.

## Supervised execution

`codex-cli agent run` validates the capsule before launching:

```text
codex --ask-for-approval never exec \
  --skip-git-repo-check \
  -C <cwd> \
  --sandbox workspace-write|danger-full-access \
  --json \
  [--ephemeral --disable plugins --disable remote_plugin \
   --disable apps --disable workspace_dependencies] \
  --output-schema <capsule>/result.schema.json \
  --output-last-message /dev/fd/0 \
  -- <supervisor-prompt>
```

The command intentionally preserves user/project instructions, config
governance, and hooks. It does not use the isolated prompt runtime, ignore
rules, or pass the dangerous bypass flag. The runner skips Codex's Git
repository precheck because a valid capsule may use a non-Git working
directory; capsule path, ownership, mode, digest, access, and optional Git
preconditions remain independently enforced.

## MCP policy

An Execution Capsule supervisor needs repository instructions, lifecycle hooks,
command rules, authentication, shell tooling, and the capsule's exact helper
commands. It does not need arbitrary external MCP tools, whose startup adds
latency, credential refreshes, external authority, and failure modes unrelated
to the prepared operation. `--mcp-mode` is a supervisor launch policy, not
authority embedded in `run.sh`; `execution-capsule.v1` and its manifest fields
are unchanged, and the direct `bash <capsule>/run.sh` route never uses MCP.

| Mode | Codex home | MCP behavior | Intended use |
| --- | --- | --- | --- |
| `disabled` (default) | Private supervisor projection | No user, plugin, or app MCP | Normal capsule execution |
| `inherited` | Real active Codex home | Existing full-home behavior | Explicit compatibility escape hatch |

`inherited` is accepted only as an explicit flag on the current invocation.
There is deliberately no environment variable or configuration default that can
silently broaden the supervisor's external tool surface, and `disabled` never
falls back to `inherited`. There is no per-server allowlist mode: a safe
allowlist would need a first-class Codex capability or a manifest schema that
can declare and validate direct and plugin server identities without copying
credential material, which would require `execution-capsule.v2`.

### Governance-projected supervisor home (`disabled`)

The default mode is not the isolated prompt runtime, which deliberately removes
instructions, hooks, plugins, and rules. It builds a separate private runtime
that:

1. resolves and refreshes active Codex authentication using the same
   source-home and remote-auth order as the isolated runtime;
2. creates a unique temporary `CODEX_HOME` with mode `0700`;
3. bridges file-backed authentication with an `auth.json` symlink and never
   copies credential bytes;
4. copies bounded active-home `AGENTS.md` bytes into an owner-only child file
   rather than symlinking a mutable governance source into a host-access
   supervisor;
5. projects only the `features.hooks` setting and the top-level `hooks` table
   into a generated owner-only `config.toml`, rekeying `hooks.state` entries
   from the source config path onto the projected config path so projected
   hooks keep their trust state, and copies a bounded, validated `hooks.json`
   when present;
6. keeps the real shell `HOME`, `PATH`, Git, SSH, GPG, and signing environment;
7. preserves project `AGENTS.md`, Git hooks, repository rules, and the
   capsule's existing sandbox and helper-attestation boundary;
8. runs ephemerally so the supervisor leaves no resumable session state;
9. removes child-control environment variables using the shared isolated
   runtime helper;
10. removes the temporary home after Codex exits and keeps the existing
    auth-symlink replacement warning.

Observed Codex behavior, recorded so the contract is not overclaimed: on Codex
`0.145.0`, `codex exec` did not execute config-defined lifecycle hooks at all.
A synthetic home with `features.hooks = true` and `SessionStart`,
`UserPromptSubmit`, and `PreToolUse` hooks fired none of them even though the
turn ran a shell command, with or without `hooks.state` trust entries. That
applies equally to `inherited` mode, so it is a property of non-interactive
Codex rather than of this projection. The projection therefore preserves hook
configuration, the hook feature, and hook trust state so that hook governance
applies wherever Codex honors it; repository Git hooks are unaffected because
the capsule script invokes Git directly.

The generated config is a governance projection, not a filtered copy of the
user config. It never includes `mcp_servers`; plugin, marketplace, app,
connector, skill, memory, goal, or subagent registrations; notification
integrations; static HTTP headers or other MCP credentials; or unrelated
user-interface configuration. Plugin-bundled hooks are not loaded in `disabled`
mode, because loading their owning plugin can also reintroduce an MCP server; a
capsule that depends on a plugin-bundled hook must use `inherited`.

Direct user MCP is absent because the projected `config.toml` contains no
`mcp_servers` table. Plugin and app MCP is absent because plugin and app
discovery is disabled explicitly and no plugin or app registration is
projected. The launch fails closed before any API request when the installed
Codex cannot enforce `--disable`, `--ephemeral`, `--skip-git-repo-check`, or the
`hooks`, `plugins`, `remote_plugin`, `apps`, and `workspace_dependencies`
features.

Before starting Codex, the runner parses each applicable project
`.codex/config.toml` within the accepted working directory and its enclosing
repository, and rejects `disabled` mode when one declares `mcp_servers`,
`plugins`, `apps`, `connectors`, `marketplace`, or a true
`features.plugins|remote_plugin|apps|workspace_dependencies`. Rejected values
are never logged; only the path and the declaration name appear. Project
configuration is read through a no-follow descriptor and re-verified
immediately before launch, so a symlink or path swap after preflight is
detected rather than silently applied.

`inherited` uses the real `CODEX_HOME` and the unchanged launch shape,
preserves the host-access acknowledgement, prints
`codex-cli agent run: MCP mode inherited; external tools may initialize` to
stderr, records `mcp_mode: "inherited"` in the receipt, never retries
automatically with a different mode, and never suppresses OAuth or MCP startup
errors.

### Startup observability

JSON stdout stays a single documented envelope; progress is written only to
stderr. Both text and JSON modes emit a phase line before spawning Codex:

```text
codex-cli agent run: starting supervisor (mcp=disabled)
```

Codex stdout is drained on a dedicated bounded reader, so the parent observes
the first newline-terminated JSONL event while still retaining the whole
evidence stream and never blocking the child on a full pipe. If no first event
arrives within an internal 60-second deadline, the runner terminates the Codex
process group, escalating from `SIGTERM` to `SIGKILL`, reaps it, and returns
`codex-supervisor-startup-timeout` with bounded recovery guidance and no
captured child stderr. After the first event the runner emits a low-frequency
stderr heartbeat only while no other event progress occurs. The deadline bounds
startup only: a slow model turn after the first event never trips it, and there
is no total model-execution timeout. The deadline is an internal constant;
`CODEX_CLI_CAPSULE_FIRST_EVENT_DEADLINE_MS` exists only so tests can make it
deterministic and is not a supported public interface.

Operator interrupts keep working: the supervisor runs in its own process group
so it can be terminated as a group, and `SIGINT`, `SIGTERM`, and `SIGHUP` are
forwarded to that group before the parent re-raises them.

`workspace` is the normal path. `host` is accepted only when both the manifest
declares it and the operator supplies `--allow-host-access`. This two-part
acknowledgement is intended for an operator launching the command outside the
current agent's constrained environment, including an urgent local hotfix.

Evidence trust differs by access class:

- `workspace` receipts are `sandbox-attested`: the filesystem sandbox keeps
  the helper and named capsule inputs outside the writable task boundary.
- `host` receipts are `supervisor-trusted`: they preserve governance,
  monitoring, validation, and durable reporting, but are not a
  tamper-resistant security attestation against a malicious same-UID process
  with `danger-full-access`. Use a distinct OS security principal when
  adversarial host attestation is required.

The parent never runs `run.sh` or validation directly. It gives Codex exact
helper commands whose executable is a private owner-only snapshot of the
running `codex-cli`, stored outside the declared workspace. The parent keeps
the helper inode open, verifies its identity, permissions, and digest after
Codex exits, and removes it before publishing the receipt. Each helper runs
inside the active Codex sandbox,
revalidates the capsule, snapshots `run.sh` into memory, checks the snapshot
digest, and executes those exact bytes. The snapshot preserves `run.sh` as
`$0`, provides `EXECUTION_CAPSULE_DIR` and `EXECUTION_CAPSULE_ENTRYPOINT`, and
does not promise a filesystem path in `BASH_SOURCE`: scripts must use the
manifest `cwd`, explicit paths, or those environment variables instead of
resolving adjacent files through `BASH_SOURCE`. If the script fails, Codex may diagnose
the failure, make only minimal corrections inside `allowed_paths`, and rerun
the same helper. After one script attempt succeeds, Codex runs the exact helper
for every declared validation.

The runner accepts an attempt only when the parent-held Codex stdout stream
contains a completed `command_execution` event for the exact helper command
and its output contains the matching nonce-bound helper attestation. The
parent publishes that captured stream as `events.jsonl` only after Codex exits,
so the supervised process cannot forge evidence through the named artifact.
Codex also writes its structured final report through standard input,
which the Node launcher preserves as a parent-held unlinked file created
inside the declared workspace; the parent validates the bytes and atomically
publishes `final.json`. A model-authored final claim alone cannot attest execution.
After Codex exits, the parent verifies the pinned helper plus `manifest.json`
and `run.sh` retain their original identity, permissions, and digest. Artifact
publication atomically replaces hostile symlink or hardlink directory entries
without following them.

The supervisor must not reinterpret a hook, permission, policy, or concurrent
Git-state failure as permission to bypass the guard.

## Receipt and output

JSON mode follows the shared service envelope contract. Successful and
post-preflight execution receipts use:

```text
cli.codex-cli.execution-capsule.receipt.v1
```

The top-level fields are:

| Field | Type | Contract |
| --- | --- | --- |
| `schema_version` | string | Exactly `cli.codex-cli.execution-capsule.receipt.v1`. |
| `command` | string | Exactly `agent run`. |
| `ok` | boolean | True only when every success condition below holds. |
| `result` | object | Required detailed receipt, including on post-preflight failure. |
| `error` | object | Omitted on success; required with stable `code`, `message`, and `details.receipt` on post-preflight failure. |

`result` records:

| Field | Type | Contract |
| --- | --- | --- |
| `capsule` | string | Canonical capsule path. |
| `manifest_sha256`, `entrypoint_sha256` | string | Validated `sha256:` digests. |
| `access` | string | `workspace` or `host`. |
| `evidence_trust` | string | `sandbox-attested` for `workspace`; `supervisor-trusted` for `host`. |
| `mcp_mode` | string | Effective policy: `disabled` or `inherited`. |
| `supervisor_runtime` | string | `governance-projected` for `disabled`; `inherited` for `inherited`. |
| `codex_exit_code` | integer | Codex process exit; `127` when launch failed. |
| `codex_error` | string | Optional; omitted unless Codex launch failed or the supervisor startup deadline expired. |
| `evidence_error` | string | Optional; omitted unless captured events could not be read or safely published. |
| `receipt_error` | string | Optional; records primary receipt publication failure when a private recovery receipt with a parent-selected, cryptographically unpredictable name was used. |
| `script_runs` | array | Attested attempts in event order: `phase`, authoritative `terminal` marker, `exit_code`, `passed`, exact `command`, `events`, and optional `error`. |
| `script_passed` | boolean | Authoritative derived outcome of the last matching script event. |
| `validation` | array | One result per manifest step using its last matching event: `name` (nullable), `argv`, `exit_code`, `passed`, exact `command`, `events`, and optional `error`. |
| `validations_passed` | boolean | True only when every last matching validation result passed. |
| `helper_integrity_valid` | boolean | Post-supervision pinned helper identity and digest result. |
| `helper_integrity_error` | string | Optional; omitted when helper integrity is valid. |
| `entrypoint_integrity_valid` | boolean | Post-supervision manifest and entrypoint integrity result. |
| `entrypoint_integrity_error` | string | Optional; omitted when integrity is valid. |
| `final_report_valid` | boolean | Whether `final.json` matched the required schema. |
| `final_report_error` | string | Optional; omitted when the report is valid. |
| `final` | object or null | `status` (`succeeded`, `failed`, or `blocked`), `summary`, `actions`, `validation`, `errors`, and `recommendations`. |
| `artifacts` | object | Paths for `result_schema`, `events`, `final_report`, and `receipt`. |
| `completed_at` | string | RFC 3339 completion time. |

Preflight/schema failures use
`cli.codex-cli.execution-capsule.error.v1` with top-level `command:
"agent run"`, `ok: false`, and `error.code`/`error.message`; JSON is always
written to stdout. The exact checked schemas are
[`execution-capsule-receipt-v1.schema.json`](execution-capsule-receipt-v1.schema.json)
and
[`execution-capsule-error-v1.schema.json`](execution-capsule-error-v1.schema.json).
Additive fields are allowed within v1. Removing or renaming a documented field
requires a new schema version.

The receipt records the effective policy but never a configured MCP server name,
an MCP URL, OAuth status or errors, a token or static header, a hook command
body, a config path outside the documented capsule artifacts, or raw child
stderr.

Text mode reports the effective policy as
`mcp mode: <mode> (supervisor runtime: <runtime>)`.

Preflight failures use the error envelope and add typed recovery guidance in
`error.details` (`retryable`, `next_action`) for the supervisor policy codes:

| Code | Phase | Exit | Retryable | Next action |
| --- | --- | --- | --- | --- |
| `capsule-supervisor-unsupported` | Preflight | `65` | No | Upgrade Codex or explicitly choose inherited mode |
| `capsule-supervisor-home-failed` | Preflight | `65` | Usually | Repair local runtime or temporary directory permissions |
| `capsule-supervisor-config-invalid` | Preflight | `65` | No | Repair the malformed hook or project configuration |
| `capsule-project-mcp-undeclared` | Preflight | `65` | No | Remove project MCP for this run or explicitly choose inherited mode |
| `codex-supervisor-startup-timeout` | Runtime | `1` | Conditional | Inspect runtime health, then retry or explicitly choose inherited mode |

A preflight rejection happens before any capsule artifact is created or
replaced, so a fail-closed MCP policy decision leaves the capsule untouched. A
startup timeout is post-preflight and still publishes a detailed receipt with
`ok: false`; it can never produce `ok: true`.

Success requires all seven conditions:

1. Codex exits zero.
2. `final.json` is valid and reports `status: "succeeded"`.
3. The pinned helper retains its validated identity, metadata, and digest.
4. The manifest and entrypoint retain their validated identity, permissions,
   and digest.
5. The parent captures and safely publishes valid execution evidence.
6. The terminal exact script helper event has a successful matching
   attestation.
7. The last matching event for every declared validation helper has a
   successful attestation.

Preflight/schema failures return `65`. Runtime, Codex, final-report, or
validation failure returns `1`. Help and successful execution return `0`.
