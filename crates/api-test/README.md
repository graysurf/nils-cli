# api-test

## Overview

`api-test` is the suite orchestrator for the api-testing stack. It loads a v1
suite manifest, dispatches each case to the matching backend runner
(REST / GraphQL / gRPC / WebSocket), captures results JSON to stdout, and
optionally writes a JUnit XML file plus a Markdown summary.

The dispatch + run loop is implemented by `api_testing_core::suite::runner`;
JUnit rendering by `api_testing_core::suite::junit`; results by
`api_testing_core::suite::results::{SuiteRunResults, SuiteCaseResult}`. See
[`crates/api-testing-core/README.md`](../api-testing-core/README.md) for the
shared library surface.

## Usage

```text
Usage: api-test <command> [args]

Commands:
  run         Run a suite (default)
  summary     Render a Markdown summary from results JSON
  completion  Print shell completion script

Common options (run; see `api-test run --help` for full details):
  --suite <name>        Resolve suite under tests/api/suites/<name>.suite.json
  --suite-file <path>   Explicit suite file path
  --tag <tag>           Filter cases by tag (repeatable; AND semantics)
  --only <csv>          Run only listed case IDs (comma-separated)
  --skip <csv>          Skip listed case IDs (comma-separated)
  --fail-fast           Stop after first failure
  --out <path>          Write results JSON to a file (stdout still emits JSON)
  --junit <path>        Write optional JUnit XML to a file
  -h, --help            Print help

Examples:
  api-test --help
  api-test --suite smoke --help
  api-test run --suite smoke --out results.json
  api-test completion zsh
```

The first positional `run` is implicit: bare flag invocations
(e.g. `api-test --suite smoke`) are rewritten to
`api-test run --suite smoke` before parsing. Explicit `run` / `summary` /
`completion` words are honored as-is.

## Commands

- `run` (default): execute a suite and write results JSON to stdout.
  Required: exactly one of `--suite <name>` or `--suite-file <path>`.
  Options: `--out <path>`, `--junit <path>`, `--allow-writes`,
  `--tag <tag>` (repeatable; AND semantics), `--only <csv>`, `--skip <csv>`,
  `--fail-fast` / `--continue` (mutually exclusive; `--continue` is the
  default).
- `summary`: render a Markdown summary from a results JSON document
  (stdin by default, or `--in <path>`).
  Options: `--out <path>`, `--slow <n>` (default `5`), `--hide-skipped`,
  `--max-failed <n>` (default `50`), `--max-skipped <n>` (default `50`),
  `--no-github-summary` (skip writing to `$GITHUB_STEP_SUMMARY`).
- `completion`: print a shell completion script. Argument: `<SHELL>`
  (`bash` or `zsh`).

`run` exit codes (driven by `SuiteRunResults::exit_code`):

- `0` — every case `passed` or `skipped`.
- `2` — at least one case `failed`.
- `1` — orchestrator-level error (suite not found, schema invalid,
  results serialization failure, JUnit write failure).

## Suite manifest (`v1`)

Top-level fields (camelCase):

- `version` (required, must be `1`).
- `name` (optional; falls back to the suite filename).
- `defaults` (optional; per-protocol overrides applied to each case).
- `auth` (optional; shared login flow that produces a bearer token reused
  by REST/GraphQL cases — see `api-testing-core` docs for details).
- `cases` (required, ordered list of case objects).

`defaults` accepts one block per backend (every block is independently
optional):

| Block | Fields |
| --- | --- |
| `defaults.rest` | `configDir` (default `setup/rest`), `url`, `token` |
| `defaults.graphql` | `configDir` (default `setup/graphql`), `url`, `jwt` |
| `defaults.grpc` | `configDir` (default `setup/grpc`), `url`, `token` |
| `defaults.websocket` | `configDir` (default `setup/websocket`), `url`, `token` |

`url` is either an endpoint preset name resolved against the backend's
`endpoints.env` or a literal URL/target; `token` / `jwt` is a profile name
resolved against the backend's token store. Suite-level `defaults.env`
and `defaults.noHistory` apply to every case unless overridden inline.

## Case dispatch

