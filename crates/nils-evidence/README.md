# nils-evidence

The `evidence` CLI — query and migrate a durable, scrubbed **skill-usage
evidence archive**. The sibling of `plan-archive` (plan history), for runtime
skill-usage records. Depends on the shared `nils-scrub` crate for redaction.

## Commands

- `evidence migrate [--repo --skill --since --until --promotion-only] [--apply]`
  — dry-run by default; rolls up + scrubs each `skill-usage.record.json` under
  the agent-out tree into a `skill-usage.rollup.v1`, dedups via the catalog
  `source_digest`, and (with `--apply`) writes, one-batch-commits, **and
  `git push`es** to the archive clone's configured upstream — `--apply`
  publishes to the remote, and fails the run if `git push` errors. Raw records
  are never written.
- `evidence discover` — read-only scan of the agent-out tree; classifies
  skill-runs eligible / blocked / unknown.
- `evidence query [--skill --outcome --repo --host --org --since --until]` —
  filtered list over the derived catalog.
- `evidence search <term>` — full-text substring match over intent + outcome
  summary.
- `evidence catalog [--write --grep --outcome --case-id --deep]` —
  generate / filter the derived `evidence.catalog.v1`.
- `evidence validate-hosts | validate-local | validate-record` — schema checks.
- `evidence completion <bash|zsh>`.

Every command supports `--format json` (a `cli.evidence.<cmd>.v1` envelope).

## Archive resolution

`--archive <path>` > `$AGENT_EVIDENCE_ARCHIVE_HOME` > machine-local config
`archive_clone_path` (`$XDG_CONFIG_HOME/agent-evidence-archive/config.yaml`) >
default `${XDG_DATA_HOME:-$HOME/.local/share}/agent-evidence-archive`.

## Cross-version

Queries declare a readable schema-version range and report (never silently
drop) rollups outside it. Each rollup carries the producing `nils_cli_version`
(from the `skill-usage.record.v1` `producer` block).

## Docs

See [`docs/README.md`](docs/README.md) for the crate docs index.
