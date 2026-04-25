# api-grpc

## Overview

api-grpc executes JSON-defined gRPC unary requests, prints response bodies to stdout, keeps
optional history, and can generate Markdown reports. Unary execution delegates to the
`grpcurl` backend exposed by `api-testing-core::grpc::runner`.

## Usage

```text
Usage: api-grpc <command> [args]

Commands:
  call             Execute a request file and print the response body to stdout (default)
  history          Print the last (or last N) history entries
  report           Generate a Markdown API test report
  report-from-cmd  Generate a report from a saved `call` snippet
  completion       Print shell completion script

Common options (see subcommand help for full details):
  --config-dir <dir>   Seed setup/grpc discovery (call/history/report)
  -h, --help           Print help

Examples:
  api-grpc --help
  api-grpc call --help
  api-grpc report --help
  api-grpc report-from-cmd --help
  api-grpc completion zsh
```

## Commands

- `call` (default): Execute a request file and print the response body.
  Positional: `<request.grpc.json>`.
  Options: `-e/--env <name>`, `-u/--url <url>`, `--token <name>`, `--config-dir <dir>`,
  `--no-history`.
- `history`: Print the last entry or tail N entries.
  Options: `--config-dir <dir>`, `--file <path>`, `--last`, `--tail <n>`, `--command-only`.
- `report`: Generate a Markdown report for a request.
  Required: `--case <name>`, `--request <file>`. Exactly one of `--run` or `--response <file|->`.
  Options: `--out <path>`, `-e/--env <name>`, `-u/--url <url>`, `--token <name>`,
  `--no-redact`, `--no-command`, `--no-command-url`, `--project-root <path>`, `--config-dir <dir>`.
- `report-from-cmd`: Generate a Markdown report from a saved `call` command snippet.
  Positional: `[snippet]` (or pass via `--stdin`).
  Options: `--case <name>`, `--out <path>`, `--response <file|->`, `--allow-empty`,
  `--dry-run`, `--stdin`.
- `completion`: Print a shell completion script. Argument: `<SHELL>` (`bash` or `zsh`).

## Request file contract

`*.grpc.json` files describe a single unary call. The JSON object is the gRPC request
envelope consumed by `api-testing-core::grpc::schema`:

| Field | Type | Required | Notes |
| --- | --- | --- | --- |
| `method` (alias `rpc`) | string | yes | Fully-qualified `service/method` (e.g. `health.HealthService/Check`). |
| `body` | object | optional | Defaults to `{}`. Must be a JSON object. |
| `metadata` | object | optional | Scalar values (string/number/bool); empty values are dropped; sorted by key. |
| `proto` | string | optional | Path to a `.proto` file (relative to the request file). |
| `importPaths` | string[] | optional | Additional `-import-path` entries (relative paths resolved against the request file). |
| `plaintext` | bool | optional | Defaults to `true`. When `true`, `grpcurl -plaintext` is added. |
| `authority` | string | optional | Forwarded as `grpcurl -authority`. |
| `timeoutSeconds` | integer | optional | Forwarded as `grpcurl -max-time`. |
| `expect.status` | integer | optional | Expected gRPC status code (numeric). |
| `expect.jq` | string | optional | jq-style boolean assertion against the decoded response body. |

Bearer tokens resolved by `--token` (or `GRPC_TOKEN_NAME`) are injected as
`-H 'authorization: Bearer <token>'` unless the request already provides an `authorization`
metadata entry.

## Quickstart

### 1) Setup files

```text
setup/grpc/
  endpoints.env
  tokens.env
  requests/
    health.grpc.json
```

`setup/grpc/endpoints.env`

```bash
GRPC_URL_LOCAL=127.0.0.1:50051
```

`setup/grpc/tokens.env`

```bash
GRPC_TOKEN_DEFAULT=<jwt-or-access-token>
```

`setup/grpc/requests/health.grpc.json`

```json
{
  "method": "health.HealthService/Check",
  "body": {
    "service": "payments"
  },
  "metadata": {
    "x-trace-id": "demo-001"
  },
  "plaintext": true,
  "expect": {
    "status": 0,
    "jq": ".ok == true"
  }
}
```

### 2) Call + history

```bash
api-grpc call --env local --token default setup/grpc/requests/health.grpc.json
api-grpc history --tail 5
```

### 3) Report

```bash
api-grpc report --case grpc-health --request setup/grpc/requests/health.grpc.json --run
api-grpc history --command-only | api-grpc report-from-cmd --stdin --dry-run
```

## Runtime dependency

- `grpcurl` is a required runtime dependency for `api-grpc call` and any suite gRPC cases
  (the unary backend in `api-testing-core::grpc::runner` shells out to it).
- The executable resolves to `grpcurl` on `PATH` by default; set `GRPCURL_BIN` to override
  the path (e.g. when pinning a vendored binary).
- Install:
  - macOS / Linuxbrew: `brew install grpcurl`
- See the workspace-level [`BINARY_DEPENDENCIES.md`](../../BINARY_DEPENDENCIES.md) for the
  canonical runtime-tooling matrix.

## Docs

- [Docs index](docs/README.md)