Each case object selects a backend via the `type` field. The runner
normalizes `ws` to `websocket` before dispatch (see
`api_testing_core::suite::runner::context::case_type_normalized`), so both
spellings are accepted.

| `type` value | Backend runner | Required case field | Notes |
| --- | --- | --- | --- |
| `rest` | `api-rest` (in-process via `api_testing_core::rest`) | `request` (path to `*.request.json`) | Optional: `env`, `url`, `token`, `configDir`, `noHistory`, `allowWrite`, `cleanup`. |
| `rest-flow` (alias `rest_flow`) | `api-rest` login + main request | `loginRequest` and `request` | Optional: `tokenJq` (jq filter that extracts the bearer token from the login response; defaults to a permissive `accessToken`/`access_token`/`token` selector). |
| `graphql` | `api-gql` (in-process via `api_testing_core::graphql`) | `op` (path to `*.graphql`) | Optional: `vars` (path to JSON variables), `jwt`, `allowErrors`, `expect.jq` (required when `allowErrors=true`). |
| `grpc` | `api-grpc` (in-process via `api_testing_core::grpc`) | `request` (path to `*.grpc.json`) | Optional: `env`, `url`, `token`, `configDir`, `noHistory`, plus gRPC-only fields handled by the request file (`grpcProto`, `grpcImportPaths`, `grpcPlaintext`). |
| `websocket` (alias `ws`) | `api-websocket` (in-process via `api_testing_core::websocket`) | `request` (path to `*.ws.json` or `*.websocket.json`) | Optional: `env`, `url`, `token`, `configDir`, `noHistory`. |

Cases are filtered before dispatch using `--tag` (AND semantics across
repeats), `--only`, `--skip`, and the `allowWrite` safety gate. A case
that opts into writes (`allowWrite: true`) only runs when `--allow-writes`
is set, or when the suite-level
`API_TEST_ALLOW_WRITES_ENABLED` env var is truthy.

## Mixed protocol suite example

```json
{
  "version": 1,
  "name": "smoke",
  "defaults": {
    "rest": {"configDir": "setup/rest"},
    "graphql": {"configDir": "setup/graphql", "jwt": "default"},
    "grpc": {"configDir": "setup/grpc", "url": "local", "token": "default"},
    "websocket": {"configDir": "setup/websocket", "url": "local", "token": "default"}
  },
  "cases": [
    {"id": "rest-health", "type": "rest", "request": "setup/rest/requests/health.request.json"},
    {"id": "gql-health", "type": "graphql", "op": "setup/graphql/operations/health.graphql"},
    {"id": "grpc-health", "type": "grpc", "request": "setup/grpc/requests/health.grpc.json"},
    {"id": "ws-health", "type": "websocket", "request": "setup/websocket/requests/health.ws.json"}
  ]
}
```

## Results JSON envelope

Stdout is exactly one JSON object per `run` invocation, also written to
`--out <path>` when supplied:

```json
{
  "version": 1,
  "suite": "smoke",
  "suiteFile": "tests/api/suites/smoke.suite.json",
  "runId": "20260131-000000Z",
  "startedAt": "2026-01-31T00:00:00Z",
  "finishedAt": "2026-01-31T00:00:01Z",
  "outputDir": "out/api-test-runner/20260131-000000Z",
  "summary": {"total": 4, "passed": 3, "failed": 1, "skipped": 0},
  "cases": [
    {
      "id": "rest-health",
      "type": "rest",
      "status": "passed",
      "durationMs": 12,
      "tags": [],
      "stdoutFile": "out/api-test-runner/.../rest-health.response.json",
      "stderrFile": "out/api-test-runner/.../rest-health.stderr.log"
    }
  ]
}
```

Per-case fields (camelCase, optional fields are omitted when empty):

- `status` is one of `passed`, `failed`, `skipped`.
- `type` is the normalized dispatch value (`ws` collapses to `websocket`).
- `command` carries the rendered backend command snippet (REST/GraphQL
  emit the equivalent `api-rest call` / `api-gql call` invocation; gRPC
  and WebSocket emit the in-process equivalent).
- `message` is set on `failed`/`skipped` cases (e.g. `cleanup_failed`,
  `rest_flow_login_failed`, the selection-skip reason, or the runner's
  own diagnostic).
