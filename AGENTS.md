# AGENTS.md

## Scope

- This file contains `nils-cli`-specific agent instructions only.
- Global agent behavior, response style, dispatch flow, clarification policy,
  commit policy, and shared tool rules are inherited from the fallback
  `AGENTS.md`.
- Do not duplicate global guidance here; add only local overrides or local
  document pointers.

## Local Source Of Truth

- Development contract: `DEVELOPMENT.md`
- Runtime dependency and degradation reference: `BINARY_DEPENDENCIES.md`
- CLI completion policy: `docs/runbooks/cli-completion-development-standard.md`
- New CLI crate standard: `docs/runbooks/new-cli-crate-development-standard.md`
- Crate docs placement policy: `docs/specs/crate-docs-placement-policy.md`

## Required Local Check Entrypoints

- Exact check contents and tool prerequisites are defined in `DEVELOPMENT.md`.
- Default local development and pre-PR check:
  `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
- Docs-only changes:
  `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
- Full CI parity checks are not the default local loop. Run them locally only
  when debugging CI, preparing release-quality verification, or explicitly
  asked:
  `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh`
- Full coverage is owned by CI for normal PRs. Run it locally only for
  coverage maintenance, release-quality verification, or CI debugging:
  `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage`
- GitHub required checks for `main` must include `test`, `test_macos`, and
  `coverage`; those checks are the full-suite merge gate.
- When completion assets change, also run:
  - `zsh -n completions/zsh/_<cli>`
  - `bash -n completions/bash/<cli>`

## Repo Conventions

- In Rust tests, prefer `pretty_assertions::{assert_eq, assert_ne}` for clearer
  diffs.
- Every user-facing CLI must expose root `-V, --version`.
- For clap-based CLIs, set `#[command(version)]` on the root `Parser`.
- `--help` output should show `-V, --version`.

## Local Helpers

- Recommended tooling bootstrap: `scripts/setup-rust-tooling.sh`
- Local release install helper:
  `./.agents/skills/nils-cli-install-local-release-binaries/scripts/nils-cli-install-local-release-binaries.sh`
