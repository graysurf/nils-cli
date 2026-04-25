# WebSocket CLI Contract v1

> Source of truth: `crates/api-websocket/src/cli.rs` (clap definitions),
> `crates/api-websocket/src/commands/{call,history,report}.rs` (envelope shapes),
> `crates/api-testing-core/src/websocket/{schema,runner}.rs` (request schema and transcript).

## Commands

`api-websocket` supports:

- `call`
- `history`
- `report`
- `report-from-cmd`
- `completion` (prints a `bash` or `zsh` completion script; no JSON envelope)

Default command behavior:

- bare positional request path is treated as `call`.
- `--help` / `-h` and `--version` / `-V` are handled at the root before the default insertion.

## Exit codes

- `0`: success
- `1`: operational/validation failure
- `3`: history file exists but contains no records (`history`)

## Stdout/stderr behavior

- `call` (text mode): stdout prints the last received message.
- `history` (text mode): stdout prints selected history records.
- `report`/`report-from-cmd`: stdout prints generated report path.
- stderr is used for human-readable diagnostics in text mode.

## JSON mode

- Explicit only: `--format json`
- Supported commands: `call`, `history`. `report`, `report-from-cmd`, and `completion`
  do not accept `--format`.
- Human-readable mode remains default.

## JSON envelope

Guideline reference:

- `docs/specs/cli-service-json-contract-guideline-v1.md`

### `call` success

```json
{
  "schema_version": "cli.api-websocket.call.v1",
  "command": "api-websocket call",
  "ok": true,
  "result": {
    "target": "ws://127.0.0.1:9001/ws",
    "last_received": "{\"ok\":true}",
    "transcript": [
      {"direction": "send", "payload": "ping"},
      {"direction": "receive", "payload": "{\"ok\":true}"},
      {"direction": "close", "payload": ""}
    ]
  }
}
```

`result.transcript` is an array of `{ "direction": "send" | "receive" | "close", "payload": "<text>" }`
entries. `payload` is always a string; binary frames are decoded as lossy UTF-8, and control
frames render as `<PING:...>`, `<PONG:...>`, `<CLOSE:<code>:<reason>>`, or `<FRAME>` placeholders
(see `api_testing_core::websocket::runner::parse_message_text`).

`result.last_received` mirrors the most recent `receive` step's `payload` (or `null` if no
`receive` step ran successfully).

### `call` failure

```json
{
  "schema_version": "cli.api-websocket.call.v1",
  "command": "api-websocket call",
  "ok": false,
  "error": {
    "code": "request_not_found",
    "message": "Request file not found: ...",
    "details": {}
  }
}
```

`error.details` is optional. It is included for failure modes that carry contextual data:

- `websocket_execute_error`: `{ "target": "<resolved url>" }`
- `expectation_failed`: `{ "target": "<resolved url>", "last_received": "<text or null>" }`

### `history` success

```json
{
  "schema_version": "cli.api-websocket.history.v1",
  "command": "api-websocket history",
  "ok": true,
  "result": {
    "history_file": ".../.ws_history",
    "count": 1,
    "records": ["..."]
  }
}
```

### `history` failure

```json
{
  "schema_version": "cli.api-websocket.history.v1",
  "command": "api-websocket history",
  "ok": false,
  "error": {
    "code": "history_not_found",
    "message": "History file not found: ...",
    "details": {
      "history_file": ".../.ws_history"
    }
  }
}
```

`error.details.history_file` is included for `history_not_found` and `history_empty`.

## Stable error codes

### `call`

- `request_not_found`
- `request_parse_error`
- `setup_resolve_error`
- `endpoint_resolve_error`
- `auth_resolve_error`
- `jwt_validation_error`
- `websocket_execute_error`
- `expectation_failed`

### `history`

- `history_resolve_error`
- `history_not_found`
- `history_read_error`
- `history_empty`

## Secret handling

- JSON output must not include bearer token material.
- Tokens are never emitted in `result` payloads.
- history command snippets mask token values (`REDACTED`) in suite artifacts.

## Transport runtime

- `api-websocket` always uses the in-process `tungstenite`-backed runner exposed by
  `api_testing_core::websocket::runner::execute_websocket_request`. There is no shell-out
  to an external WebSocket binary at any point in the CLI surface. See
  `BINARY_DEPENDENCIES.md` section 1.2 and `crates/api-websocket/README.md` ("Transport
  decision") for the workspace-level statement and the historical "rejected backend"
  rationale.
