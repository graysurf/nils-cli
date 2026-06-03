# Plan: zsh-kit Setup Entrypoint

## Overview

Add a nils-cli `zsh-kit` binary that acts as the stable runtime entrypoint for
bootstrapping an operator-supplied Zsh repository after an environment starts.
The command should make shell setup repeatable in containers and local agent
workspaces without baking personal `~/.config/zsh` contents, private scripts,
or credentials into the public `agent-runtime-kit` image.

The architectural boundary is deliberate: nils-cli owns the generic
clone/update/inspect/bootstrap/dispatch flow and stable CLI contract; the Zsh
repository owns Zsh-specific behavior through a repo-owned setup hook;
`agent-runtime-kit` only carries public prerequisites such as `zsh` plus the
released nils-cli binary set.

## Read First

- Primary source:
  `docs/plans/2026-06-03-zsh-kit-setup-entrypoint/zsh-kit-setup-entrypoint-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Repo anchors:
  - `docs/runbooks/new-cli-crate-development-standard.md`
  - `docs/runbooks/cli-completion-development-standard.md`
  - `docs/specs/crate-docs-placement-policy.md`
  - `docs/specs/completion-coverage-matrix-v1.md`
  - `scripts/workspace-bins.sh`
  - `release/crates-io-publish-order.txt`
  - neighboring publishable CLI crates under `crates/`
  - `BINARY_DEPENDENCIES.md`
- External workflow anchors:
  - Zsh repository bootstrap hook, to be owned outside nils-cli.
  - `agent-runtime-kit` Dockerfile, nils-cli pin, and Docker documentation,
    updated only after the nils-cli binary is released.
- Key decisions carried into execution:
  - Create a nils-cli binary named `zsh-kit`.
  - Keep Zsh-specific behavior out of nils-cli and in the Zsh repository hook.
  - Keep personal/private shell contents out of the public runtime image.
  - Use runtime auth only; do not persist tokens by default.
  - Prefer dry-run first and require explicit `--apply` for mutation.
- Open questions carried into execution: none.

## Scope

- In scope:
  - Add publishable crate `nils-zsh-kit` and binary `zsh-kit`.
  - Implement `zsh-kit setup` with text and JSON output contracts.
  - Support clone/update of an operator-supplied repository, destination safety
    checks, ref selection, optional `ZDOTDIR` bootstrap, feature forwarding,
    tool-install policy, and repo-hook dispatch.
  - Add fixtures and tests for dry-run, apply, refusal, and JSON contracts.
  - Add generated zsh/bash completions and completion-matrix coverage.
  - Add release-order/package integration and crate README.
  - Add a compatible setup hook in the Zsh repository.
  - Release nils-cli, then update `agent-runtime-kit` to consume the released
    binary and add only the public Docker prerequisite `zsh`.
- Out of scope:
  - Baking private Zsh repositories, `~/.config/zsh`, or private scripts into
    public images.
  - Encoding Zsh plugin/alias/function semantics in nils-cli.
  - Credential storage or secret management beyond using standard runtime auth.
  - Mandatory package-manager installs inside containers.
  - Replacing the Zsh repository's own bootstrap structure.

## Assumptions

1. `zsh-kit` can be a normal publishable workspace CLI crate with a
   clap-first command model.
2. The Zsh repository can expose one stable hook path, for example
   `bootstrap/zsh-kit-setup.zsh` or `.zsh-kit/setup.zsh`.
3. Runtime auth for private repositories is supplied by the operator through
   environment variables, `gh` auth state, or SSH agent forwarding.
4. The default destination is `$HOME/.config/zsh`, but tests should use
   temporary paths and fixture repositories.
5. A nils-cli release is needed before `agent-runtime-kit` can consume the new
   binary from its pinned release artifact.

## Sprint 1: nils-cli CLI Contract And Implementation

**Goal**: Add the `zsh-kit` binary with safe dry-run/apply setup behavior,
stable JSON output, tests, completions, and release packaging.

**PR grouping intent**: `group`
**Execution Profile**: serial

### Task 1.1: Lock the command contract and fixtures first

- **Location**:
  - `crates/` (new `zsh-kit` crate)
  - `docs/specs/completion-coverage-matrix-v1.md`
- **Description**: Scaffold the new crate contract and fixture strategy before
  production behavior: command shape, exit codes, JSON envelope, hook discovery
  expectations, mutation model, redaction rules, and local fixture repositories
  for dry-run/apply/refusal tests.
- **Dependencies**:
  - none
- **Complexity**: 2
- **Acceptance criteria**:
  - The command contract is visible in crate README or crate-local docs.
  - Tests or fixtures express dry-run, apply, missing hook, dirty destination,
    path conflict, and credential-redaction cases.
  - JSON envelope fields and stable error codes are specified before broader
    implementation.
- **Validation**:
  - `cargo test -p nils-zsh-kit`
  - `cargo fmt -p nils-zsh-kit -- --check`

### Task 1.2: Implement setup orchestration

- **Location**:
  - `crates/` (new `zsh-kit` crate)
- **Description**: Implement `zsh-kit setup` with `--repo`, `--dest`,
  `--branch` or `--ref`, `--write-zshenv`, `--features`, `--install-tools`,
  `--dry-run`, `--apply`, `--force`, and `--format text|json`. The command
  clones when absent, safely updates when present, validates destination state,
  detects a repo-owned hook, optionally writes a Zsh bootstrap, and dispatches
  to the hook only in apply mode.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 4
- **Acceptance criteria**:
  - Dry-run performs no filesystem or git mutation and reports planned actions.
  - Apply works against a local fixture repository and records changed paths.
  - Dirty or mismatched destinations refuse without `--force`.
  - Missing setup hook and unsafe credential-bearing output refuse or redact
    deterministically.
  - JSON and text modes cover success and failure cases.
- **Validation**:
  - `cargo test -p nils-zsh-kit`

### Task 1.3: Workspace integration, completions, and release packaging

- **Location**:
  - root `Cargo.toml`
  - `release/crates-io-publish-order.txt`
  - `docs/specs/completion-coverage-matrix-v1.md`
  - `completions/zsh/_zsh-kit`
  - `completions/bash/zsh-kit`
  - `scripts/workspace-bins.sh` consumers
  - workspace README or release docs if they enumerate binaries
- **Description**: Wire the new binary into the workspace, completion assets,
  release order, binary inventory, and publish-readiness checks.
- **Dependencies**:
  - Task 1.2
- **Complexity**: 2
- **Acceptance criteria**:
  - `zsh-kit` appears in `scripts/workspace-bins.sh --release-default`.
  - Completion matrix marks `zsh-kit` as required with zsh and bash assets.
  - Completion assets are generated from clap and pass shell syntax checks.
  - Publish dry-run for `nils-zsh-kit` succeeds.
- **Validation**:
  - `bash scripts/workspace-bins.sh --release-default | rg '^zsh-kit$'`
  - `zsh -n completions/zsh/_zsh-kit`
  - `bash -n completions/bash/zsh-kit`
  - `scripts/publish-crates.sh --dry-run --crate nils-zsh-kit`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`