- `assertions` mirrors per-runner expect output (currently surfaced by
  GraphQL expect/allow-errors flow and gRPC/WebSocket assertions).
- `stdoutFile` / `stderrFile` are repo-relative paths under `outputDir`.

The output directory defaults to `<repo>/out/api-test-runner/<runId>/`
and can be overridden with the `API_TEST_OUTPUT_DIR` env var (relative
paths resolve against the repo root).

## JUnit XML output

`--junit <path>` writes a single `<testsuite>` element per run via
`api_testing_core::suite::junit::render_junit_xml`. The shape is:

```xml
<?xml version="1.0" encoding="utf-8"?>
<testsuite name="smoke" tests="4" failures="1" skipped="0">
  <testcase name="rest-health" classname="rest" time="0.012"/>
  <testcase name="gql-health" classname="graphql" time="0.034">
    <failure message="graphql_runner_failed">command: api-gql call ...
stdoutFile: out/.../gql-health.response.json
stderrFile: out/.../gql-health.stderr.log</failure>
  </testcase>
  <testcase name="ws-skip" classname="websocket" time="0.000"><skipped message="filtered_by_only"/></testcase>
</testsuite>
```

Notes:

- `<testsuite>` attributes: `name` (suite name or filename), `tests`,
  `failures`, `skipped` (taken from `summary`).
- `<testcase>` attributes: `name` = case `id`, `classname` = normalized
  case `type`, `time` = `durationMs / 1000` formatted to 3 decimals.
- `<failure>` carries the runner `message` and a body listing
  `command`, `stdoutFile`, `stderrFile` when present (XML-escaped).
- `<skipped>` carries the skip reason in `message`.
- Passed cases emit `<testcase ... />` with no child element.

## Environment variables

| Variable | Effect |
| --- | --- |
| `API_TEST_OUTPUT_DIR` | Override the per-run output directory base (default `<repo>/out/api-test-runner`). Relative values resolve against the repo root. |
| `API_TEST_PROGRESS` | `auto` (default), `on`/`1`/`true`/`yes`, or `off`/`0`/`false`/`no`. Controls the stderr progress bar. |
| `API_TEST_ALLOW_WRITES_ENABLED` | Truthy value forces `--allow-writes` on for every case (still gated by per-case `allowWrite: true`). |
| `API_TEST_REST_URL` | Overrides REST endpoint resolution for suite runs. |
| `API_TEST_GQL_URL` | Overrides GraphQL endpoint resolution for suite runs. |
| `API_TEST_GRPC_URL` | Overrides gRPC target resolution for suite runs. |
| `API_TEST_WS_URL` | Overrides WebSocket target resolution for suite runs. |
| `API_TEST_AUTH_JSON` | Default secret-env name for `auth.secretEnv` (overridable in the manifest). |
| `GITHUB_STEP_SUMMARY` | When set and `summary --no-github-summary` is not passed, the rendered Markdown summary is appended to this file. |

## Reuse matrix (unchanged vs additive protocol paths)

| Capability | Status | Evidence |
| --- | --- | --- |
| suite selection/filtering | unchanged | `cargo test -p nils-api-testing-core --test suite_rest_graphql_matrix` |
| run/result envelope | unchanged | `cargo test -p nils-api-testing-core --test suite_runner_loopback` |
| summary/JUnit generation | unchanged | `cargo test -p nils-api-testing-core suite::summary suite::junit` |
| grpc protocol dispatch | additive grpc | `cargo test -p nils-api-testing-core --test suite_runner_grpc_matrix` |
| websocket protocol dispatch | additive websocket | `cargo test -p nils-api-testing-core --test suite_runner_websocket_matrix` |
| suite schema validation | additive | `cargo test -p nils-api-test suite_schema` |
| env override wiring | additive | `cargo test -p nils-api-testing-core suite::runtime_tests` |

## Docs

- [Docs index](docs/README.md)
- Sibling backend crates: [`api-rest`](../api-rest/README.md),
  [`api-gql`](../api-gql/README.md), [`api-grpc`](../api-grpc/README.md),
  [`api-websocket`](../api-websocket/README.md).
- Shared library: [`api-testing-core`](../api-testing-core/README.md).
