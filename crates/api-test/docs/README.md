# api-test Docs Index

## Specs

- None yet. Add documents under `docs/specs/` and register them here.

## Runbooks

- None yet. Add documents under `docs/runbooks/` and register them here.

## Reports

- None yet. Add documents under `docs/reports/` and register them here.

## Cross-references

- Suite manifest schema, dispatch loop, results envelope, and JUnit
  rendering live in `api-testing-core`. Source of truth modules:
  - `api_testing_core::suite::schema` — manifest (`SuiteManifest`,
    `SuiteCase`, `SuiteDefaults*`, `SuiteCleanup`).
  - `api_testing_core::suite::runner` — per-backend dispatch and
    `SuiteRunOptions` plumbing (REST / REST-flow / GraphQL / gRPC /
    WebSocket).
  - `api_testing_core::suite::results` — results JSON envelope
    (`SuiteRunResults`, `SuiteCaseResult`, `SuiteRunSummary`).
  - `api_testing_core::suite::junit` — JUnit XML rendering.
  - `api_testing_core::suite::summary` — Markdown summary used by
    `api-test summary`.
- Per-backend READMEs:
  - [`api-rest`](../../api-rest/README.md)
  - [`api-gql`](../../api-gql/README.md)
  - [`api-grpc`](../../api-grpc/README.md)
  - [`api-websocket`](../../api-websocket/README.md)
- Shared library docs index:
  [`api-testing-core/docs/README.md`](../../api-testing-core/docs/README.md).

## Links

- Back to crate README: [`../README.md`](../README.md)
