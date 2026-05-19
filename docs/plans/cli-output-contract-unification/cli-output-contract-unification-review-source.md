# CLI Output Contract Unification Review Source

- Status: open, ready for implementation planning
- Date: 2026-05-19
- Source: static `/code-review-specialists` audit of the `nils-cli` workspace
  (`main` @ `bf740b5`).
- Scope: workspace-wide standardisation of machine-readable output, exit
  codes, and JSON contract for every user-facing binary.

## Execution

- Recommended plan:
  docs/plans/cli-output-contract-unification/cli-output-contract-unification-plan.md
- Recommended execution state:
  docs/plans/cli-output-contract-unification/cli-output-contract-unification-execution-state.md

## Purpose

Three foundational gaps were observed across ~30 workspace binaries and they
compound: each binary advertises a slightly different way to ask for JSON,
emits JSON with different envelopes, and signals failure with a different
exit code. Once any of these is standardised in isolation it becomes a
breaking change for downstream agents and scripts, so this record groups the
fixes into one coherent contract and sets up the rest of the docs/plans/*
backlog (help, dispatch modernization, destructive-op safety, progress) to
build on top of it.

This document is the source of truth for the unified CLI output contract.
Other plan-source docs in the `cli-*` family should `Read First` this record
when they need the contract envelope or exit-code constants.

## Current Judgment

The workspace is in an "almost there" state: several binaries already model
the right shape (`memo-cli` for `--format`/`--json` conflict detection,
`agent-workflow-primitives` for `schema_version`, `plan-issue-cli` for
`EXIT_USAGE = 2`), but no single binary models all of them and nothing in
`nils-common` exposes them as shared primitives. The most efficient path is
to publish the contract once in `nils-common` and migrate binaries from
highest-traffic to lowest.

## Findings

| Priority | ID | Issue | Evidence | Fix Location | Acceptance |
| --- | --- | --- | --- | --- | --- |
| high | A1 | Three coexisting JSON flag styles (`--json` bool, `--format text/json`, ad-hoc `--output`) | `crates/memo-cli/src/cli.rs:70-75`; `crates/agent-docs/src/cli.rs:60-70`; `crates/image-processing/src/cli.rs:47`; `crates/agent-out/src/cli.rs:50-51`; `crates/agent-scope-lock/src/cli.rs:39-40` | every user-facing binary's clap layer + a shared helper in `nils-common` | `--format text\|json` is the canonical form on every binary; `--json` survives as a hidden alias on prior binaries; new binaries reject `--json` bool at lint time |
| high | B1 | Exit codes diverge (`1` / `2` / `64` / `65` all used for usage error) | `crates/semantic-commit/src/usage.rs:30`; `crates/plan-tooling/src/usage.rs:31`; `crates/git-cli/src/usage.rs:66,74`; `crates/memo-cli/src/errors.rs:23-26`; `crates/plan-issue-cli/src/lib.rs:22-25` | `crates/nils-common/src/exit.rs` (new module) + per-binary call sites | every binary maps usage error → `64`, data error → `65`, runtime error → `1`, software error → `70` |
| medium | A2 | `schema_version` only present on a subset of JSON-emitting commands | `crates/agent-workflow-primitives/src/canary_check.rs:237,141`; `crates/semantic-commit/src/staged_context.rs:280`; `crates/codex-cli/src/diag_output.rs:15`; `crates/api-gql/src/commands/report.rs` (missing) | every JSON-emitting subcommand + a shared envelope type in `nils-common` | every JSON record has a `schema_version` field shaped `cli.<binary>.<command>.v<N>`; snapshot tests assert the literal string |
| medium | A3 | JSON field casing is inconsistent (camelCase in `staged-context`, snake_case elsewhere) | `crates/semantic-commit/src/staged_context.rs:248,254,280` | `staged_context` serializer | all JSON output uses snake_case (`#[serde(rename_all = "snake_case")]`); `staged-context` migrates with a `schema_version` bump |
| medium | A4 | Parse-time errors fall back to plain stderr even when `--json` was requested | `crates/semantic-commit/src/usage.rs:27-30`; `crates/git-cli/src/usage.rs:64-66`; `crates/memo-cli/src/errors.rs:12-18`; `crates/plan-issue-cli/src/lib.rs:97-100` | every binary's main entry point | when `--format json` (or `--json`) is detected on argv, parse and unknown-subcommand errors emit `{ "schema_version": ..., "ok": false, "error": { "code": ..., "message": ... } }` and exit `64` |
| medium | I2 | `schema_version` literal not snapshot-locked | `crates/memo-cli/tests/integration/json_contract.rs:19` (only one binary) | per-binary `tests/integration/json_contract.rs` | one snapshot test per JSON-emitting subcommand pins the exact `schema_version` string |
| medium | B2 | Exit-code matrix coverage is patchy | `crates/semantic-commit/tests/integration/commit.rs` (good); `crates/api-gql/src/main.rs:73`; `crates/memo-cli/tests/integration/json_contract.rs` (partial) | per-binary integration tests | every binary has at least one test per exit code variant (`success`, `usage`, `data`, `runtime`) |
| low | A5 | `memo-cli` warnings go to stderr while JSON goes to stdout — JSON consumers miss them | `crates/memo-cli/src/output/text.rs:135-140`; `crates/memo-cli/src/output/json.rs:119` | `memo-cli` JSON output module; the shared envelope | `--format json` JSON envelope has a `warnings: []` array; text mode keeps stderr behaviour |

## Ownership Boundary

- Runtime: every workspace binary that emits JSON or returns an exit code.
- Shared library: a new `nils-common::cli_contract` module (envelope type,
  exit-code constants, parse-error JSON helper).
- Test/harness: per-binary `tests/integration/json_contract.rs` snapshot
  files.
- Docs: `docs/specs/cli-output-contract-v1.md` (new) — the public contract
  spec that other plans `Read First`.

## Backlog / Next Fixes

1. Publish the envelope and exit-code primitives in `nils-common`.
2. Migrate `memo-cli` first (it already models most of the right shape and
   has the most integration tests).
3. Migrate `agent-workflow-primitives` binaries next (they already use
   `schema_version`; mainly need exit-code alignment and `--format`).
4. Migrate `api-rest` / `api-gql` / `api-grpc` / `api-websocket` / `api-test`
   together (they share `api-testing-core`).
5. Migrate `semantic-commit`, `git-scope`, `git-summary`, `git-lock`
   one-by-one.
6. Backfill snapshot tests as each binary lands.

## Retention Intent

- This source doc is execution coordination — delete `docs/plans/cli-output-contract-unification/`
  once execution completes.
- Promote `docs/specs/cli-output-contract-v1.md` as long-lived spec
  (durable knowledge for future binaries).

## Validation Gate

- `bash scripts/ci/plan-bundle-validate.sh --strict`
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
- Per-binary: `cargo test -p <crate> json_contract`
- Workspace: `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage`

## Do Not Do

- Do not silently drop the `--json` bool flag from binaries that already
  ship it — keep as a hidden alias for one minor version, then remove.
- Do not change the JSON shape for binaries that already publish a
  `schema_version` without bumping the version field.
- Do not standardise exit code `2` (clap's default parse-error code) — clap
  may emit it before our parse-error envelope runs; document this and
  intercept where possible.
- Do not introduce a new top-level `error` schema that differs from
  agent-workflow-primitives' existing `Record { schema_version, ok, ... }`
  shape; reuse it.

## Open Questions

- Should `cli-template` ship the canonical contract as a worked example
  before any production binary migrates? (Recommended: yes; lower-risk to
  validate the envelope on the template first.)
- Where should `schema_version` live in the JSON envelope when the binary
  also emits a single-record payload (e.g. `semantic-commit
  staged-context`)? (Recommended: top-level alongside `ok`/`data`.)
- Do we need a migration-period flag to opt back into legacy exit codes for
  any caller? (Recommended: no; treat as a documented breaking change at
  the next minor version.)
