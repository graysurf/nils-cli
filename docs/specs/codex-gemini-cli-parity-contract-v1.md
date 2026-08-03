# codex/gemini CLI parity contract v1

## Purpose

This document is the canonical parity contract for `nils-codex-cli` and `nils-gemini-cli` after core-crate consolidation into CLI adapters
plus `nils-common::provider_runtime`.

## Topology parity

Both binaries must expose the same top-level command topology:

- `agent`
- `auth`
- `diag`
- `config`
- `prompt-segment`
- `completion`

Shared help behavior:

- `--help`/`help` exit code remains `0`.
- Unknown groups/subcommands return deterministic usage errors (`64`) with lane-specific binary prefixes.

## Invalid-command parity

Non-canonical command invocations must preserve equal exit semantics between lanes:

- Unknown top-level groups
- Unknown subcommands under canonical groups

Parity requirement: for equivalent invocation shapes above, `codex-cli` and `gemini-cli` must return identical exit code classes.

## JSON contract parity

Structure parity is required while schema ids remain provider-specific.

- Auth command family:
  - Codex schema id prefix: `codex-cli.auth.v1`
  - Gemini schema id prefix: `gemini-cli.auth.v1`
- Diag command family:
  - Codex schema id prefix: `codex-cli.diag.rate-limits.v1`
  - Gemini schema id prefix: `gemini-cli.diag.rate-limits.v1`

Compatibility rules:

1. Required fields and envelope shape remain aligned across both lanes.
2. Provider labels and schema ids remain lane-specific.
3. No secret-bearing fields are emitted in error envelopes.

## Runtime adapter invariants

1. Provider-specific env key names, default model values, path precedence, and dangerous-exec command shape are configured via provider
   profiles.
2. Provider-specific exec flags may diverge when the upstream provider CLI capabilities differ (for example Codex supports optional
   `--ephemeral`; Gemini currently has no equivalent).
3. Shared runtime primitives (`auth/json/jwt/error/path/config/exec` logic) stay in `nils-common::provider_runtime`.
4. Human output text and exit semantics stay stable for existing commands.

## Concurrency guardrails

### Gemini prompt-segment background-refresh parity

- `--refresh` remains the only blocking refresh path in both lanes. The default prompt path must keep printing the cached line first and
  keep appending the lane-specific stale suffix when the cache is stale.
- Current state: both `codex-cli` and `gemini-cli` call `refresh::enqueue_background_refresh(&target_file)` from the default
  stale/missing-cache path (see `crates/codex-cli/src/prompt_segment/mod.rs` and `crates/gemini-cli/src/prompt_segment/mod.rs`). Inline
  `refresh_blocking()` is reserved for the explicit `--refresh` path in both lanes.
- The parity guardrails below are required invariants for both lanes; treat any future regression as a contract break:
  - Best-effort background spawn via the current executable; `current_exe` or spawn failures stay silent no-ops.
  - Minimum refresh interval throttling via `*_PROMPT_SEGMENT_REFRESH_MIN_SECONDS`.
  - Last-attempt markers are written before `spawn()` so prompt storms still throttle after child-launch failure.
  - Lock contention stays a no-op for prompt rendering, with stale-lock recovery via `*_PROMPT_SEGMENT_LOCK_STALE_SECONDS`.
  - Lock and throttle files stay cache-adjacent (`<cache-stem>.refresh.lock` and `<cache-stem>.refresh.at`).
  - Cache format, stale suffix rendering, and exit codes stay unchanged.
- Anchor tests for the cached/refresh paths live under the per-crate `integration` test target:
  - `crates/gemini-cli/tests/integration/prompt_segment_cached.rs`
  - `crates/codex-cli/tests/integration/prompt_segment_cached.rs`
  - `crates/codex-cli/tests/integration/prompt_segment_refresh.rs`
- Coverage gap to track separately: a dedicated `crates/gemini-cli/tests/integration/prompt_segment_refresh.rs` mirroring the codex
  blocking-path test surface (lock/min-interval/stale-lock recovery) does not exist yet; backlog only — do not patch under this contract.

### Async rate-limits guardrails

- Shared async invariants:
  - `--async` still rejects `--one-line` and positional secret args with exit `64`.
  - `--async --cached -c` remains invalid; clear-cache failures remain exit `1`.
  - Async JSON keeps `schema_version`, `command="diag rate-limits"`, `mode="async"`, top-level `ok`, and a full `results` array.
  - Collection results stay deterministically sorted by `name`, never by completion order.
  - Command-level secret discovery failures remain top-level `error.code="secret-discovery-failed"`.
  - Per-secret failures in async JSON keep the full `results` array and return exit `1`.
  - `--jobs` remains non-fatal on zero/invalid input; later concurrency work must not introduce a new `invalid --jobs` usage error.
  - Missing-access-token fallback remains explicit: successful fallback emits `source="cache-fallback"` with `ok=true`; otherwise the
    result stays an error.
- JSON error-path invariants:
  - `codex-cli` command failures emit `error { code, message, details? }`; per-result failures use the same nested `error` envelope and
    may carry `details`.
  - `gemini-cli` command failures emit the same top-level `error { code, message, details? }`, but per-result failures currently
    serialize a nested `error { code, message }` object without per-result `details`.
  - Stable per-secret async JSON error codes are `missing-access-token`, `request-failed`, `invalid-usage-payload`, and
    `cache-read-failed`.
- Crate-specific differences to preserve during concurrency refactors:
  - `codex-cli` text async already uses bounded worker threads and honors `--jobs`, defaulting zero/invalid values to `5`; its watch mode
    is codex-only and still requires `--async`.
  - `codex-cli` async JSON is still sequential today; Sprint 4 should change only the execution strategy, not envelopes, ordering,
    fallback, or return codes.
  - `codex-cli` async JSON already honors `--cached`.
  - `gemini-cli` text async is sequential today because it delegates to `run_all_mode(...)`; `jobs` exists in `RateLimitsOptions` but is
    unused.
  - `gemini-cli` async JSON is sequential and keeps a `missing-access-token -> cache-fallback` special case.
  - `gemini-cli` text async honors `--cached`, but async JSON currently does not branch on `args.cached` and still fetches from the
    network. Treat that difference as part of the current observable contract until an intentional contract update says otherwise.

### Focused concurrency validation

Per-crate integration tests are consolidated under a single `--test integration` target; filter to the relevant submodule by name:

- `cargo test -p nils-gemini-cli --test integration -- prompt_segment_cached`
- `cargo test -p nils-gemini-cli --test integration -- rate_limits_async rate_limits_network`
- `cargo test -p nils-codex-cli --test integration -- prompt_segment_cached prompt_segment_refresh`
- `cargo test -p nils-codex-cli --test integration -- rate_limits_async`

## Validation anchors

- `cargo build -p nils-gemini-cli --bins && cargo test -p nils-codex-cli --test integration -- parity_oracle`
- `cargo build -p nils-codex-cli --bins && cargo test -p nils-gemini-cli --test integration -- parity_oracle`
- `cargo test -p nils-codex-cli --test integration -- runtime_auth_contract runtime_error_contract runtime_exec_contract runtime_paths_config_contract`
- `cargo test -p nils-gemini-cli --test integration -- runtime_auth_contract runtime_error_contract runtime_exec_contract runtime_paths_config_contract`

Each parity oracle compares its own CLI against the *other* package's binary,
which a package-scoped `cargo test` does not build. Without the paired build the
suite skips and reports green with zero parity assertions, so the build is part
of the anchor rather than an optimisation. See `bin::sibling_or_skip` in
`crates/nils-test-support`.
