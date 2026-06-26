# codex-cli JSON Consumers Runbook

## Scope

This runbook covers service consumption of `codex-cli` JSON output for:

- `diag rate-limits` (single/all/async)
- `auth login|use|save|remove|refresh|auto-refresh|status|current|sync|remote pull`
- `prompt-segment status`

Shared baseline guidance:

- `docs/specs/cli-service-json-contract-guideline-v1.md`

Codex-specific contract source:

- `crates/codex-cli/docs/specs/codex-cli-diag-rate-limits-and-auth-json-contract-v1.md`

## Provider-specific schema routing

- `diag rate-limits` => `schema_version=codex-cli.diag.rate-limits.v1`
- `auth *` => `schema_version=codex-cli.auth.v1`
- `prompt-segment status` => `schema_version=codex-cli.prompt-segment.v1`

## Codex-specific integration notes

- `auth login` stable method values:
  - `chatgpt-browser`
  - `chatgpt-device-code`
  - `api-key`
- `auth save` overwrite confirmation failure code:
  - `overwrite-confirmation-required`
- `auth remove` confirmation failure code:
  - `remove-confirmation-required`
- `auth status` exits `0` for unauthenticated states and reports the machine-readable reason in `result.reason`.
- `auth refresh` may report `result.remote_sync=true` with `remote_ssh` and `remote_name` when default active-auth refresh delegates to a
  configured remote token authority; in that mode `synced=false` because local secret files are not overwritten with access-only auth.
- `prompt-segment status` exits `0` for non-rendering states and reports the machine-readable reason in `result.reason`.
- `auth current` secret-dir resolution failure codes:
  - `secret-dir-not-configured`
  - `secret-dir-not-found`
  - `secret-dir-read-failed`

## Consumer checklist

1. Follow the shared parsing/retry baseline from `docs/specs/cli-service-json-contract-guideline-v1.md`.
2. Route logic by both `command` and codex schema ids above.
3. Treat informational metadata (for example `raw_usage`) as optional.
4. Keep provider-specific behavior handling in codex caller code paths only.

Example commands:

```bash
codex-cli diag rate-limits --format json alpha.json
codex-cli diag rate-limits --all --format json
codex-cli auth login --format json
codex-cli auth login --format json --device-code
codex-cli auth login --format json --api-key
codex-cli auth save --format json --yes team-alpha.json
codex-cli auth remove --format json --yes team-alpha.json
codex-cli auth auto-refresh --format json
codex-cli auth status --format json
codex-cli auth current --format json
codex-cli auth remote pull --ssh g14 --name team --access-only --write-active --format json
codex-cli prompt-segment status --format json
```