## Sprint 2: Zsh Repository Hook

**Goal**: Add the repo-owned setup hook that `zsh-kit setup` dispatches to,
keeping shell-specific behavior outside nils-cli.

**PR grouping intent**: `group`
**Execution Profile**: serial after Sprint 1 contract is stable

### Task 2.1: Add a stable setup hook to the Zsh repository

- **Location**:
  - Zsh repository hook path, expected to be
    `bootstrap/zsh-kit-setup.zsh` or `.zsh-kit/setup.zsh`
  - Zsh repository docs/tests
- **Description**: Implement the hook that receives forwarded features and
  tool-install policy, validates the repository bootstrap, optionally runs
  repository-owned tool setup, and exposes a container-safe smoke path.
- **Dependencies**:
  - Task 1.2
- **Complexity**: 3
- **Acceptance criteria**:
  - Hook behavior is documented in the Zsh repository.
  - Hook can run in dry-run/smoke mode without private mutations.
  - Hook honors `--install-tools skip|repo` behavior as forwarded by nils-cli.
  - Existing Zsh checks pass.
- **Validation**:
  - `./tools/check.zsh`
  - `./tools/check.zsh --smoke`

## Sprint 3: Release And agent-runtime-kit Consumption

**Goal**: Ship the new binary and consume it in the runtime image without
baking personal shell config into the image.

