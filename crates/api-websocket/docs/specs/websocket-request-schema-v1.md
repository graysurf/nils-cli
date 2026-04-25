# WebSocket Request Schema v1

> Source of truth: `crates/api-testing-core/src/websocket/schema.rs`
> (`parse_websocket_request_json` and friends).

## Scope

This schema defines deterministic, file-based WebSocket request execution used by:

- `api-websocket call`
- `api-websocket report --run`
- `api-test` suite cases with `type: "websocket"`

## Top-level object

A request file must be a JSON object. File extension is conventionally `*.ws.json` or
`*.websocket.json`, but the schema parser does not enforce the suffix.

Supported top-level fields:

- `url` (string, optional): explicit WebSocket target. CLI flags (`--url`/`--env`) take
  precedence; the runner errors if neither the request file nor the CLI provides a URL.
- `headers` (object, optional): handshake headers. Values must be scalar (string,
  number, boolean, or null); empty values are dropped. Keys are inserted into the
  client request as-is.
- `connectTimeoutSeconds` (integer or numeric string, optional): handshake-timeout
  ceiling. The runner spawns the connect on a worker thread and aborts the request
  with a `websocket connect timed out after <secs>s` error if the handshake has
  not completed in time.
- `steps` (array, required, non-empty): ordered scripted session steps.
- `expect` (object, optional): assertion against the last received message
  (see "Expect object" below).

Unknown top-level fields are accepted and ignored.

## Step schema

Each `steps[i]` must be a JSON object that includes `type` (case-insensitive after trim).

### `type: "send"`

- Payload field (one of, in resolution order): `text`, `json`, `payload`. At least one
  must be present.
- The chosen value is coerced to a text frame:
  - JSON strings are sent as-is;
  - JSON objects, arrays, numbers, booleans, and `null` are JSON-stringified before send.

### `type: "receive"`

- `timeoutSeconds` (integer or numeric string, optional): per-receive timeout
  applied to the underlying TCP socket via `set_read_timeout` for the duration of
  the step. The error surfaces as `websocket receive timed out after <secs>s at
  step <i>` and the timeout is cleared before the next step runs. When the field
  is absent the read call blocks until the next message.
- `expect` (optional): see "Expect object" below.

### `type: "close"`

- no extra fields required.

## Expect object

Top-level or step-level `expect` supports:

- `textContains` (string): substring match against the received text. The shorter key
  `contains` is also accepted as an alias.
- `jq` (string): jq expression evaluated against the JSON-parsed receive text.

Validation behavior:

- if both are omitted/empty/whitespace-only, the expect block is ignored;
- the top-level `expect` is evaluated against the most recent received message
  (`last_received`); if no `receive` step ran, it is evaluated against an empty string;
- jq assertions fail when the receive text is not valid JSON.

## Frame handling

- Text frames pass through verbatim.
- Binary frames are decoded as lossy UTF-8 before assertion.
- Control frames render as placeholders for transcript and assertion purposes:
  `<PING:<payload>>`, `<PONG:<payload>>`, `<CLOSE:<code>:<reason>>`, `<FRAME>` for
  raw frames. There are no schema options for selectively suppressing these frames;
  scripted `receive` steps observe whichever message arrives next.

## Error behavior

Deterministic schema errors include:

- request file is not valid JSON
- request root is not a JSON object
- `steps` is missing/empty
- unsupported `steps[i].type`
- missing send payload fields

## Fixture matrix

| Fixture | Purpose | Expected outcome |
| --- | --- | --- |
| `health.ws.json` | send/receive success (`jq` true) | pass |
| `expect-fail.ws.json` | receive message fails `textContains`/`jq` | failure with assertion message |
| `invalid-json.ws.json` | malformed JSON file | schema load failure |
| `missing-steps.ws.json` | no `steps` field | schema validation failure |
| `connect-fail.ws.json` | unreachable target URL | connection failure |

## Reusable fixture pattern

A minimal reusable fixture for both CLI and suite tests:

```json
{
  "steps": [
    {"type": "send", "text": "ping"},
    {"type": "receive", "expect": {"jq": ".ok == true"}},
    {"type": "close"}
  ],
  "expect": {"textContains": "ok"}
}
```
