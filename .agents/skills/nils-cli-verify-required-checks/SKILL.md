---
name: nils-cli-verify-required-checks
description: Run the required nils-cli checks from DEVELOPMENT.md.
---

# Nils CLI Verify Required Checks

## Contract

Prereqs:

- Run inside the `nils-cli` git work tree (the script resolves the repo root via `git`).
- Follow the tool prerequisites defined in `DEVELOPMENT.md`.

Inputs:

- Optional flag: `--docs-only` (documentation-only fast path).

Outputs:

- Runs the required checks defined in `DEVELOPMENT.md`.
- In `--docs-only` mode, runs only the documentation checks defined there.
- Prints the failing command (if any) and exits non-zero on failure.

Exit codes:

- `0`: all checks passed
- `1`: a check failed
- `2`: usage error (invalid arguments) or missing prerequisites

Failure modes:

- Not in a git work tree (cannot resolve repo root).
- Missing a required tool on `PATH`.
- Any of the required lint/tests fail.

## Scripts (only entrypoints)

- `.agents/skills/nils-cli-verify-required-checks/scripts/nils-cli-verify-required-checks.sh`

## Workflow

- Run before you claim a task is done.
- For docs-only changes (`README.md` / `docs/**` / `*.md` only), prefer:
  - `.agents/skills/nils-cli-verify-required-checks/scripts/nils-cli-verify-required-checks.sh --docs-only`
- If it fails, fix the reported issue and re-run until it exits `0`.

## Alternate entry points

Claude Code's `/pre-pr` slash command covers the same intent via
`<repo>/.agents/scripts/pre-pr.sh`, which runs a **superset** of these
checks: `scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage` adds
an llvm-cov coverage gate on top of the base audit stack. Invoke this
skill's script directly when you want the base stack without the coverage
gate, or when you are driving from a CLI that discovers through
`.agents/skills/` (codex / opencode).

See claude-kit's `docs/dispatcher-commands.md` for the multi-CLI mirror
rule behind this pairing.
