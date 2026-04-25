# API testing CLIs overview

## Overview

This workspace ships five Rust API testing CLIs (`api-rest`, `api-gql`, `api-grpc`, `api-websocket`, `api-test`) plus the shared library
crate `api-testing-core`. The canonical Rust CLI contracts are the source of truth: flags, defaults, exit codes, and on-disk artifacts
(history files, reports, results).

Detailed command specs live with each binary:

- `crates/api-rest/README.md`
- `crates/api-gql/README.md`
- `crates/api-grpc/README.md`
- `crates/api-websocket/README.md`
- `crates/api-test/README.md`

This README focuses on the shared repository layout, cross-CLI concepts, and the `api-testing-core` surface area.

## Binaries and command surface mapping

- `api-rest` (REST)
  - `api-rest call` (default)
  - `api-rest history`
  - `api-rest report`
- `api-gql` (GraphQL)
  - `api-gql call` (default)
  - `api-gql history`
  - `api-gql report`
  - `api-gql schema`
- `api-test` (suite runner)
  - `api-test run`
  - `api-test summary`
- `api-grpc` (gRPC unary)
  - `api-grpc call` (default)
  - `api-grpc history`
  - `api-grpc report`
  - `api-grpc report-from-cmd`
- `api-websocket` (WebSocket scripted sessions)
  - `api-websocket call` (default)
  - `api-websocket history`
  - `api-websocket report`
  - `api-websocket report-from-cmd`

Notes:

- `api-test` executes REST/GraphQL/gRPC/WebSocket cases through shared core runners (no shelling out to scripts or the binaries).
- `api-rest`/`api-gql`/`api-grpc`/`api-websocket` also provide `report-from-cmd` as a Rust-only convenience for replaying saved `call`
  snippets.

## api-testing-core scope

`api-testing-core` is a library crate used by all five CLIs.

### Public modules

The `lib.rs` re-exports the following top-level modules (see `crates/api-testing-core/src/lib.rs`):

- `auth_env`: profile/env-fallback bearer-token resolution shared by REST/gRPC/WebSocket.
- `cli_endpoint`: env-preset / `--url` resolution with `endpoints.env` lookup.
- `cli_history`: history-file path resolution, append, and `history` subcommand rendering helpers.
- `cli_io`: shared CLI I/O helpers (read response file/stdin, conditional stderr body printing).
- `cli_report`: report metadata/path builder for the `report` subcommand.
- `cli_util`: misc helpers (env-key normalization, slugify, timestamps, `WarnSink`).
- `cmd_snippet`: parser for saved `call` command snippets used by `report-from-cmd`.
- `config`: setup-dir discovery for REST/GraphQL/gRPC/WebSocket configs (resolves history defaults).
- `env_file`: `.env` parsing with last-wins precedence and `.local.env` overrides.
- `graphql`: schema/vars loading, auth/JWT resolution, runner, expect / allow-errors, report rendering, mutation detection.
- `grpc`: unary request schema, transport runner (delegates to `grpcurl`), expect logic, report rendering.
- `history`: rotating append-only history writer + path resolver.
- `http`: placeholder HTTP executor (currently unimplemented; REST runner uses `nils_common` HTTP).
- `jq`: in-process `jq`-style query helper backed by `jaq`.
- `jwt`: JWT validation helpers (exp/nbf checks with leeway, strict/non-strict modes).
- `markdown`: Markdown rendering helpers (headings, code blocks, JSON pretty-print).
- `redact`: secret-key redaction for response bodies and report payloads.
- `report`: shared report-builder primitives (header / sections).
- `rest`: request schema, runner, expect / cleanup logic, report rendering.
- `suite`: suite schema v1, path resolution, filters, safety gates, auth integration, runner, cleanup, results, summary, JUnit (covers
  REST / GraphQL / gRPC / WebSocket case types).
- `websocket`: request schema, scripted runner (native Rust `tungstenite`), expect logic, report rendering.

## Transport decision and reuse matrix

- Transport decision:
  - gRPC selected: `grpcurl` adapter for unary MVP (`api-testing-core::grpc::runner`).
  - gRPC rejected for MVP: native dynamic invocation path (higher complexity for the same unary delivery goal).
  - WebSocket selected: native Rust `tungstenite` transport (`api-testing-core::websocket::runner`).
