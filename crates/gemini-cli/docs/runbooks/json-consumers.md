# gemini-cli JSON Consumers Runbook

## Scope

This runbook covers service consumption of `gemini-cli` JSON output for:

- `diag rate-limits` (single/all/async)
- `auth login|use|save|remove|refresh|auto-refresh|current|sync`

Shared baseline guidance:

- `docs/specs/cli-service-json-contract-guideline-v1.md`

Gemini-specific contract source:

- `crates/gemini-cli/docs/specs/gemini-cli-diag-rate-limits-and-auth-json-contract-v1.md`

## Provider-specific schema routing

- `diag rate-limits` => `schema_version=gemini-cli.diag.rate-limits.v1`
- `auth *` => `schema_version=gemini-cli.auth.v1`

## Gemini-specific integration notes

- `auth login` stable method values:
  - `gemini-browser`
  - `gemini-device-code`
  - `api-key`
- `auth login` stable provider values:
  - `gemini` (browser / device-code)
  - `gemini-api` (api-key)
- `auth save` overwrite confirmation failure code:
  - `overwrite-confirmation-required`
- `auth remove` confirmation failure code:
  - `remove-confirmation-required`
- `auth current` secret-dir resolution failure codes:
  - `secret-dir-not-configured`
  - `secret-dir-not-found`
  - `secret-dir-read-failed`
- `auth current` no-match failure code:
  - `secret-not-matched`

## Consumer checklist

1. Follow the shared parsing/retry baseline from `docs/specs/cli-service-json-contract-guideline-v1.md`.
2. Route logic by both `command` and gemini schema ids above.
3. Treat informational metadata (for example `raw_usage`) as optional.
4. Keep provider-specific behavior handling in gemini caller code paths only.

Example commands:

```bash
gemini-cli diag rate-limits --format json alpha.json
gemini-cli diag rate-limits --all --format json
gemini-cli auth login --format json
gemini-cli auth login --format json --device-code
gemini-cli auth login --format json --api-key
gemini-cli auth save --format json --yes team-alpha.json
gemini-cli auth remove --format json --yes team-alpha.json
gemini-cli auth auto-refresh --format json
gemini-cli auth current --format json
```
