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
  --output-schema <capsule>/result.schema.json \
  --output-last-message /dev/fd/<parent-held-final-capture> \
  -- <supervisor-prompt>
```

The command intentionally preserves the active Codex home, user/project
instructions, config, and hooks. It does not use the isolated prompt runtime,
ignore rules, or pass the dangerous bypass flag. The runner skips Codex's Git
repository precheck because a valid capsule may use a non-Git working
directory; capsule path, ownership, mode, digest, access, and optional Git
preconditions remain independently enforced.

`workspace` is the normal path. `host` is accepted only when both the manifest
declares it and the operator supplies `--allow-host-access`. This two-part
acknowledgement is intended for an operator launching the command outside the
current agent's constrained environment, including an urgent local hotfix.

The parent never runs `run.sh` or validation directly. It gives Codex exact
helper commands whose executable is an inherited descriptor pinned to the
running `codex-cli` inode, rather than a reopenable pathname. Each helper runs
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
Codex also writes its structured final report through a parent-held unlinked
descriptor; the parent validates the bytes and atomically publishes
`final.json`. A model-authored final claim alone cannot attest execution.
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
| `codex_exit_code` | integer | Codex process exit; `127` when launch failed. |
| `codex_error` | string | Optional; omitted unless Codex launch failed. |
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
