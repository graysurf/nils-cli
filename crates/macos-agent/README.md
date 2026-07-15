# macos-agent

`macos-agent` is a guarded adapter around one immutable Peekaboo release. It
does not contain a second macOS automation engine: Peekaboo owns observation,
Accessibility actions, synthetic input, capture, scenarios, and stdio MCP.
The adapter owns supply-chain verification, local/SSH transport, policy,
privacy-preserving journals, and guarded replay.

Supported hosts are macOS 15 or newer. Linux can build and run the deterministic
fake-backend/fake-SSH test suite, but it is not a desktop automation target.

## Backend lifecycle

The exact repository, tag, commit, asset URLs, SHA256 values, architectures,
CLI/app Bridge builds, bundle/signing identities, minimum macOS, and capability
probes are frozen in [`peekaboo-lock.json`](peekaboo-lock.json). Runtime code
never resolves a floating `latest` release.

```bash
macos-agent backend install --dry-run --format json
macos-agent backend install --strict --format json
macos-agent backend status --format json
macos-agent backend verify --strict --format json
macos-agent backend rollback --dry-run --strict --format json
macos-agent doctor --strict --format json
macos-agent capabilities --strict --format json
```

Install downloads the official locked CLI and app assets into private,
versioned user storage, validates archive paths/symlinks, archive SHA256 values,
and locked extracted-executable SHA256 values, then checks version,
architecture, app metadata, and exact code-signing identities. `--strict`
additionally requires the locked CLI notarization and app Gatekeeper
assessments. It atomically owns one stable app path. Rollback is permitted only
when the exact prior tag, commit, assets, and executable digests are retained in
the embedded `rollback_releases` allowlist; mutable receipts are never a trust
root. A shared verified-backend lease, including the digest recorded in the
journal, is held for the full lifetime of every execution. Install and rollback
take the exclusive lifecycle lock, so verified code cannot be swapped between
check and use. It will not replace an app it cannot prove it owns. It never
changes TCC permissions.

`doctor` without `--strict` is report-only and exits successfully with
`ready=false` when the environment is not ready. `doctor --strict` exits 77 for
the same failed permission, Bridge, runtime, or capability checks.

## Execute Peekaboo

Arguments after `--` are passed as argv without grammar translation. Mutating
commands require an observable `--expected` postcondition.

```bash
run_dir="$(agent-out project --topic calculator-inspect --mkdir)"

macos-agent exec \
  --out-dir "$run_dir" \
  --intent "Inspect Calculator" \
  -- see --app Calculator --json

macos-agent exec \
  --out-dir "$run_dir" \
  --intent "Press the clear button" \
  --expected "Calculator display is zero" \
  -- click --app Calculator --on C --json
```

Use `--runtime app|daemon|auto|process` to select the effective Peekaboo
authority. `app` is the stable default: it reuses the owned app only when its
exact locked Bridge build is ready, otherwise launches a new stable app
instance on `~/Library/Application Support/Peekaboo/bridge.sock` and verifies
the exact handshake. `daemon` starts the verified CLI daemon on an
executable-digest-scoped socket. `auto` first selects an exact compatible GUI
Bridge and otherwise starts the verified CLI daemon on its own digest-scoped
socket. A release transition retires only inactive daemon sockets whose exact
old build is still authorized by the embedded lock. `process` passes
`--no-remote`.
Evidence modes are:

- `minimal`: structural journal and sanitized upstream response.
- `debug`: minimal plus a sanitized upstream result artifact.
- `sensitive`: suppresses values, titles, paths, and payload fields; no replay
  material is retained for sensitive input.

Hard-disabled capabilities are published by `capabilities`: `agent`, `analyze`,
`audio`, `browser`, `clipboard`, `config`, `credentials`, `image`, `mcp_agent`,
`permission_mutation`, `shell`, HTTP MCP, and SSE MCP.

## Scenarios

Only a regular, non-symlink JSON file of at most 1 MiB is accepted. The adapter
reads and validates it once, pre-scans disabled commands, hashes the validated
bytes, and executes only a private mode-0600 staged copy. The caller's source
is never modified or reopened for execution.

```bash
macos-agent scenario \
  --out-dir "$run_dir" \
  --file ./flow.peekaboo.json \
  --runtime app \
  --evidence-mode minimal
```

