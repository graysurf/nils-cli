# nils-cli

[![CI](https://github.com/sympoies/nils-cli/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/sympoies/nils-cli/actions/workflows/ci.yml)
[![Coverage](https://raw.githubusercontent.com/sympoies/nils-cli/coverage-badge/badges/coverage.svg)](https://github.com/sympoies/nils-cli/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/sympoies/nils-cli?sort=semver)](https://github.com/sympoies/nils-cli/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

A Rust workspace of focused CLI binaries for API testing, Git operations, agent workflow evidence, provider automation, planning, and
desktop/media utilities. Shared crates keep JSON contracts, terminal UX, and cross-CLI behavior consistent.

## CLI surface map

Start here when choosing a binary. The source of truth for the current installable binary list is:

```bash
bash scripts/workspace-bins.sh
```

Completion obligations for those binaries are tracked in
[docs/specs/completion-coverage-matrix-v1.md](docs/specs/completion-coverage-matrix-v1.md).

| Area | Binaries | Use when |
| ---- | -------- | -------- |
| API testing | `api-rest`, `api-gql`, `api-grpc`, `api-websocket`, `api-test` | Run protocol-specific API checks or orchestrate a mixed API test suite. |
| Git tooling | `git-scope`, `git-cli`, `git-summary`, `git-lock` | Inspect changes, run Git helper flows, summarize commits, or manage repo-local commit locks. |
| Forge automation | `forge-cli` | Drive PR/MR + Issue lifecycle and repository label catalog maintenance on GitHub (via `gh`) or GitLab (via `glab`) through a single provider-neutral surface; covers create / view / edit / comment / ready / merge / close, label list / audit / ensure, CI wait-checks, and the `pr deliver` macro. |
| Agent policy and evidence | `agent-runtime`, `agent-docs`, `agent-out`, `agent-scope-lock`, `agent-run`, `test-first-evidence`, `web-evidence`, `browser-session`, `canary-check`, `docs-impact`, `heuristic-inbox`, `model-cross-check`, `repo-retro`, `review-evidence`, `review-specialists`, `skill-usage` | Render/install/audit runtime-kit surfaces, resolve agent policy docs, run project commands through explicit env handling, allocate artifact paths, enforce edit scope, inspect repo retrospectives, merge specialist review evidence, or persist deterministic workflow evidence. |
| Planning and delivery | `plan-tooling`, `plan-issue`, `plan-issue-local`, `semantic-commit` | Validate/split implementation plans, orchestrate issue delivery, rehearse local plan flows, or run validated commit workflows. |
| Provider lanes | `codex-cli`, `gemini-cli` | Run provider-specific diagnostics, auth checks, and workflow adapters. |
| Markdown rendering | `md-render` | Render `.md.tera` templates from JSON view data through the shared `nils-markdown` engine. |
| Desktop, media, and local utilities | `macos-agent`, `screen-record`, `image-processing`, `fzf-cli`, `memo-cli` | Automate local desktop tasks, capture media, convert images, use interactive shell helpers, or record/search local memos. |
| Development-only/internal | `cli-template` | Validate packaging and new-crate patterns; excluded from user-facing completion obligations. |

## Workspace layout

Each crate is either a standalone CLI binary, a multi-binary crate, or a shared library used across the workspace.

### Shared foundations

- [crates/nils-common](crates/nils-common): Shared cross-CLI utilities (including markdown payload validation and markdown-table
  canonicalization helpers).
- [crates/nils-markdown](crates/nils-markdown): Tera-backed Markdown template engine, shared helpers, golden-test harness, and
  opt-in `md-render` binary for rendering `.md.tera` templates from JSON view data.
- [crates/nils-term](crates/nils-term): Terminal UX helpers (TTY detection + progress rendering on stderr).
- [crates/nils-test-support](crates/nils-test-support): Test-only helpers for deterministic workspace integration tests.
- [crates/cli-template](crates/cli-template): Minimal example CLI for validating packaging and new-crate patterns.

### API testing stack

- [crates/api-testing-core](crates/api-testing-core): Shared library for the API testing CLIs (config/auth, history, reporting).
- [crates/api-rest](crates/api-rest): REST request runner from JSON request specs, with history + Markdown reports.
- [crates/api-gql](crates/api-gql): GraphQL operation runner for `.graphql` files (variables, history, reports, schema).
- [crates/api-grpc](crates/api-grpc): gRPC request runner from JSON specs, with history + Markdown reports.
- [crates/api-websocket](crates/api-websocket): Deterministic WebSocket request runner with history + Markdown reports.
- [crates/api-test](crates/api-test): Suite runner that orchestrates REST/GraphQL/gRPC/WebSocket cases and outputs JSON (and optional
  JUnit).

### Git tooling

- [crates/git-scope](crates/git-scope): Git change inspector (tracked/staged/unstaged/untracked/commit) with tree + optional file printing.
- [crates/git-cli](crates/git-cli): Git tools dispatcher (utils/reset/commit/branch/ci/open).
- [crates/git-summary](crates/git-summary): Per-author contribution summaries over a date range (adds/dels/net/commits).
- [crates/git-lock](crates/git-lock): Label-based commit locks per repo (lock/list/diff/unlock/tag).
- [crates/forge-cli](crates/forge-cli): Provider-neutral forge CLI wrapping `gh` / `glab` for
  PR/MR + Issue lifecycle, repository label catalog maintenance, CI
  wait-checks, and the `pr deliver` macro
  (open draft → CI green → ready → merge).

### Desktop, media, and local utility CLIs

- [crates/macos-agent](crates/macos-agent): macOS desktop automation primitives for app/window discovery, input actions, screenshot, and
  wait helpers.
- [crates/fzf-cli](crates/fzf-cli): Interactive `fzf` toolbox for files, Git, processes, ports, and shell history.
- [crates/memo-cli](crates/memo-cli): Capture-first memo workflow CLI with agent enrichment loop (`add`, `list`, `search`, `report`,
  `fetch`, `apply`).
- [crates/image-processing](crates/image-processing): Image conversion CLI for `svg/png/webp/jpg` plus SVG validation with JSON/report outputs.
- [crates/screen-record](crates/screen-record): macOS ScreenCaptureKit + Linux (X11) recorder for a single window or display with optional
  audio.

### Agent policy and evidence tooling

- [crates/agent-runtime-cli](crates/agent-runtime-cli): Runtime-kit tooling binary (`agent-runtime`) for render, install, doctor,
  audit-drift, runtime state maintenance, skill listing, and PR/MR body rendering.
- [crates/agent-docs](crates/agent-docs): Deterministic policy-document resolver for Codex/agent workflows (`resolve`, `contexts`, `add`,
  `baseline`).
- [crates/agent-out](crates/agent-out): Canonical `$AGENT_HOME/out/` path generator and layout auditor for agent workflow artifacts.
- [crates/agent-scope-lock](crates/agent-scope-lock): Deterministic edit-scope lock CLI for agent workflows (`create`, `read`,
  `validate`, `clear`).
- [crates/web-evidence](crates/web-evidence): Redacted static HTTP evidence capture for agent workflows (`capture`, `completion`).
- [crates/agent-workflow-primitives](crates/agent-workflow-primitives): Multi-binary local-first agent workflow primitives
  (`agent-run`, `browser-session`, `canary-check`, `docs-impact`, `heuristic-inbox`, `model-cross-check`, `repo-retro`,
  `review-evidence`, `review-specialists`, `skill-usage`, `test-first-evidence`).

### Planning, delivery, and provider lanes

- [crates/codex-cli](crates/codex-cli): Provider-specific CLI for OpenAI/Codex workflows (auth, diagnostics, execution flows, Starship),
  with adapters over `nils-common::provider_runtime`.
- [crates/gemini-cli](crates/gemini-cli): Provider-specific CLI lane for Gemini workflows, with adapters over
  `nils-common::provider_runtime`.
- [crates/semantic-commit](crates/semantic-commit): Helper CLI for staged context, Semantic Commit validation, commit amend, and cleanup commit workflows.
- [crates/plan-tooling](crates/plan-tooling): Plan Format v1 tooling CLI (`to-json`, `validate`, `batches`, `artifact-audit`,
  `split-prs`, `scaffold`, `completion`), with bundle validation, advisory durable-artifact classification, deterministic/auto grouping
  primitives, and strict lane-metadata validation gates.
- [crates/plan-issue-cli](crates/plan-issue-cli): Plan issue orchestration binaries (`plan-issue`, `plan-issue-local`).
  The v3 issue-backed lifecycle is owned by `record open`, `record post`,
  `record repair-dashboard`, `record audit`, and `record close` (see
  [`docs/specs/issue-backed-plan-record-contract-v2.md`](crates/plan-issue-cli/docs/specs/issue-backed-plan-record-contract-v2.md)).
  `Task Decomposition` orchestration remains available through `start-plan`,
  `start-sprint`, etc., with runtime lane metadata materialized from plan
  content + split-prs grouping results.

## Shared helper policy (`nils-common`)

Contributors should treat `nils-common` as the shared helper boundary for cross-CLI primitives.

- Put reusable, domain-neutral helpers in [crates/nils-common](crates/nils-common).
- Keep crate-local adapters for user-facing copy, warning style, exit-code mapping, and CLI-specific UX policy.
- During migration, preserve parity by keeping output text/warnings/colors/exit behavior byte-for-byte stable.
- Characterize behavior with tests before moving helper logic, then re-run affected crate tests after migration.

Detailed scope, API examples, migration conventions, and non-goals are documented in
[crates/nils-common/README.md](crates/nils-common/README.md).

Finalized shared-crate boundary and extraction lane ownership are tracked in
[docs/specs/workspace-shared-crate-boundary-v1.md](docs/specs/workspace-shared-crate-boundary-v1.md).

Workspace doc retention scope and delete/keep decisions are tracked in
[docs/specs/workspace-doc-retention-matrix-v1.md](docs/specs/workspace-doc-retention-matrix-v1.md).

## Shell wrappers and completions

Canonical completion architecture and contributor validation live in
[docs/runbooks/cli-completion-development-standard.md](docs/runbooks/cli-completion-development-standard.md). Use
[DEVELOPMENT.md](DEVELOPMENT.md) for required delivery checks.

Completion obligation coverage is tracked in
[docs/specs/completion-coverage-matrix-v1.md](docs/specs/completion-coverage-matrix-v1.md).

Assets:

- [completions/zsh/](completions/zsh/): zsh completions (plus `aliases.zsh`)
- [completions/bash/](completions/bash/): bash completions (plus `aliases.bash`)
- [wrappers/](wrappers/): dev-only wrapper scripts

Local shell setup:

1. Zsh: add [completions/zsh/](completions/zsh/) to your `fpath`, then run `compinit` in your shell init.
2. Zsh (optional): `source completions/zsh/aliases.zsh` (see [completions/zsh/aliases.zsh](completions/zsh/aliases.zsh))
3. Bash: copy `completions/bash/<command>` into your bash-completion directory, or source them from your shell init.
4. Bash (optional): `source completions/bash/aliases.bash` (see [completions/bash/aliases.bash](completions/bash/aliases.bash))
5. Dev-only: add [wrappers/](wrappers/) to your PATH (or symlink wrapper scripts into a bin directory).

## Contributor checks

Use [DEVELOPMENT.md](DEVELOPMENT.md) as the canonical checklist.

- Default local check: `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
- Docs-only fast path: `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
- Full CI parity check, when needed locally:
  `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh`
- Full coverage is enforced by CI for PRs; run
  `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage`
  only for coverage/release/CI-debugging work.
- Regenerate third-party license/notice artifacts after dependency or metadata changes:
  - `bash scripts/generate-third-party-artifacts.sh --write`
- Verify third-party artifacts are current (fails on drift):
  - `bash scripts/generate-third-party-artifacts.sh --check`

## Local install (release)

- Build + install all workspace binaries into `~/.local/nils-cli/`:
  - `./scripts/install-local-release-binaries.sh`
- Install only a specific binary:
  - `./scripts/install-local-release-binaries.sh --bin git-scope`
- Add the install dir to `PATH` (example):
  - `export PATH="$HOME/.local/nils-cli:$PATH"`

## GitHub Releases (prebuilt binaries)

This repo can publish prebuilt tarballs via GitHub Releases for both:

- x86_64 (amd64)
- aarch64 (arm64)

To trigger a release build, push a tag like `v0.27.0`:

- `git tag -a v0.27.0 -m "v0.27.0"`
- `git push origin v0.27.0`

Then download the matching `nils-cli-<tag>-<target>.tar.gz` asset, extract it, and add `<extract_dir>/bin` to your `PATH`.

Release packaging contract: shipped artifacts must include `completions/zsh/`, `completions/bash/`, `completions/zsh/aliases.zsh`,
`completions/bash/aliases.bash`, `THIRD_PARTY_LICENSES.md`, and `THIRD_PARTY_NOTICES.md`. After extracting release assets, follow the
same setup flow from
["Shell wrappers and completions"](#shell-wrappers-and-completions).

## crates.io publishing (shared crates)

Use `scripts/publish-crates.sh` for crate publishing flow.

- Default list + order: `release/crates-io-publish-order.txt`
- Dry-run (recommended first):
  - `scripts/publish-crates.sh --dry-run`
- Real publish:
  - `scripts/publish-crates.sh --publish`
- Override target crates:
  - `scripts/publish-crates.sh --crates "nils-term nils-common" --dry-run`

In `--dry-run` mode, the script runs `cargo publish --dry-run` for every selected crate. In `--publish` mode, the script runs
`dry-run -> publish` sequentially per crate (in your specified order). By default, `--publish` skips crates that are already published at
the same version on crates.io.

To query crates.io publish status (single/multi/all crates), use:

- `scripts/crates-io-status.sh --all --format text`
- `scripts/crates-io-status.sh --crates "nils-common nils-codex-cli" --version v0.3.1 --format json`
- `scripts/crates-io-status.sh --crate nils-codex-cli --format both --json-out "$AGENT_HOME/out/codex-status.json"`

`--version` checks that exact version; without `--version` the script checks each crate's current workspace version. Use `--fail-on-missing`
for CI gates.

GitHub Actions manual flow is also available at `.github/workflows/publish-crates.yml`:

- Trigger via `workflow_dispatch`.
- `mode=dry-run` runs checks only.
- `mode=publish` requires repository secret `CARGO_REGISTRY_TOKEN`.

## Adding a new CLI crate

Use the canonical onboarding runbook:

- [docs/runbooks/new-cli-crate-development-standard.md](docs/runbooks/new-cli-crate-development-standard.md)
