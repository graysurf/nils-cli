---
name: nils-cli-dispatch-crates-io-publish
description: Publish one, many, or all workspace crates to crates.io through GitHub workflow dispatch with run reporting.
---

# Nils CLI Dispatch Crates IO Publish

## Contract

Prereqs:

- Run inside the `nils-cli` git work tree.
- `bash`, `git`, `python3`, `cargo`, and `gh` available on `PATH`.
- GitHub auth configured for workflow dispatch (`gh auth status` passes).
- GitHub workflow exists:
  - `.github/workflows/publish-crates.yml`
- GitHub repository secret is configured for publish mode:
  - `CARGO_REGISTRY_TOKEN`
- Workspace publish order file exists:
  - `release/crates-io-publish-order.txt`
- crates.io status helper exists for post-run verification:
  - `scripts/crates-io-status.sh`

Inputs:

- Crate selection (one of):
  - `--crate <name>` (repeatable),
  - `--crates "a b,c"`,
  - `--all` (or omit selectors to use default list file).
- Optional behavior flags:
  - `--publish` (default) or `--dry-run-only`.
  - `--ref <git-ref>`: dispatch target ref (default `main`).
  - `--workflow <name>`: override workflow (default `publish-crates.yml`).
  - `--registry <name>`: pass optional registry input to workflow.
  - `--wait` / `--no-wait`: wait for run completion or return after dispatch.
  - `--discover-timeout-seconds <N>` and `--poll-seconds <N>` for run-id discovery.
  - `--report-file <path>`: custom report output path.
  - `--skip-status-check`: skip post-run crates.io snapshot generation.

Outputs:

- Validates selected crates against workspace metadata and dependency order.
- Dispatches GitHub workflow for publish/dry-run with selected crates.
- Optionally waits for workflow completion and captures run metadata.
- Produces a markdown report with:
  - mode, workflow/ref, run id/url/status/conclusion,
  - selected crate list and versions,
  - crates.io status snapshot metadata (json/text path + status).
- Default report path:
  - a project run directory from
    `agent-out project --topic crates-io-publish --mkdir`, when available;
  - otherwise
    `${CLAUDE_KIT_STATE_HOME:-${XDG_STATE_HOME:-$HOME/.local/state}/agent-runtime-kit}/out/crates-io-publish/`.
- Default status snapshot outputs:
  - `${report_file%.md}.status.json`
  - `${report_file%.md}.status.md`

Exit codes:

- `0`: workflow dispatch succeeded and (if waiting) run completed successfully
- `1`: dispatch failure, run failure, status snapshot failure, or validation error
- `2`: usage error

Failure modes:

- Crate not found in workspace metadata.
- Selected crate has `publish = false`.
- Publish order invalid for workspace path dependencies.
- GitHub workflow dispatch fails.
- Dispatched run cannot be discovered within timeout.
- Workflow run concludes with failure/cancelled/timed out.
- Status snapshot check fails after successful publish run.

## Scripts (only entrypoints)

- `<PROJECT_ROOT>/.agents/skills/nils-cli-dispatch-crates-io-publish/scripts/publish-crates-io.sh`

## Workflow

1. Resolve crate set (single / multi / all) and validate against workspace metadata.
2. Dispatch `.github/workflows/publish-crates.yml` via `gh workflow run`.
3. Discover the run id and optionally wait for completion via `gh run watch`.
4. For successful publish runs, run post-run crates.io snapshot:
   - `scripts/crates-io-status.sh --fail-on-missing`.
5. Write a structured report from template:
   - `.agents/skills/nils-cli-dispatch-crates-io-publish/references/PUBLISH_REPORT_TEMPLATE.md`