## SSH transport

Add `--host <trusted-alias>` to backend, doctor, capabilities, exec, scenario,
or MCP commands. SSH uses batch authentication and a fixed remote command. A
versioned JSON request is sent over stdin; user argv is never interpolated into
a shell command. Scenario inputs use digest-verified private staging. Only
journal-core files and artifact-index-declared files are returned, with path,
count, size, and SHA256 checks. Remote session cleanup is audited on success,
failure, timeout, and transfer failure. Host/user/key/config values are not
persisted or echoed.

```bash
ssh_run_dir="$(agent-out project --topic calculator-inspect-ssh --mkdir)"
macos-agent exec --host mac-role \
  --out-dir "$ssh_run_dir" \
  -- see --app Calculator --json
```

The same `macos-agent` version and Peekaboo lock must be present at both ends.

## stdio MCP

```bash
local_mcp_dir="$(agent-out project --topic peekaboo-mcp-local --mkdir)"
ssh_mcp_dir="$(agent-out project --topic peekaboo-mcp-ssh --mkdir)"
macos-agent mcp --out-dir "$local_mcp_dir" --tool-profile observe
macos-agent mcp --host mac-role --out-dir "$ssh_mcp_dir" --tool-profile interact
```

The proxy keeps stdout JSON-RPC-clean, filters `tools/list`, rejects disallowed
`tools/call` requests before forwarding, clears provider API keys, and journals
only method/tool metadata. Request, response, and server-notification envelopes
are validated before correlation. Bounded reader and writer queues plus write
and response deadlines prevent a stalled upstream from consuming unbounded
memory or holding a session indefinitely. SSH carries a bounded typed terminal
status outside protocol stdout, preserving upstream exit class 70 while
reserving 75 for transport failures. Profiles are monotonic:

- `observe`: `see`, `inspect_ui`, `list`, `permissions`, `sleep`.
- `interact`: observe plus `click`, `type`, `hotkey`, `scroll`, `swipe`, `drag`,
  `move`, `set_value`, `perform_action`, `window`, `app`, `menu`.
- `extended`: interact plus `dialog`, `dock`, `space`, `capture`, `paste`.

The hard-disabled tools remain unavailable even when upstream configuration or
provider credentials attempt to enable them.

## Journals and replay

Every exec, scenario, and MCP session writes:

- `manifest.json`
- `steps.jsonl`
- `artifacts/index.json`
- `summary.json`
- `redaction.json`
- `review.json` when review is requested

```bash
macos-agent journal summarize --out-dir "$run_dir" --format json
macos-agent journal review --out-dir "$run_dir" --format json
macos-agent journal replay-plan --out-dir "$run_dir" --format json
macos-agent journal replay-step --out-dir "$run_dir" --step step-000001
```

Replay is deterministic `safe|conditional|never`. Planning is read-only;
conditional replay requires explicit confirmation, a fresh caller-supplied
`--expected` observable postcondition, explicit pre-action snapshot lineage,
and a caller-supplied current snapshot that matches it. Replay recomputes the
command, policy, argument shape, and replay class from retained argv before
execution. Unknown mutations, unguarded mutations, secrets, typed/clipboard
values, policy blocks, tampered replay metadata, or changed backend digests are
never replayed. See the
[`journal v2 spec`](docs/specs/macos-agent-journal-v2.md).

## Exit classes

| Code | Class |
| ---: | --- |
| 0 | Success |
| 64 | Usage |
| 69 | Backend unavailable, unverified, or drifted |
| 70 | Peekaboo/upstream failure or timeout |
| 74 | Journal/artifact integrity failure |
| 75 | SSH/transport/cleanup failure |
| 77 | Required macOS permission/runtime readiness failure |
| 78 | Adapter policy refusal |

Errors are written only to stderr. `--error-format json` emits
`macos-agent.error.v2`; successful command envelopes use
`macos-agent.adapter.v2`.

## Development

```bash
cargo test -p nils-macos-agent
bash scripts/ci/completion-freshness-audit.sh --strict
bash scripts/ci/docs-placement-audit.sh --strict
```

Crate docs are indexed in [`docs/README.md`](docs/README.md).
