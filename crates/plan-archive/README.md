# nils-plan-archive

`plan-archive` is the deterministic CLI surface for the plan-archive
workflow described in
[`agent-runtime-kit:docs/plans/2026-05-26-plan-archive-system/`][master-design].
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

## Sprint 3 surface

Sprint 3 (Plan 1) lands `plan-archive migrate`, which moves a closed
plan folder out of a working repo into the archive repo. Dry-run is
the default; `--apply` performs the writes and commits.

```bash
plan-archive migrate \
  --plan docs/plans/2026-05-27-my-plan \
  --issue https://github.com/org/repo/issues/123 \
  [--source-repo <path>] [--archive <path>] [--hosts <path>] \
  [--pr <url>] [--mr <url>] [--apply]
```

- `--source-repo` defaults to the current git repo root; `--archive`
  defaults to the machine-local config's `archive_clone_path`;
  `--hosts` defaults to `<archive>/config/hosts.yaml`.
- At least one of `--issue` / `--pr` / `--mr` is required so the
  archived `metadata.yaml` always carries a provider reference.
- Dry-run resolves the source identity from the `origin` remote
  (host, org/group path, repo, branch, HEAD SHA), classifies the host
  against `config/hosts.yaml`, enumerates the tracked plan files via
  `git ls-files`, and assembles the `metadata.yaml` payload — without
  touching any working tree.
- Apply copies the files under
  `plans/<host>/<org>/<repo>/<folder>/`, writes `metadata.yaml`,
  commits the archive via the released `semantic-commit` binary,
  pushes the archive, and only on push success deletes the source
  folder and commits that deletion in the working repo.

Stable error codes (Sprint 3):

| Code | When |
| --- | --- |
| `migrate-source-repo-not-found` | source repo path is not a git repo |
| `migrate-plan-folder-missing` | `--plan` folder absent in the source repo |
| `migrate-archive-clone-missing` | archive clone path not found |
| `migrate-unknown-host` | source host absent from `config/hosts.yaml` |
| `migrate-hosts-load-failed` / `migrate-hosts-parse-failed` | hosts config unreadable/invalid |
| `migrate-no-refs-supplied` | none of `--issue`/`--pr`/`--mr` given |
| `migrate-identity-failed` | could not read remote/branch/HEAD |
| `migrate-archive-target-exists` | archive target already present (apply) |
| `migrate-source-repo-dirty` | uncommitted changes in the plan folder (apply) |
| `migrate-subprocess-failed` | a git or semantic-commit subprocess failed |
| `migrate-io-error` | filesystem failure |

## Sprint 4 surface

Sprint 4 (Plan 1) lands `plan-archive refresh`, which fetches
provider payloads through `forge-cli`, scrubs them with the Sprint 2
library, and appends an `<ISO8601>.json` snapshot under `_index/`.
Refresh **writes and scrubs but never commits** — when a snapshot
triggers redactions it emits the `.scrub.log` sibling and flags
`requires_review: true`; the wrapping skill/user reviews the log and
commits separately.

```bash
plan-archive refresh --ref https://github.com/org/repo/issues/123
plan-archive refresh --repo github.com/org/repo [--since 2026-05-01]
  [--archive <path>] [--hosts <path>]
```

- `--ref` refreshes a single issue/PR/MR URL. `--repo` lists open
  refs through `forge-cli … list --state open` and refreshes each;
  `--since YYYY-MM-DD` narrows the batch. The two selectors are
  mutually exclusive.
- Provider API access is delegated to `forge-cli` (override the binary
  with `PLAN_ARCHIVE_FORGE_CLI`); the host is mapped to `github` for
  `github.com` and `gitlab` otherwise. `config/hosts.yaml` gates which
  hosts are allowed and drives the `_index/` path.
- Snapshots are append-only: each refresh writes a new
  `<ISO8601>.json` (basic-format UTC, no colons) and never overwrites
  or deletes a prior one. A batch isolates per-ref failures into the
  `failed` array instead of aborting.