- Reuse matrix:
  - unchanged: suite selection/filtering, run directory/artifact envelope, summary/JUnit/results rendering.
  - additive grpc: suite schema defaults/case validation, `type: grpc` runner branch, gRPC endpoint/token resolution.
  - additive websocket: suite schema defaults/case validation, `type: websocket` runner branch, WS endpoint/token resolution.
- Evidence commands:
  - `cargo test -p nils-api-testing-core --test suite_rest_graphql_matrix`
  - `cargo test -p nils-api-testing-core --test suite_runner_loopback`
  - `cargo test -p nils-api-testing-core --test suite_runner_grpc_matrix`
  - `cargo test -p nils-api-testing-core --test suite_runner_websocket_matrix`
  - `cargo test -p nils-api-test suite_schema`

## Shared terminology

- Setup dir
  - Protocol-specific config directory.
  - Canonical locations: REST `setup/rest`, GraphQL `setup/graphql`, gRPC `setup/grpc`, WebSocket `setup/websocket`.
- Config dir
  - CLI arg `--config-dir <dir>` that seeds setup-dir discovery.
  - Discovery searches upward for known config files; fallback uses the canonical `setup/<tool>` when applicable.
- Env preset
  - `--env <name>` selects a base URL from `endpoints.env` (+ optional `.local` overrides).
  - REST: `REST_URL_<ENV_KEY>`; GraphQL: `GQL_URL_<ENV_KEY>`; gRPC: `GRPC_URL_<ENV_KEY>`.
  - WebSocket: `WS_URL_<ENV_KEY>`.
  - If the value looks like `http(s)://...` or `ws(s)://...`, it is treated as a direct URL (like `--url`).
- Token/JWT profile
  - REST: `--token <name>` or `REST_TOKEN_NAME` selects `REST_TOKEN_<NAME>`.
  - GraphQL: `--jwt <name>` or `GQL_JWT_NAME` selects `GQL_JWT_<NAME>`.
  - gRPC: `--token <name>` or `GRPC_TOKEN_NAME` selects `GRPC_TOKEN_<NAME>`.
  - WebSocket: `--token <name>` or `WS_TOKEN_NAME` selects `WS_TOKEN_<NAME>`.
  - REST fallback: `ACCESS_TOKEN`, then `SERVICE_TOKEN` if no profile is selected.
  - GraphQL fallback: `ACCESS_TOKEN`, then `SERVICE_TOKEN` if no profile is selected.
- History
  - REST: `<setup_dir>/.rest_history`, GraphQL: `<setup_dir>/.gql_history`, gRPC: `<setup_dir>/.grpc_history`, WebSocket:
    `<setup_dir>/.ws_history`.
  - Enabled by default, can be disabled, and supports rotation/size limits.
- Report
  - Markdown artifact (usually under `<repo>/docs/`) capturing request/operation, response, and optional assertions.
  - Redacts common secret fields by default, with opt-out flags for debugging.
- Suite
  - JSON manifest (`version: 1`) that drives `api-test run` and defines cases, defaults, auth, and cleanup.

## Canonical repo layouts

The CLIs support the following repository layouts.

### Layout A: App repo with `setup/` + `tests/` (recommended)

```text
<repo>/
  setup/
    rest/
      endpoints.env
      endpoints.local.env        # optional (local override)
      tokens.env
      tokens.local.env           # optional (local tokens; do not commit)
    graphql/
      endpoints.env
      endpoints.local.env        # optional (local override)
      jwts.env
      jwts.local.env             # optional (local tokens; do not commit)
      schema.env                 # optional (sets GQL_SCHEMA_FILE)
      schema.local.env           # optional (local override)
      schema.graphql             # or: schema.gql / schema.graphqls / api.graphql / api.gql
      operations/                # optional (login.graphql, shared ops, etc.)
    grpc/
      endpoints.env
      endpoints.local.env        # optional (local override)
      tokens.env
      tokens.local.env           # optional (local tokens; do not commit)
      requests/                  # *.grpc.json request definitions
    websocket/
      endpoints.env
      endpoints.local.env        # optional (local override)
      tokens.env
      tokens.local.env           # optional (local tokens; do not commit)
      requests/                  # *.ws.json or *.websocket.json request definitions
  tests/
    api/
      suites/
        <name>.suite.json
  out/
    api-test-runner/             # suite runner output base dir
```

