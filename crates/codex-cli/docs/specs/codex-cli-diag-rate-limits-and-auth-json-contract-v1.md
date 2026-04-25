# codex-cli Diag/Auth JSON Contract v1

## Purpose

This document extends `docs/specs/cli-service-json-contract-guideline-v1.md` for service-consumed
JSON output from:

- `codex-cli diag rate-limits` (single/all/async)
- `codex-cli auth login|use|save|remove|refresh|auto-refresh|current|sync`

Human-readable output remains the default UX. JSON mode must be explicit (`--format json` or
`--json` where supported for compatibility).

## Schema Versions and Command Paths

| Surface | Canonical `command` | `schema_version` | Success payload key |
| --- | --- | --- | --- |
| diag rate-limits (single) | `diag rate-limits` | `codex-cli.diag.rate-limits.v1` | `result` |
| diag rate-limits (all/async) | `diag rate-limits` | `codex-cli.diag.rate-limits.v1` | `results` |
| auth login | `auth login` | `codex-cli.auth.v1` | `result` |
| auth use | `auth use` | `codex-cli.auth.v1` | `result` |
| auth save | `auth save` | `codex-cli.auth.v1` | `result` |
| auth remove | `auth remove` | `codex-cli.auth.v1` | `result` |
| auth refresh | `auth refresh` | `codex-cli.auth.v1` | `result` |
| auth auto-refresh | `auth auto-refresh` | `codex-cli.auth.v1` | `result` |
| auth current | `auth current` | `codex-cli.auth.v1` | `result` |
| auth sync | `auth sync` | `codex-cli.auth.v1` | `result` |

Auth surfaces use one shared schema contract: `codex-cli.auth.v1`.

## Required Envelope Rules

Top-level required keys (stable):

- `schema_version`: string
- `command`: canonical command path string (table above)
- `ok`: boolean

Success envelope:

- `ok=true`
- exactly one of:
  - `result` for single-target/single-entity responses
  - `results` for collection responses

Failure envelope:

- `ok=false`
- `error` object with:
  - `code` (stable machine code)
  - `message` (human-readable summary)
  - optional `details` (structured diagnostics)
- `result`/`results` must not be present when `ok=false`.

Partial failure rule:

- For collection workflows (`diag --all`, `diag --async`, and auth workflows that include per-target
  outcomes), top-level `ok=true` is allowed with per-item failures in `results`/`result.targets`.
- Command-level failure that prevents a usable payload must return `ok=false` with top-level `error`.

Sensitive data rule:

- Never emit local secrets/tokens (`access_token`, `refresh_token`, raw auth headers, private keys)
  in either success or failure payloads.

## Stable vs Informational Fields

Stable (safe for strict parsing):

- Top-level: `schema_version`, `command`, `ok`, `result|results|error`
- Error envelope: `error.code`, `error.message`, optional `error.details`
- Diag:
  - `result.mode` (`single`) for single mode
  - top-level `mode` (`all` or `async`) for collection mode
  - `result.target_file`, `results[*].target_file`
  - `results[*].name`
  - `results[*].status` (`ok|error`)
  - `summary.non_weekly_label`, `summary.non_weekly_remaining`,
    `summary.weekly_remaining`, `summary.weekly_reset_at_epoch`,
    `summary.non_weekly_reset_at_epoch`
- Auth:
  - `auth login`: `method` (`chatgpt-browser|chatgpt-device-code|api-key`),
    `provider` (`chatgpt|openai-api`), `completed`
  - `auth use`: `target`, `matched_secret`, `applied`, `auth_file`
  - `auth save`: `auth_file`, `target_file`, `saved`, `overwritten`
    (`true` when an existing target file is replaced)
  - `auth remove`: `target_file`, `removed`
  - `auth refresh`: `target_file`, `refreshed`, `synced`, `refreshed_at`
  - `auth auto-refresh`: `refreshed`, `skipped`, `failed`, `min_age_days`, `targets[*]`
  - `auth current`: `auth_file`, `matched`, `matched_secret`, `match_mode`
  - `auth sync`: `auth_file`, `synced`, `skipped`, `failed`, `updated_files`

Informational (do not hard-depend for schema validation):

- `raw_usage` (upstream payload passthrough; shape may evolve)
- Optional additive metadata (`source`, timestamps, debugging hints)
- Human-display-oriented strings inside `error.details`

## Compatibility Rules (v1)

- Additive fields are allowed within `codex-cli.diag.rate-limits.v1` and `codex-cli.auth.v1`.
- Renaming/removing/changing semantics of stable fields is breaking and requires a new schema
  version.
- Informational fields may be added/adjusted, but must not break stable field interpretation.
- Keep prior schema behavior available until consumers migrate.

## Examples

### diag rate-limits (single, success: `result`)

```json
{
  "schema_version": "codex-cli.diag.rate-limits.v1",
  "command": "diag rate-limits",
  "ok": true,
  "result": {
    "mode": "single",
    "target_file": "alpha.json",
    "source": "network",
    "summary": {
      "non_weekly_label": "5h",
      "non_weekly_remaining": 94,
      "weekly_remaining": 88,
      "weekly_reset_at_epoch": 1700600000,
      "non_weekly_reset_at_epoch": 1700003600
    },
    "raw_usage": {
      "rate_limit": {}
    }
  }
}
```

### diag rate-limits (all/async, partial failure: `results`)

