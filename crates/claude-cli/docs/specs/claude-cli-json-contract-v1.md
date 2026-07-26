# claude-cli JSON Contract v1

## Purpose

This specification extends
`docs/specs/cli-service-json-contract-guideline-v1.md` for:

- `claude-cli usage --format json`
- `claude-cli prompt-segment status --format json`
- `claude-cli auth status --format json`
- `claude-cli agent doctor --format json`

Text remains the default. JSON is opt-in and is emitted to stdout as one
versioned envelope.

## Schemas

| Command | `schema_version` | Payload |
| --- | --- | --- |
| `usage` | `claude-cli.usage.v1` | `result` |
| `prompt-segment status` | `claude-cli.prompt-segment.v1` | `result` |
| `auth status` | `claude-cli.auth.v1` | `result` or `error` |
| `agent doctor` | `claude-cli.agent.doctor.v1` | `result` |

Every envelope contains `schema_version`, `command`, and `ok`. Additive fields
are compatible within v1. Renaming, removing, or changing the meaning of
stable fields requires a new schema version.

## Usage

Stable result fields are `provider`, `source`, `stale`, `windows`, and optional
`reason_code`. `provider` is `claude`; `source` is `oauth`, `cli`, `cache`, or
`none`.

Each window contains `key`, `label`, `window_minutes`, `used_percent`,
`remaining_percent`, and optional `resets_at` and `resets_at_epoch`.
Informational result fields are `cache_file`, `updated_at`, `plan`, and `note`.

Consumers must not authorize a state transition from stale windows. An empty
`windows` array is a valid unavailable result, not unlimited quota.

Stable `reason_code` values:

- `auth_required`
- `auth_expired`
- `billing_past_due`
- `subscription_inactive`
- `organization_disabled`
- `permission_denied`
- `rate_limited`
- `service_unavailable`
- `timeout`
- `unknown`

## Prompt-segment status

Stable result fields are `authenticated`, `cache_exists`, `cache_stale`,
`would_render`, and `reason`. `auth_source` and `cache_file` are informational.
`authenticated` reports prompt-segment OAuth availability, not general Claude
Code login state.

## Auth status

Stable result fields are `logged_in` and optional `auth_method`,
`api_provider`, and `subscription_type`.

The command calls `claude auth status --json` and constructs a new allowlisted
result. It drops email, organization identifiers and names, tokens, credential
paths, and unknown upstream fields.

Authenticated status exits `0`; unauthenticated status exits `1`. Either may
produce `ok: true` when the wrapper successfully inspected valid upstream JSON.

Command-level failures use `ok: false` with:

- `launch-failed`
- `upstream-timeout`
- `output-too-large`
- `invalid-upstream-output`
- `invalid-upstream-shape`
- `unexpected-upstream-status`
- `inconsistent-upstream-status`

The wrapper accepts only a top-level object with boolean `loggedIn`. Exit `0`
must pair with `loggedIn: true`; exit `1` must pair with `loggedIn: false`.
Malformed JSON, malformed shapes, and JSON/exit disagreement are data errors.
Other upstream exit values and the three-second child deadline are runtime
errors. A non-`0`/`1` exit is classified before parsing stdout, so malformed
diagnostic output cannot hide a runtime failure. Captured stdout and stderr
share one aggregate limit while the child is still running.

## Agent doctor

Stable result fields are `ready`, `commit_profile`,
`configured_commit_profile`, `upstream_doctor`, `upstream_doctor_status`,
`dependencies`, and `flags`. `commit_profile` covers the fixed safe commit
flags. `configured_commit_profile` additionally requires optional model and
effort flags when the effective wrapper configuration selects them; `ready`
uses this configured profile.

`dependencies` contains booleans for `claude`, `git`, `semantic_commit`, and
`semantic_commit_compatible`. Presence and compatibility remain separate so an
older or unrelated executable cannot make `ready` true.
`flags` maps each required upstream option to a boolean. Stable
`upstream_doctor_status` values are:

- `ready`
- `failed`
- `timeout`
- `output-too-large`
- `launch-failed`

The command never invokes a model. It runs `claude --help` and
`claude doctor` with bounded stdout/stderr capture and emits none of the
captured text. `ok: true` means diagnosis completed even when `ready` is
false. Ready exits `0`; unavailable exits `1`.

## Examples

```json
{"schema_version":"claude-cli.usage.v1","command":"usage","ok":true,"result":{"provider":"claude","source":"cache","stale":true,"windows":[{"key":"5h","label":"5h","window_minutes":300,"used_percent":25.0,"remaining_percent":75.0}]}}
```

```json
{"schema_version":"claude-cli.auth.v1","command":"auth status","ok":true,"result":{"logged_in":true,"auth_method":"claude.ai","api_provider":"firstParty","subscription_type":"team"}}
```

```json
{"schema_version":"claude-cli.agent.doctor.v1","command":"agent doctor","ok":true,"result":{"ready":true,"commit_profile":true,"configured_commit_profile":true,"upstream_doctor":true,"upstream_doctor_status":"ready","dependencies":{"claude":true,"git":true,"semantic_commit":true,"semantic_commit_compatible":true},"flags":{"--json-schema":true,"--safe-mode":true}}}
```

## Sensitive-data rules

- Never emit tokens, API keys, raw credential JSON, authorization headers,
  upstream error bodies, or terminal transcripts.
- Auth status additionally excludes personal and organization identity.
- Agent doctor excludes upstream diagnostic stdout/stderr, settings paths,
  environment values, and model output because it does not make a model call.
- Tests seed recognizable secret markers and assert that stdout and stderr omit
  them.
