---
name: project-verify-required-checks
description: Run the full nils-cli CI/parity checks from DEVELOPMENT.md.
---

# Nils CLI Verify Required Checks

## Contract

Prereqs:

- Run inside the `nils-cli` git work tree (the script resolves the repo root via `git`).
- Follow the tool prerequisites defined in `DEVELOPMENT.md`.

Inputs:

- Optional flag: `--docs-only` (documentation-only fast path).

Outputs:

- Runs the full CI/parity checks defined in `DEVELOPMENT.md`.
- In `--docs-only` mode, runs only the documentation checks defined there.
- Prints the failing command (if any) and exits non-zero on failure.

Exit codes:

- `0`: all checks passed
- `1`: a check failed
- `2`: usage error (invalid arguments) or missing prerequisites

Failure modes:

- Not in a git work tree (cannot resolve repo root).
- Missing a required tool on `PATH`.
- Any of the full-stack lint/tests fail.

## Scripts (only entrypoints)

- `.agents/skills/project-verify-required-checks/scripts/project-verify-required-checks.sh`

## Workflow

- For day-to-day implementation, prefer the changed-scope local gate:
  `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`.
- Run this full stack locally only when you need CI parity, release-quality
  verification, or CI debugging evidence.
- For docs-only changes (`README.md` / `docs/**` / `*.md` only), prefer:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
- If it fails, fix the reported issue and re-run until it exits `0`.

## Alternate entry points

Claude Code's `/pre-pr` slash command covers the default local intent via
`<repo>/.agents/scripts/pre-pr.sh`, which runs
`scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` by default. Use
`/pre-pr --full` or this skill's script directly only for local CI parity; use
`/pre-pr --with-coverage` when local coverage evidence is explicitly needed.

See claude-kit's `docs/dispatcher-commands.md` for the multi-CLI mirror
rule behind this pairing.
