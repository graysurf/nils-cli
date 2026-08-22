# macos-agent

`macos-agent` is a guarded adapter around one immutable Peekaboo release. It
does not contain a second macOS automation engine: Peekaboo owns observation,
Accessibility actions, synthetic input, capture, and stdio MCP.
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
architecture, app metadata, and exact code-signing identities. Download,
archive, architecture, signature, and Gatekeeper commands use fixed macOS
system paths rather than caller-controlled `PATH` lookup. App Gatekeeper/notary
assessment is mandatory during both install and execution verification;
`--strict` additionally assesses standalone CLI notarization. The current
v4.2.2 CLI and app both require notarization and report
`security_posture=full`.
Timeout or signal termination always fails closed. There is no runtime bypass
flag. Lifecycle responses from install,
status, and rollback also expose `strict`, `cli_notarization_policy`, and
`security_posture`; an accepted waiver is therefore never collapsed into an
undifferentiated `verified=true` result. It atomically owns one stable app path.
Rollback is permitted only
when the exact prior tag, commit, assets, and executable digests are retained in
the embedded `rollback_releases` allowlist; mutable receipts are never a trust
root. The v4.2.2 lock intentionally authorizes no rollback release because the
v3 command and capability surface cannot satisfy the v4 adapter contract. The
exact v3.9.3 tuple is retained separately in `upgrade_from_releases` only to
authenticate and retire an existing installation during an in-place upgrade;
it can never become active again through rollback. A shared verified-backend
lease, including the digest recorded in the
journal, is held for the full lifetime of every execution. Install and rollback
take the exclusive lifecycle lock, so verified code cannot be swapped between
check and use. It will not replace an app it cannot prove it owns. It never
changes TCC permissions.

`doctor` without `--strict` is report-only and exits successfully with
`ready=false` when the environment is not ready. `doctor --strict` exits 77 for
the same failed permission, Bridge, runtime, or capability checks. Its Bridge
probe targets the stable app socket at
`~/Library/Application Support/Peekaboo/bridge.sock` directly, so an unrelated
default-selected daemon cannot satisfy or block stable-app readiness.

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

## Migrating from the native engine

The adapter v2 boundary began with nils-cli v1.22.6. The Peekaboo v4 migration
publishes `macos-agent.adapter.v3` because it removes an interface and changes
the admitted upstream surface. Existing callers must migrate as one breaking
contract change; there are no compatibility aliases:

- `preflight` → `doctor --strict` for permission, Bridge, runtime, and
  capability readiness.
- `windows`, `apps`, `window`, `input`, `input-source`, and `ax` → `exec --
  <peekaboo argv>` with an observable `--expected` postcondition for mutations.
- Peekaboo v3 `list screens` → the exact read-only v4 replacement `exec --
  screen list`; other `screen` subcommands are not admitted.
- `observe`, `debug`, `wait`, and `profile` have no adapter-level grammar.
  Use the corresponding reviewed Peekaboo argv through `exec`; structural
  evidence, debug redaction, and postcondition ownership now live in journal v2
  and the calling skill.
- Peekaboo v4 removed the `.peekaboo.json` runner, so `scenario` is no longer an
  adapter interface. Chain individually reviewed `exec` calls in the caller;
  `macos-agent` does not expose a shell fallback.
- TSV output and the former global retry, timeout, and trace flags are removed.
  Successful adapter envelopes use `macos-agent.adapter.v3`; upstream JSON is
  nested under `result.upstream`. Typed adapter failures remain on stderr and
  use the exit codes listed below.

Create a new `--out-dir` after 512 journal steps. This explicit rotation bound
caps cross-process reopen and integrity-scan work while retaining the complete
append-only record for each run directory.

SSH transport retains at most 16 concurrent or cleanup-pending remote session
roots per user. New sessions fail closed at that aggregate bound until a
completed or explicitly cleaned session releases a slot.

Evidence modes are:

- `minimal`: structural journal and sanitized upstream response.
- `debug`: minimal plus a sanitized upstream result artifact.
- `sensitive`: suppresses values, titles, paths, and payload fields; no replay
  material is retained for sensitive input.

Hard-disabled capabilities are published by `capabilities`: `agent`, `analyze`,
`audio`, `browser`, `clipboard`, `config`, `credentials`, `image`, `mcp_agent`,
`permission_mutation`, `shell`, HTTP MCP, and SSE MCP.

## SSH transport

Add `--host <trusted-alias>` to backend, doctor, capabilities, exec, or MCP
commands. SSH uses batch authentication and a fixed remote command. A
versioned JSON request is sent over stdin; user argv is never interpolated into
a shell command. Only journal-core files and artifact-index-declared files are
returned, with path, count, size, and SHA256 checks. Remote session cleanup is
audited on success, failure, timeout, and transfer failure. Host/user/key/config
values are not persisted or echoed.

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
only bounded structural `mcp_method`/`mcp_tool` metadata. Request, response, and
server-notification envelopes are validated before correlation. Bounded reader
and writer queues plus write
and response deadlines prevent a stalled upstream from consuming unbounded
memory or holding a session indefinitely. SSH carries a bounded typed terminal
status outside protocol stdout, preserving upstream exit class 70 while
reserving 75 for transport failures. Profiles are monotonic:

- `observe`: `see`, `inspect_ui`, `permissions`, `sleep`, `verify_state`.
- `interact`: observe plus `click`, `type`, `press`, `scroll`, `drag`, `move`,
  `set_value`, `action`, `window`, `app`, `menu`.
- `extended`: interact plus `dialog`, `dock`, `space`, `capture`, `paste`.

The hard-disabled tools remain unavailable even when upstream configuration or
provider credentials attempt to enable them.

## Journals and replay

Every exec and MCP session writes:

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
never replayed. Journals received over SSH are also never locally replayable:
the current interface cannot supply and re-verify a fresh explicit remote
target, so both replay planning and execution refuse those steps. See the
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
`macos-agent.adapter.v3`.

## Development

```bash
cargo test -p nils-macos-agent
bash scripts/ci/completion-freshness-audit.sh --strict
bash scripts/ci/docs-placement-audit.sh --strict
```

Crate docs are indexed in [`docs/README.md`](docs/README.md).