### Layout B: Suites under `setup/` (fallback)

`api-test` resolves `--suite <name>` to:

- `<repo>/tests/api/suites/<name>.suite.json` (preferred)
- `<repo>/setup/api/suites/<name>.suite.json` (fallback)

### Layout C: Custom suites directory

- `API_TEST_SUITES_DIR=<path>` overrides the suites directory for `--suite <name>`.

## Quickstart examples

### `api-rest`

Run a request:

```bash
api-rest call --env staging setup/rest/requests/health.request.json
```

Write a report:

```bash
api-rest report --case health --request setup/rest/requests/health.request.json --run
```

Generate a report from a saved snippet:

```bash
api-rest history --command-only | api-rest report-from-cmd --stdin
```

### `api-gql`

Run an operation:

```bash
api-gql call --env staging setup/graphql/operations/health.graphql
```

Write a report:

```bash
api-gql report --case health --op setup/graphql/operations/health.graphql --run
```

Resolve and print schema:

```bash
api-gql schema --cat
```

### `api-grpc`

Run a unary request:

```bash
api-grpc call --env staging setup/grpc/requests/health.grpc.json
```

Write a report:

```bash
api-grpc report --case health --request setup/grpc/requests/health.grpc.json --run
```

Generate a report from a saved snippet:

```bash
api-grpc history --command-only | api-grpc report-from-cmd --stdin
```

### `api-test`

Run a suite (always emits results JSON to stdout):

```bash
api-test run --suite smoke
```

Use an explicit suite file:

```bash
api-test run --suite-file tests/api/suites/smoke.suite.json
```

Write results JSON + JUnit:

```bash
api-test run --suite smoke --out out/api-test-runner/results.json --junit out/api-test-runner/junit.xml
```

Render a Markdown summary:

```bash
api-test summary --in out/api-test-runner/results.json --out out/api-test-runner/summary.md
```

### `api-websocket`

Run a scripted request:

```bash
api-websocket call --env staging setup/websocket/requests/health.ws.json
```

Write a report:

```bash
api-websocket report --case health --request setup/websocket/requests/health.ws.json --run
```

Generate a report from a saved snippet:

```bash
api-websocket history --command-only | api-websocket report-from-cmd --stdin
```

## Suite runner behavior (high-level)

- Results JSON is always emitted to stdout. `--out` writes an additional copy.
- Output directory base defaults to `<repo>/out/api-test-runner` (override via `API_TEST_OUTPUT_DIR`).
- Each run creates `<output_dir>/<run_id>/` where `run_id` is `YYYYMMDD-HHMMSSZ`.
- Per-case artifacts include `<case>.response.json` and `<case>.stderr.log`, referenced in results JSON.
- Exit code is `2` when any case fails; otherwise `0`.
- Write safety is two-step:
  - Case must set `allowWrite: true`.
  - Writes must be enabled via `--allow-writes`, `API_TEST_ALLOW_WRITES_ENABLED=true`, or `env: local`.
- Optional suite auth can derive tokens from secret JSON in `API_TEST_AUTH_JSON`.
- Optional cleanup steps run after the main case; cleanup failures mark the case failed.

## Suite runner environment variables

- `API_TEST_OUTPUT_DIR`: override the base output directory.
- `API_TEST_SUITES_DIR`: override the suites directory used by `--suite`.
- `API_TEST_ALLOW_WRITES_ENABLED`: enable write-capable cases.
- `API_TEST_REST_URL`: override REST base URL for all REST/rest-flow cases.
- `API_TEST_GQL_URL`: override GraphQL endpoint URL for all GraphQL cases.
- `API_TEST_GRPC_URL`: override gRPC target for all gRPC cases.
- `API_TEST_WS_URL`: override WebSocket target URL for all websocket cases.
- `API_TEST_AUTH_JSON`: credentials JSON used by suite auth (default key name).
- `GITHUB_STEP_SUMMARY`: when set, `api-test summary` appends Markdown output (disable via `--no-github-summary`).

## Docs

- [Docs index](docs/README.md)
