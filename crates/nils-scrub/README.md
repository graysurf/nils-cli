# nils-scrub

Shared secret-scrub library for the nils-cli workspace. Extracted from
`plan-archive` so multiple CLIs (`plan-archive refresh`, `evidence migrate`)
reuse one stable v1 pattern set and one scrub-log format.

## Surface

- `scrub_text(input) -> ScrubResult` — redact secrets in `input`; returns the
  redacted text plus per-match metadata (`Match { pattern_id, offset, length,
  redaction_length }`).
- `ScrubResult::triggered_patterns()`, `pattern_ids()`, `PATTERN_SET` (`"v1"`),
  `REDACTION_TOKEN` (`"[REDACTED]"`).
- `format_log(label, matches)` / `write_log_if_any(label, path, matches)` —
  emit a stable, diffable `<label> scrub log`. The caller passes its own label
  (`plan-archive`, `evidence`, ...) so each consumer's logs self-identify.

## v1 pattern set

`github-token`, `gitlab-token`, `bitbucket-app-password`, `aws-access-key-id`,
`generic-secret-kv` (value-only), `pem-private-key`. The set is intentionally
small and stable so scrub logs stay diffable across writes.

## Docs

See [`docs/README.md`](docs/README.md) for the crate docs index.