**PR grouping intent**: `group`
**Execution Profile**: serial after Sprint 1 and Sprint 2

### Task 3.1: Release nils-cli with zsh-kit

- **Location**:
  - nils-cli release workflow and generated release assets
- **Description**: Publish a nils-cli release that includes `zsh-kit` and its
  completion assets so downstream runtime images can consume it from the normal
  pinned release artifact.
- **Dependencies**:
  - Task 1.3
- **Complexity**: 2
- **Acceptance criteria**:
  - Release artifacts include the `zsh-kit` binary.
  - Release artifacts include zsh and bash completion assets.
  - Downstream pin information is available for `agent-runtime-kit`.
- **Validation**:
  - nils-cli release validation and provider checks

### Task 3.2: Update agent-runtime-kit Docker and docs

- **Location**:
  - agent-runtime-kit nils-cli pin
  - agent-runtime-kit Dockerfile
  - agent-runtime-kit Docker documentation and image smoke tests
- **Description**: Bump the nils-cli pin after release, add `zsh` as a public
  Docker prerequisite, document the runtime `zsh-kit setup` flow, and keep
  private Zsh repository contents out of the image.
- **Dependencies**:
  - Task 3.1
- **Complexity**: 2
- **Acceptance criteria**:
  - Image contains `zsh` and the released `zsh-kit` binary.
  - Image smoke proves `zsh --version` and `zsh-kit --version` work.
  - Docker docs show a runtime setup command using an operator-supplied repo
    URL and runtime auth.
  - Dockerfile does not copy `~/.config/zsh` or private scripts.
- **Validation**:
  - agent-runtime-kit Docker build and CLI smoke
  - `zsh-kit setup --dry-run` inside the image
  - `bash scripts/ci/all.sh`
  - `bash tests/hooks/run.sh`

## Done Criteria

- `zsh-kit setup` is a released nils-cli command with tested dry-run/apply
  behavior, fixture coverage, completions, JSON contract, and publish packaging.
- The Zsh repository owns all shell-specific setup behavior behind a stable hook
  consumed by nils-cli.
- `agent-runtime-kit` image includes only public prerequisites and released
  nils-cli artifacts, not private shell contents.
- An operator can start the image, provide runtime auth, run one `zsh-kit setup`
  command, and enter the configured Zsh environment.
- Local and provider validation pass for all touched repositories.

## Validation Plan

- `plan-tooling validate --file docs/plans/2026-06-03-zsh-kit-setup-entrypoint/zsh-kit-setup-entrypoint-plan.md --format text --explain`
- `bash scripts/ci/plan-bundle-validate.sh --strict --file docs/plans/2026-06-03-zsh-kit-setup-entrypoint/zsh-kit-setup-entrypoint-plan.md`
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
- Targeted nils-cli tests from Sprint 1.
- Zsh repository checks from Sprint 2.
- agent-runtime-kit Docker and project-dev checks from Sprint 3.

## Risks And Guardrails

- **Risk**: public tracker records expose private shell repository details.
  **Guardrail**: public examples use placeholders and operator-supplied repo
  wording; command diagnostics redact credential-bearing URLs.
- **Risk**: nils-cli drifts into a Zsh framework.
  **Guardrail**: nils-cli owns orchestration and dispatch only; shell behavior
  remains in the Zsh repository hook.
- **Risk**: setup mutates an existing shell environment unexpectedly.
  **Guardrail**: dry-run first, explicit `--apply`, safe destination checks,
  explicit `--write-zshenv`, and backup/refusal behavior for conflicts.
- **Risk**: runtime image setup requires privileged package installation.
  **Guardrail**: image includes minimal public prerequisites; tool installation
  defaults to `skip` and repo-owned installs are explicit.
- **Risk**: release consumers miss the new binary or completion assets.
  **Guardrail**: include workspace-bins, completion matrix, release order,
  publish dry-run, and downstream Docker smoke in acceptance criteria.

## Future Work

- Add provider-specific auth diagnostics for GitHub and GitLab if private repo
  setup failures repeat.
- Add an optional `doctor` subcommand after `setup` usage stabilizes.
- Consider a broader `shell-kit` abstraction only if non-Zsh shells need the
  same lifecycle later.
