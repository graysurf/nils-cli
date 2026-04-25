# api-websocket

## Overview

`api-websocket` executes deterministic WebSocket request files, writes optional call history, and generates Markdown reports.
It follows the same CLI conventions as `api-rest`, `api-gql`, and `api-grpc`.

## Transport decision

- Selected backend: in-process Rust transport via `tungstenite` in
  `api-testing-core::websocket::runner`. This is the only supported runtime path.
- Revisit when:
  - streaming/session orchestration needs async multiplexing beyond scripted send/receive steps;
  - platform/runtime behavior diverges in CI and a swap behind the transport boundary is justified.

## Runtime dependency policy

- No extra external binary is required for WebSocket execution.
- `api-websocket` uses the embedded Rust transport path only.
- Cross-reference: see `BINARY_DEPENDENCIES.md` section 1.2 for the workspace-level statement.

## Setup and naming conventions

Canonical setup directory: `setup/websocket`.

### Endpoint variables

- `WS_URL_<PROFILE>` in `setup/websocket/endpoints.env` (optional local override: `endpoints.local.env`).
- `WS_ENV_DEFAULT` can set a default endpoint profile.
- `WS_URL` can force an explicit URL.

### Token variables

- `WS_TOKEN_<PROFILE>` in `setup/websocket/tokens.env` (optional local override: `tokens.local.env`).
- `WS_TOKEN_NAME` chooses a token profile.
- If no profile is selected, fallback envs are `ACCESS_TOKEN` then `SERVICE_TOKEN`.

### URL/token precedence

URL precedence (`call`):

1. `--url`
2. `--env` (profile lookup via `WS_URL_<PROFILE>`, or literal `ws://`/`wss://`)
3. `WS_URL`
4. `WS_ENV_DEFAULT` profile
5. default `ws://127.0.0.1:9001/ws`

Token precedence (`call`):

1. `--token`
2. `WS_TOKEN_NAME`
3. profile lookup via `WS_TOKEN_<PROFILE>`
4. env fallback `ACCESS_TOKEN` then `SERVICE_TOKEN`

### History

- Default history file: `<setup_dir>/.ws_history`
- Override: `WS_HISTORY_FILE`
- Controls: `WS_HISTORY_ENABLED`, `WS_HISTORY_MAX_MB`, `WS_HISTORY_ROTATE_COUNT`, `WS_HISTORY_LOG_URL_ENABLED`

## Request schema (v1)

See [`docs/specs/websocket-request-schema-v1.md`](docs/specs/websocket-request-schema-v1.md).

Quick example:

```json
{
  "url": "ws://127.0.0.1:9001/ws",
  "steps": [
    {"type": "send", "text": "ping"},
    {"type": "receive", "expect": {"jq": ".ok == true"}},
    {"type": "close"}
  ],
  "expect": {"textContains": "ok"}
}
```

## Usage

```text
Usage: api-websocket <command> [args]

Commands:
  call             Execute a request file and print the last received message to stdout (default)
  history          Print the last (or last N) history entries
  report           Generate a Markdown API test report
  report-from-cmd  Generate a report from a saved `call` snippet
  completion       Print shell completion script

Common options (see subcommand help for full details):
  --config-dir <dir>   Seed setup/websocket discovery (call/history/report)
  --format <text|json> Structured output for call/history
  -h, --help           Print help

Examples:
  api-websocket --help
  api-websocket call --help
  api-websocket report --help
  api-websocket report-from-cmd --help
  api-websocket completion zsh
```

## Commands

- `call` (default): Execute a request file and print the last received message.
  Positional: `<request.ws.json>` (also accepts `*.websocket.json`).
  Options: `-e/--env <name>`, `-u/--url <url>`, `--token <name>`, `--config-dir <dir>`,
  `--no-history`, `--format <text|json>`.
- `history`: Print the last entry or tail N entries.
  Options: `--config-dir <dir>`, `--file <path>`, `--last`, `--tail <n>`, `--command-only`,
  `--format <text|json>`.
- `report`: Generate a Markdown report for a request.
  Required: `--case <name>`, `--request <file>`. Exactly one of `--run` or `--response <file|->`.
  Options: `--out <path>`, `-e/--env <name>`, `-u/--url <url>`, `--token <name>`,
  `--no-redact`, `--no-command`, `--no-command-url`, `--project-root <path>`,
  `--config-dir <dir>`.
- `report-from-cmd`: Generate a Markdown report from a saved `call` command snippet.
  Positional: `[snippet]` (or pass `--stdin` to read from stdin).
  Options: `--case <name>`, `--out <path>`, `--response <file|->`, `--allow-empty`
  (alias `--expect-empty`; no-op for `api-websocket`, kept for parity), `--dry-run`,
  `--stdin`.
- `completion`: Print a shell completion script. Argument: `<SHELL>` (`bash` or `zsh`).

## JSON contract (`--format json`)

Supported for `call` and `history`. Other subcommands ignore `--format`.

Success envelope:

```json
{
  "schema_version": "cli.api-websocket.call.v1",
  "command": "api-websocket call",
  "ok": true,
  "result": {}
}
```

Failure envelope:

```json
{
  "schema_version": "cli.api-websocket.call.v1",
  "command": "api-websocket call",
  "ok": false,
  "error": {
    "code": "stable-machine-code",
    "message": "human-readable summary",
    "details": {}
  }
}
```

`error.details` is optional and only present for failure modes that carry contextual
data (for example `expectation_failed` includes `target` and `last_received`;
`history_not_found` includes `history_file`).

Full CLI/JSON contract: [`docs/specs/websocket-cli-contract-v1.md`](docs/specs/websocket-cli-contract-v1.md)

## Quickstart

```bash
api-websocket call --env local setup/websocket/requests/health.ws.json
api-websocket call --format json --url ws://127.0.0.1:9001/ws setup/websocket/requests/health.ws.json
api-websocket history --tail 5
api-websocket report --case ws-health --request setup/websocket/requests/health.ws.json --run
api-websocket history --command-only | api-websocket report-from-cmd --stdin --dry-run
```

## Docs

- [Docs index](docs/README.md)
- [Request schema v1](docs/specs/websocket-request-schema-v1.md)
- [CLI contract v1](docs/specs/websocket-cli-contract-v1.md)
