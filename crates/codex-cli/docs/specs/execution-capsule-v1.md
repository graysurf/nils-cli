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
objects, not symlinks. `run.sh` must hash to the digest recorded in the
manifest. The manifest is limited to 64 KiB.

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
| `validation` | Optional ordered commands represented as non-empty argv arrays and run in `cwd` after Codex exits. |

Neither `task`, `validation.argv`, nor `run.sh` should embed credentials. Refer
to an existing protected environment or secret store instead.

## Script contract

`run.sh` is the operator-reviewable source of authority. It should:

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
  -C <cwd> \
  --sandbox workspace-write|danger-full-access \
  --json \
  --output-schema <capsule>/result.schema.json \
  --output-last-message <capsule>/final.json \
  -- <supervisor-prompt>
```

The command intentionally preserves the active Codex home, user/project
instructions, config, and hooks. It does not use the isolated prompt runtime,
ignore rules, or pass the dangerous bypass flag.

`workspace` is the normal path. `host` is accepted only when both the manifest
declares it and the operator supplies `--allow-host-access`. This two-part
acknowledgement is intended for an operator launching the command outside the
current agent's constrained environment, including an urgent local hotfix.

The supervisor is instructed to run the exact script, diagnose failures, make
only minimal corrections inside `allowed_paths`, rerun the script and
validation, and report a structured status. It must not reinterpret a hook,
permission, policy, or concurrent Git-state failure as permission to bypass
the guard.

## Receipt and output

The final stdout JSON envelope uses:

```text
cli.codex-cli.execution-capsule.receipt.v1
```

It records:

- manifest and entrypoint digests;
- access class and Codex exit code;
- independently rerun validation argv, exit codes, and pass/fail state;
- whether the structured final report was valid;
- the final report and artifact paths;
- completion time.

Success requires all three conditions:

1. Codex exits zero.
2. `final.json` is valid and reports `status: "succeeded"`.
3. Every declared wrapper validation passes.

Preflight/schema failures return `65`. Runtime, Codex, final-report, or
validation failure returns `1`. Help and successful execution return `0`.
