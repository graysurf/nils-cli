# crates.io Status Script Runbook

## Purpose

This runbook explains how to use `scripts/crates-io-status.sh` to check workspace crates publish status on crates.io.

The script supports:

- single crate, multiple crates, or all crates from `release/crates-io-publish-order.txt`
- explicit target version query (`--version`)
- workspace-version query (omit `--version`)
- CI-friendly JSON output and human-readable text output

## Prerequisites

- run inside this git repo
- `bash`, `python3`, `cargo` available on `PATH`
- network access to crates.io API

## Common Usage

```bash
# 1) single crate, workspace version, text output
scripts/crates-io-status.sh --crate nils-codex-cli --format text

# 2) multiple crates, explicit version, JSON output
scripts/crates-io-status.sh \
  --crates "nils-common nils-codex-cli" \
  --version v0.3.1 \
  --format json \
  --json-out "$AGENT_HOME/out/crates-status-v0.3.1.json"

# 3) all crates from publish-order file, both outputs
scripts/crates-io-status.sh \
  --all \
  --format both \
  --text-out "$AGENT_HOME/out/crates-status-all.md" \
  --json-out "$AGENT_HOME/out/crates-status-all.json"

# 4) CI gate: fail when any crate is missing/error
scripts/crates-io-status.sh \
  --all \
  --format json \
  --json-out "$AGENT_HOME/out/crates-status-ci.json" \
  --fail-on-missing
```

## Output Semantics

- `status=published`: version exists and not yanked
- `status=yanked`: version exists but yanked
- `status=missing`: queried version not found on crates.io
- `status=error`: API/network/transient failure after retries

`--fail-on-missing` returns non-zero if any result is not `published`/`yanked`.

## Real Successful Query Results

The following commands were executed successfully on **2026-02-11 (UTC)**.

### A. Explicit Version Query (Success)

Command:

```bash
scripts/crates-io-status.sh \
  --crate nils-codex-cli \
  --version v0.3.1 \
  --format both \
  --text-out "$AGENT_HOME/out/crates-io-status-nils-codex-cli-v0.3.1.md" \
  --json-out "$AGENT_HOME/out/crates-io-status-nils-codex-cli-v0.3.1.json"
```

Text output snapshot:

```md
# crates.io Status Report

- Checked at (UTC): `2026-02-11T10:59:48Z`
- Query mode: `explicit-version`
- Target version: `0.3.1`
- Total crates: `1`
- Published: `1`
- Yanked: `0`
- Missing: `0`
- Error: `0`

| Crate | Workspace | Checked | Status | Latest | Published at (UTC) | Note |
|---|---:|---:|---|---:|---|---|
| nils-codex-cli | 0.3.1 | 0.3.1 | published | 0.3.1 | 2026-02-11T10:28:48.326957Z | - |
```

JSON output snapshot:

```json
{
  "checked_at": "2026-02-11T10:59:48Z",
  "query": {
    "mode": "explicit-version",
    "target_version": "0.3.1"
  },
  "summary": {
    "total": 1,
    "published": 1,
    "yanked": 0,
    "missing": 0,
    "error": 0
  },
  "results": [
    {
      "crate": "nils-codex-cli",
      "publishable": true,
      "workspace_version": "0.3.1",
      "checked_version": "0.3.1",
      "status": "published",
      "published": true,
      "yanked": false,
      "published_at": "2026-02-11T10:28:48.326957Z",
      "latest_version": "0.3.1",
      "crate_updated_at": "2026-02-11T10:28:48.326957Z",
      "version_downloads": 0,
      "crate_exists": true,
      "error": null
    }
  ]
}
```

### B. Workspace Version Query (Success)

Command:

```bash
scripts/crates-io-status.sh \
  --crate nils-codex-cli \
  --format both \
  --text-out "$AGENT_HOME/out/crates-io-status-nils-codex-cli-workspace.md" \
  --json-out "$AGENT_HOME/out/crates-io-status-nils-codex-cli-workspace.json"
```

Text output snapshot:

```md
# crates.io Status Report

- Checked at (UTC): `2026-02-11T10:59:48Z`
- Query mode: `workspace-version`
- Total crates: `1`
- Published: `1`
- Yanked: `0`
- Missing: `0`
- Error: `0`

| Crate | Workspace | Checked | Status | Latest | Published at (UTC) | Note |
|---|---:|---:|---|---:|---|---|
| nils-codex-cli | 0.3.1 | 0.3.1 | published | 0.3.1 | 2026-02-11T10:28:48.326957Z | - |
```

JSON output snapshot:

```json
{
  "checked_at": "2026-02-11T10:59:48Z",
  "query": {
    "mode": "workspace-version",
    "target_version": null
  },
  "summary": {
    "total": 1,
    "published": 1,
    "yanked": 0,
    "missing": 0,
    "error": 0
  },
  "results": [
    {
      "crate": "nils-codex-cli",
      "publishable": true,
      "workspace_version": "0.3.1",
      "checked_version": "0.3.1",
      "status": "published",
      "published": true,
      "yanked": false,
      "published_at": "2026-02-11T10:28:48.326957Z",
      "latest_version": "0.3.1",
      "crate_updated_at": "2026-02-11T10:28:48.326957Z",
      "version_downloads": 0,
      "crate_exists": true,
      "error": null
    }
  ]
}
```

## Notes for project-dispatch-crates-io-publish Skill Integration

`./.agents/skills/project-dispatch-crates-io-publish/scripts/publish-crates-io.sh` already supports post-run snapshot by calling this script.

- Default: generate `${report_file%.md}.status.json` and `${report_file%.md}.status.md`
- Disable snapshot: `--skip-status-check`
- Override script/output paths: `--status-script`, `--status-json-file`, `--status-text-file`
