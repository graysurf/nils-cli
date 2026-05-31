---
name: project-deliver-new-cli-crate
description: Plan and implement new CLI crates by following repository standards for parity, JSON contracts, and publish readiness.
---

# Nils CLI Deliver New CLI Crate

## Contract

Prereqs:

- Run inside the `nils-cli` git work tree.
- `agent-docs`, `bash`, `git`, and `python3` available on `PATH`.
- The following policy docs exist:
  - `docs/runbooks/new-cli-crate-development-standard.md`
  - `docs/specs/cli-service-json-contract-guideline-v1.md`

Inputs:

- User request to create, port, or redesign a CLI crate.
- Optional:
  - source CLI/script reference (for parity work),
  - target crate/package/bin naming,
  - whether the crate is publishable or internal-only.

Outputs:

- A standards-aligned plan and/or implementation for the target CLI crate.
- Required policy checks captured in the response:
  - human-readable output contract,
  - service-consumable JSON contract,
  - publish-readiness requirements.
- Validation evidence (commands run and pass/fail status).

Exit codes:

- `0`: success
- `1`: failure
- `2`: usage error

Failure modes:

- Required policy documents are missing.
- `agent-docs preflight --intent project-dev --strict` fails.
- JSON contract or parity requirements are underspecified and cannot be inferred.
- Local changed-scope checks or CI-required checks fail.

## Scripts (only entrypoints)

- `<PROJECT_ROOT>/.agents/skills/project-deliver-new-cli-crate/scripts/create-cli-crate.sh`

## Workflow

1. Run preflight policy resolution:
   - `agent-docs audit --target all --strict`
   - `agent-docs preflight --intent project-dev --strict`
2. Read and apply canonical standards:
   - `docs/runbooks/new-cli-crate-development-standard.md`
   - `docs/specs/cli-service-json-contract-guideline-v1.md`
3. If planning is needed first, produce a rigorous plan under `docs/plans/*-plan.md` before code changes.
4. During implementation, enforce these gates:
   - human-readable output remains clear and stable,
   - JSON mode is explicit, versioned, and machine-consumable,
   - JSON error envelope is structured (`code`, `message`, optional `details`),
   - no sensitive data appears in JSON output.
5. Apply crate-level publishability rules:
   - publishable crates follow workspace metadata conventions and release order checks,
   - internal-only crates explicitly set/document `publish = false`.
6. Validate before delivery:
   - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
   - if publishable: `scripts/publish-crates.sh --dry-run --crate <crate-package-name>`
   - rely on the GitHub required checks for the full workspace and coverage
     gates before merge.
7. In the final response, include:
   - what was changed,
   - policy gates satisfied,
   - any unresolved risks.
