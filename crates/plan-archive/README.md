# nils-plan-archive

`plan-archive` is the deterministic CLI surface for the plan-archive
workflow described in
[`agent-runtime-kit:docs/plans/plan-archive-system/`][master-design].
Skills in `agent-runtime-kit` (`meta:plan-archive-migrate` and
`meta:plan-archive-query`) wrap this binary; this crate does not talk
to provider APIs directly.

## Sprint 1 surface

Sprint 1 (Plan 1 of the plan-archive system) ships the three schema
validators that later subcommands depend on:

- `plan-archive validate-hosts --input <path>` — validate an archive
  `config/hosts.yaml`. Enforces the v1 schema (`personal` / `employer`
  classes, employer name on `class: employer`, recognised retention
  values, supported `version`).
- `plan-archive validate-local --input <path>` — validate the
  machine-local config at
  `$XDG_CONFIG_HOME/agent-plan-archive/config.yaml`. Tolerates a
  missing file by returning documented defaults and exit code 0.
- `plan-archive validate-metadata --input <path>` — validate an
  archived plan's `metadata.yaml`. Accepts pre-classification plans
  that omit `captured_classification` (emits the
  `metadata-captured-classification-missing` warning) and rejects
  plans that name no `refs.issue|pr|mr`.

`migrate`, `refresh`, and `query` are declared in the CLI surface but
respond with `subcommand-not-implemented` in Sprint 1; their bodies
land in Sprints 3–5.

## Sprint 2 surface

Sprint 2 (Plan 1) lands the secret-scrub library that
`plan-archive refresh` consumes before it writes a snapshot to
`_index/`. The library is exported from the crate root and has no
CLI subcommand of its own:

- `plan_archive::scrub_text(input)` — scan `input` against the v1
  pattern set, return the redacted text plus per-match metadata.
- `plan_archive::scrub::write_log_if_any(path, matches)` — write the
  stable `<ISO8601>.scrub.log` sibling when at least one redaction
  occurred. No file is written for clean payloads.
- `plan_archive::scrub::pattern_ids()` — read-only view of the
  configured pattern set, stable across patch versions of the same
  `PATTERN_SET` major.

v1 pattern set:

| Pattern id | Detects | Capture |
| --- | --- | --- |
| `github-token` | GitHub personal/OAuth/app tokens (`ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_`) | entire token |
| `gitlab-token` | GitLab personal access tokens (`glpat-`) | entire token |
| `bitbucket-app-password` | Bitbucket app passwords (`ATBB…`) | entire token |
| `aws-access-key-id` | AWS access key ids (`AKIA`, `ASIA`, …) | entire id |
| `generic-secret-kv` | `secret/token/password/api_key`-style key-value pairs | value only |
| `pem-private-key` | `-----BEGIN … PRIVATE KEY-----` blocks | entire block |

Replacement token is `[REDACTED]`. Overlapping matches keep the
earliest, widest span; the same secret is never reported twice. The
scrub log itself never contains the secret value — only pattern id,
byte offset, span length, and replacement length.

## Output contracts

Both human-readable text (default) and a versioned JSON envelope
(`--format json`) are supported, following
`docs/specs/cli-output-contract-v1.md` and the workspace
`nils_common::cli_contract` primitives. Successful runs use the
`cli.plan-archive.<subcommand>.v1` schema version. Failures emit a
`{code, message, hint?}` error envelope on `stderr` with exit code
`65` (`EX_DATAERR`).

Stable error codes (Sprint 1):

| Subcommand | Code | When |
| --- | --- | --- |
| `validate-hosts` | `hosts-parse-error` | YAML parse failure |
| `validate-hosts` | `hosts-unsupported-version` | `version` is not `1` |
| `validate-hosts` | `hosts-missing-hosts` | top-level `hosts` missing or empty |
| `validate-hosts` | `hosts-unknown-class` | `class` is not `personal`/`employer` |
| `validate-hosts` | `hosts-employer-missing-name` | `class: employer` with no `employer` |
| `validate-hosts` | `hosts-unknown-retention` | unrecognised `retention` |
| `validate-local` | `local-parse-error` | YAML parse failure |
| `validate-local` | `local-unsupported-version` | `version` is not `1` |
| `validate-local` | `local-invalid-batch-size` | `refresh_batch_size <= 0` |
| `validate-local` | `local-io-error` | filesystem read failure |
| `validate-metadata` | `metadata-parse-error` | YAML parse failure |
| `validate-metadata` | `metadata-unsupported-version` | `version` is not `1` |
| `validate-metadata` | `metadata-missing-required-field` | required source field missing |
| `validate-metadata` | `metadata-no-refs` | `refs` carries no issue/pr/mr |
| `validate-metadata` | `metadata-unknown-class` | classification class not recognised |
| `validate-metadata` | `metadata-employer-missing-name` | `employer` class with no employer |

Stable warning codes (Sprint 1):

| Subcommand | Code | When |
| --- | --- | --- |
| `validate-local` | `local-defaults-used` | file missing or empty; defaults filled in |
| `validate-metadata` | `metadata-captured-classification-missing` | pre-classification plan without captured classification |

## Boundary

- Provider API access is delegated to `forge-cli`. `plan-archive` never
  duplicates auth or host configuration.
- Commit creation in the `migrate` and `refresh` subcommands (Sprints
  3–4) goes through the released `semantic-commit` binary, not raw
  `git commit`.
- The archive repository is treated as opaque storage; this CLI does
  not enforce repository-level governance beyond what the validators
  check.

## Tests

Run the validators:

```bash
cargo test -p nils-plan-archive
```

Validator behaviour is covered by:

- Unit tests inside `src/validate/{hosts,local,metadata}.rs`.
- Integration tests in `tests/validators.rs` driving the shipped
  fixture set under `tests/fixtures/{hosts,local,metadata}/`.

Scrub behaviour is covered by:

- Unit tests inside `src/scrub/{mod,log}.rs`.
- Integration tests in `tests/scrub.rs` driving
  `tests/fixtures/scrub/{all-patterns,clean}.txt`.

## Related

- Master design: `agent-runtime-kit:docs/plans/plan-archive-system/plan-archive-system-discussion-source.md`
- Plan 1: `agent-runtime-kit:docs/plans/plan-archive-nils-cli/plan-archive-nils-cli-plan.md`
- Plan 3 (skill bodies): `agent-runtime-kit:docs/plans/plan-archive-runtime-kit/plan-archive-runtime-kit-plan.md`
- Tracker: <https://github.com/sympoies/nils-cli/issues/571>

[master-design]: https://github.com/graysurf/agent-runtime-kit/blob/main/docs/plans/plan-archive-system/plan-archive-system-discussion-source.md