```json
{
  "schema_version": "codex-cli.diag.rate-limits.v1",
  "command": "diag rate-limits",
  "mode": "all",
  "ok": true,
  "results": [
    {
      "name": "alpha",
      "target_file": "alpha.json",
      "status": "ok",
      "source": "network",
      "summary": {
        "non_weekly_label": "5h",
        "non_weekly_remaining": 94,
        "weekly_remaining": 88,
        "weekly_reset_at_epoch": 1700600000,
        "non_weekly_reset_at_epoch": 1700003600
      },
      "raw_usage": {
        "rate_limit": {}
      }
    },
    {
      "name": "beta",
      "target_file": "beta.json",
      "status": "error",
      "error": {
        "code": "missing-access-token",
        "message": "missing access_token in beta.json",
        "details": {
          "target_file": "beta.json"
        }
      }
    }
  ]
}
```

### diag rate-limits (command-level failure)

```json
{
  "schema_version": "codex-cli.diag.rate-limits.v1",
  "command": "diag rate-limits",
  "ok": false,
  "error": {
    "code": "invalid-arguments",
    "message": "--one-line is not compatible with --json",
    "details": {
      "flag": "--one-line"
    }
  }
}
```

### auth use (success)

```json
{
  "schema_version": "codex-cli.auth.v1",
  "command": "auth use",
  "ok": true,
  "result": {
    "target": "alpha@example.com",
    "matched_secret": "alpha.json",
    "applied": true,
    "auth_file": "/home/user/.agents/auth.json"
  }
}
```

### auth login (success)

```json
{
  "schema_version": "codex-cli.auth.v1",
  "command": "auth login",
  "ok": true,
  "result": {
    "method": "chatgpt-device-code",
    "provider": "chatgpt",
    "completed": true
  }
}
```

### auth login method mapping (stable)

| CLI invocation | `result.method` | `result.provider` |
| --- | --- | --- |
| `auth login` | `chatgpt-browser` | `chatgpt` |
| `auth login --device-code` | `chatgpt-device-code` | `chatgpt` |
| `auth login --api-key` | `api-key` | `openai-api` |

### auth save (success)

```json
{
  "schema_version": "codex-cli.auth.v1",
  "command": "auth save",
  "ok": true,
  "result": {
    "auth_file": "/home/user/.agents/auth.json",
    "target_file": "/home/user/.agents/secrets/team-alpha.json",
    "saved": true,
    "overwritten": false
  }
}
```

`result.overwritten` is `true` when `auth save` replaces an existing target file.

### auth save (overwrite confirmation required, failure)

```json
{
  "schema_version": "codex-cli.auth.v1",
  "command": "auth save",
  "ok": false,
  "error": {
    "code": "overwrite-confirmation-required",
    "message": "codex-save: /home/user/.agents/secrets/team-alpha.json exists; rerun with --yes to overwrite",
    "details": {
      "target_file": "/home/user/.agents/secrets/team-alpha.json",
      "overwritten": false
    }
  }
}
```

### auth remove (success)

```json
{
  "schema_version": "codex-cli.auth.v1",
  "command": "auth remove",
  "ok": true,
  "result": {
    "target_file": "/home/user/.agents/secrets/team-alpha.json",
    "removed": true
  }
}
```

### auth remove (confirmation required, failure)

```json
{
  "schema_version": "codex-cli.auth.v1",
  "command": "auth remove",
  "ok": false,
  "error": {
    "code": "remove-confirmation-required",
    "message": "codex-remove: /home/user/.agents/secrets/team-alpha.json exists; rerun with --yes to remove",
    "details": {
      "target_file": "/home/user/.agents/secrets/team-alpha.json",
      "removed": false
    }
  }
}
```

### auth refresh (success)

```json
{
  "schema_version": "codex-cli.auth.v1",
  "command": "auth refresh",
  "ok": true,
  "result": {
    "target_file": "alpha.json",
    "refreshed": true,
    "synced": true,
    "refreshed_at": "2026-02-11T03:20:11Z"
  }
}
```

### auth auto-refresh (success with per-target outcomes)

```json
{
  "schema_version": "codex-cli.auth.v1",
  "command": "auth auto-refresh",
  "ok": true,
  "result": {
    "refreshed": 2,
    "skipped": 1,
    "failed": 1,
    "min_age_days": 5,
    "targets": [
      {
        "target_file": "alpha.json",
        "status": "refreshed"
      },
      {
        "target_file": "beta.json",
        "status": "failed",
        "reason": "token-endpoint-failed"
      }
    ]
  }
}
```

### auth current (failure: secret-not-matched)

```json
{
  "schema_version": "codex-cli.auth.v1",
  "command": "auth current",
  "ok": false,
  "error": {
    "code": "secret-not-matched",
    "message": "/home/user/.agents/auth.json does not match any known secret",
    "details": {
      "auth_file": "/home/user/.agents/auth.json",
      "matched": false
    }
  }
}
```

### auth current (failure: secret-dir-not-found)

```json
{
  "schema_version": "codex-cli.auth.v1",
  "command": "auth current",
  "ok": false,
  "error": {
    "code": "secret-dir-not-found",
    "message": "/home/user/.config/codex_secrets not found",
    "details": {
      "secret_dir": "/home/user/.config/codex_secrets"
    }
  }
}
```

### auth sync (success)

```json
{
  "schema_version": "codex-cli.auth.v1",
  "command": "auth sync",
  "ok": true,
  "result": {
    "auth_file": "/home/user/.agents/auth.json",
    "synced": 1,
    "skipped": 3,
    "failed": 0,
    "updated_files": [
      "/home/user/.agents/secrets/alpha.json"
    ]
  }
}
```
