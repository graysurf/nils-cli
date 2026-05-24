# CLI Output Contract v1

## Purpose

This spec is the single source of truth for how every binary in the
`nils-cli` workspace renders machine-readable output and signals failure
to its callers. The contract is implemented by the
`nils_common::cli_contract` module and exercised end-to-end by
`crates/cli-template` as the reference implementation.

Goals:

- one shape for every JSON-emitting subcommand (`Envelope<T>`);
- one set of exit codes (BSD sysexits-aligned);
- one path for parse and unknown-subcommand errors that respects
  `--format json`;
- a documented deprecation path for pre-contract `--json` boolean flags
  so downstream agents and scripts do not break during the migration.

## Scope

Apply this contract to every user-facing binary in the workspace:

- New binaries MUST adopt the contract before they ship.
- Existing binaries SHOULD migrate within one minor release cycle. The
  rollout plan lives in
  `docs/plans/cli-output-contract-unification/cli-output-contract-unification-plan.md`.
- Binaries that already publish a `schema_version` (notably
  `agent-workflow-primitives`) keep their string literals byte-stable;
  only the surrounding envelope changes when they migrate.

This spec supersedes the per-field shape in
`docs/specs/cli-service-json-contract-guideline-v1.md`: the older
guideline kept a top-level `command` field and used
`result` / `results`. The new envelope drops `command` (the binary +
schema version already disambiguate the source) and uses a single `data`
field plus a top-level `warnings` array. The retry / fallback guidance,
partial-failure handling, and sensitive-data policy from that older
guideline still apply unchanged.

## Output format