Stable error codes (Sprint 4):

| Code | When |
| --- | --- |
| `refresh-no-selector` | neither `--ref` nor `--repo` supplied |
| `refresh-unparseable-ref` | `--ref` is not an issue/PR/MR URL |
| `refresh-unparseable-repo` | `--repo` is not `host/org/repo` |
| `refresh-unknown-host` | host absent from `config/hosts.yaml` |
| `refresh-invalid-since` | `--since` is not `YYYY-MM-DD` |
| `refresh-archive-clone-missing` | archive clone path not found |
| `refresh-hosts-load-failed` / `refresh-hosts-parse-failed` | hosts config unreadable/invalid |
| `refresh-forge-fetch-failed` | forge-cli fetch failed (per-ref in batch) |
| `refresh-io-error` | filesystem failure |

## Sprint 5 surface

Sprint 5 (Plan 1) lands `plan-archive query`, the read-only cache
surface. It never fetches, writes, or commits. Three mutually
exclusive modes:

```bash
plan-archive query --ref https://github.com/org/repo/issues/123
plan-archive query --host github.com [--org <o>] [--repo <r>] [--since 2026-05-01]
plan-archive query --plan plans/github.com/org/repo/2026-05-27-slug
plan-archive query --refs-from path/to/metadata.yaml
  [--archive <path>]
```

- single-ref (`--ref`): returns the latest snapshot for one
  issue/PR/MR plus its `fetched_at`. A ref with no snapshots returns
  a record with `latest_snapshot: null` (exit 0), not an error.
- aggregate (`--host`/`--org`/`--repo`/`--since`): walks `_index/`
  across every reachable host in one pass and returns each matching
  ref's latest snapshot. A no-match query returns an empty array,
  exit 0.
- link traversal (`--plan` / `--refs-from`): reads an archived plan's
  `metadata.yaml` (`--plan` resolves `<archive>/<plan>/metadata.yaml`;
  `--refs-from` takes the metadata path directly) and resolves each
  `refs.issue|pr|mr` to its latest snapshot.

`fetched_at` is surfaced by default on every record (decoded from the
basic-format snapshot filename into extended ISO8601). Latest is the
lexically-last snapshot filename in the ref folder.

Stable error codes (Sprint 5):

| Code | When |
| --- | --- |
| `query-no-selector` | no `--ref`/filter/`--plan`/`--refs-from` supplied |
| `query-unparseable-ref` | `--ref` is not an issue/PR/MR URL |
| `query-invalid-since` | `--since` is not `YYYY-MM-DD` |
| `query-archive-clone-missing` | archive clone path not found |
| `query-metadata-not-found` | `--plan`/`--refs-from` metadata absent |
| `query-metadata-read-failed` | metadata unreadable/invalid |
| `query-metadata-no-refs` | metadata carries no refs to traverse |
| `query-io-error` | filesystem failure |

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
- `query` is strictly read-only: it never fetches, writes, or commits.
  `refresh` writes and scrubs but holds the commit so any emitted
  scrub log can be reviewed first.

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

- Master design: `agent-runtime-kit:docs/plans/2026-05-26-plan-archive-system/plan-archive-system-discussion-source.md`
- Plan 1: `agent-plan-archive:plans/github.com/graysurf/agent-runtime-kit/2026-05-27-plan-archive-nils-cli/plan-archive-nils-cli-plan.md`
- Plan 3 (skill bodies): `agent-plan-archive:plans/github.com/graysurf/agent-runtime-kit/2026-05-27-plan-archive-runtime-kit/plan-archive-runtime-kit-plan.md`
- Tracker: <https://github.com/sympoies/nils-cli/issues/571>

[master-design]: https://github.com/graysurf/agent-runtime-kit/blob/main/docs/plans/2026-05-26-plan-archive-system/plan-archive-system-discussion-source.md