- The canonical flag is `--format text|json` (rendered through
  `nils_common::cli_contract::OutputFormat`'s `clap::ValueEnum`).
- Binaries that previously shipped `--json` MAY keep it as a hidden alias
  (`#[arg(long, global = true, hide = true, conflicts_with = "format")]`)
  for one minor cycle, after which it MUST be removed.
- New binaries MUST NOT introduce a `--json` boolean flag. The lint
  script at `scripts/ci/cli-output-contract-lint.sh` (added in Sprint 3
  of the migration plan) enforces this on PRs.
- Help output MUST advertise `--format` only; the hidden alias does not
  appear in `--help`.

## Envelope

Every JSON-emitting subcommand renders a single envelope object to
stdout. Multi-record commands wrap their list inside `data`; this keeps
the wire shape uniform regardless of payload cardinality.

```jsonc
{
  "schema_version": "cli.<binary>.<command>.v<N>",
  "ok": true,
  "data": { /* per-subcommand payload */ },
  "warnings": ["optional", "non-fatal", "messages"]
}
```

Rules:

- Field casing is snake_case across the whole envelope and every payload
  body (`#[serde(rename_all = "snake_case")]` where applicable).
- `schema_version` is the literal string
  `cli.<binary>.<command>.v<N>`. The helper
  `nils_common::cli_contract::schema_version_for` builds it.
- `ok` mirrors success/failure regardless of payload presence.
- `data` is omitted when there is no payload (Serde
  `skip_serializing_if = "Option::is_none"`).
- `warnings` is omitted when empty. JSON consumers see what text mode
  would print to stderr.
- `error` (failure only) is described below.

Failure envelope:

```jsonc
{
  "schema_version": "cli.<binary>.<command>.v<N>",
  "ok": false,
  "error": {
    "code": "stable-machine-code",
    "message": "human-readable summary",
    "hint": "optional follow-up suggestion",
    "details": { /* optional machine-readable structured payload */ }
  }
}
```

The error `code` is stable and machine-usable; the `message` is a single
line; the optional `hint` is human-only; the optional `details` carries
machine-readable structured payload (e.g. the offending payload path or
per-item validation errors). When richer per-item detail is needed for
partial-failure flows, prefer extending `details` over inventing
crate-specific top-level fields. Free-form text never belongs anywhere
in the error envelope.

Provider aggregation commands that support strict partial-failure modes SHOULD
put provider status rows under `error.details.providers[]` when failing. This
keeps the failure shape machine-readable without returning a success `data`
payload, and lets automation distinguish healthy, failed, timed-out, stale, or
skipped providers from the same provider-row contract used in non-strict
success envelopes.

Reference implementation:
[`nils_common::cli_contract::Envelope`](../../crates/nils-common/src/cli_contract.rs)
and the worked example in
[`crates/cli-template/src/main.rs`](../../crates/cli-template/src/main.rs).

## Exit codes

Every binary returns one of the BSD sysexits-aligned constants exposed by
`nils_common::cli_contract::exit`:

| Constant      | Value | When                                                             |
| ------------- | ----- | ---------------------------------------------------------------- |
| `SUCCESS`     | `0`   | Subcommand finished without error.                               |
| `RUNTIME`     | `1`   | Generic runtime failure (the historic catch-all).                |
| `USAGE`       | `64`  | `EX_USAGE`: bad CLI syntax, missing arg, unknown subcommand.     |
| `DATA`        | `65`  | `EX_DATAERR`: input data malformed or otherwise invalid.         |
| `UNAVAILABLE` | `69`  | `EX_UNAVAILABLE`: a required service or resource is unavailable. |
| `SOFTWARE`    | `70`  | `EX_SOFTWARE`: internal software error / invariant violation.    |

Rules:

- Binaries MUST NOT inline numeric exit literals for usage errors. Call
  the shared constants so `cargo grep std::process::exit\(64\)` stays
  empty post-migration.
- Each binary MUST ship at least one exit-code matrix test covering
  `success`, `usage`, `data`, and `runtime` paths.
- Clap's default unmatched-flag/value exit code is `2`. This contract
  does not standardise on `2`; instead the parse-error path below
  intercepts that case before clap calls `exit()` and remaps it to
  `USAGE = 64`.

## Parse and unknown-subcommand errors

`clap` renders parse errors before any subcommand runs. To make `--format
json` work for those errors too, every binary's `main` MUST follow this
pattern:

1. Call `Cli::try_parse()` (instead of `parse()`).
2. Let clap handle informational exits unchanged:
   - `ErrorKind::DisplayHelp` → clap's `err.exit()`.
   - `ErrorKind::DisplayVersion` → clap's `err.exit()`.
   - `ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand` → clap's
     `err.exit()`.
3. For every other error kind, detect the output format from raw argv
   (NOT from the parsed flags, which are not available yet) and call
   `nils_common::cli_contract::emit_parse_error(binary, format, code,
   message)`. The helper writes:
   - In JSON mode: a single-line envelope on stdout with
     `schema_version = "cli.<binary>.error.v1"`, `ok = false`, and
     `error = { code, message }`.
   - In text mode: the historical `error: <msg>` line on stderr.
4. Exit with the value the helper returned (`exit::USAGE = 64`).

Stable error codes for the parse path:

| `error.code`         | Trigger                                                        |
| -------------------- | -------------------------------------------------------------- |
| `parse-error`        | Any clap error that is not an unknown-subcommand or info exit. |
| `unknown-subcommand` | `ErrorKind::InvalidSubcommand`.                                |

The worked example is in
[`crates/cli-template/src/main.rs`](../../crates/cli-template/src/main.rs)
(see `parse_or_exit` and `detect_format_from_argv`).

## Deprecation: `--json` boolean flag

The workspace converges on `--format text|json`. For binaries that
previously shipped `--json`:

- Keep `--json` as a hidden alias for one minor release cycle:
  `#[arg(long, global = true, hide = true, conflicts_with = "format")]`.
- The clap `conflicts_with` declaration makes `--json --format text`
  fail at parse time (exit `64`) instead of relying on a runtime check.
- After the minor cycle, remove the alias. The contract lint script
  fails any new binary that re-introduces `--json` without `hide = true`.

`agent-workflow-primitives` and `staged-context` carry their own
schema-version aliases; their deprecation timelines are documented in
their respective migration tasks. See
`docs/plans/cli-output-contract-unification/cli-output-contract-unification-plan.md`.

## Testing requirements

Per binary:

- One JSON snapshot test per JSON-emitting subcommand. The test MUST
  pin the literal `schema_version` string so any drift fails fast.
- One exit-code matrix test covering `success`, `usage`, `data`, and at
  least one of `runtime` / `unavailable` / `software` depending on the
  binary's failure modes.

Per workspace (covered by the migration plan's Sprint 3 lint script):

- `scripts/ci/cli-output-contract-lint.sh` fails on:
  - any new `--json` boolean flag without `hide = true`;
  - any `std::process::exit(1|2)` literal for usage errors in `main.rs`;
  - any JSON serializer that emits camelCase outside the documented
    aliases.

## Cross-references

- Implementation:
  [`crates/nils-common/src/cli_contract.rs`](../../crates/nils-common/src/cli_contract.rs).
- Reference binary:
  [`crates/cli-template`](../../crates/cli-template).
- Migration plan:
  [`docs/plans/cli-output-contract-unification/cli-output-contract-unification-plan.md`](../plans/cli-output-contract-unification/cli-output-contract-unification-plan.md).
- Shared-crate boundary rules:
  [`docs/specs/workspace-shared-crate-boundary-v1.md`](workspace-shared-crate-boundary-v1.md).
- Predecessor guideline (retry / partial-failure / sensitive-data
  policy still applies):
  [`docs/specs/cli-service-json-contract-guideline-v1.md`](cli-service-json-contract-guideline-v1.md).
