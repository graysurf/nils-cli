# zsh-kit Setup Entrypoint Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: Sprint 1 implementation and local validation complete, with Task 1.3
  publish dry-run blocked by unreleased internal workspace dependencies; follow-up
  Sprint 2 and Sprint 3 work remains open.
- Target scope: nils-cli `zsh-kit` setup entrypoint, the repo-owned Zsh setup
  hook it dispatches to, and the follow-up agent-runtime-kit Docker/pin/docs
  consumption after nils-cli release.
- Execution window: Sprint 1 (`nils-cli` CLI contract, implementation, tests,
  completions, release packaging, PR1) -> Sprint 2 (Zsh repository setup hook,
  PR2) -> Sprint 3 (nils-cli release and agent-runtime-kit consumption, PR3),
  serial.
- Current task: Task 1.3 - blocked on release-sequence publish dry-run.
- Next task: Deliver the Sprint 1 PR, then unblock Task 1.3 during the Sprint 3
  release sequence after internal 1.0.6 dependencies are published.
- Last updated: 2026-06-03
- Branch/commit/PR: tracker opened from committed bundle `a9e34e7`;
  implementation branch `feat/zsh-kit-setup-entrypoint`; Sprint 1 PR pending.
- Source document: docs/plans/2026-06-03-zsh-kit-setup-entrypoint/zsh-kit-setup-entrypoint-plan.md
- Direct source-doc execution waiver: not applicable
- Tracking issue: <https://github.com/sympoies/nils-cli/issues/762>
- Source snapshot: <https://github.com/sympoies/nils-cli/issues/762#issuecomment-4612460747>
- Plan snapshot: <https://github.com/sympoies/nils-cli/issues/762#issuecomment-4612460912>
- Initial state snapshot: <https://github.com/sympoies/nils-cli/issues/762#issuecomment-4612461130>

## Validation Plan

- Bundle creation: targeted plan-source bundle validation and docs-only/local
  nils-cli validation before opening the tracker.
- Sprint 1: targeted nils-cli tests for `zsh-kit`, completion syntax checks,
  workspace binary inventory, publish dry-run, and
  `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`.
- Sprint 2: Zsh repository syntax/unit/smoke validation through its repo-owned
  check entrypoints.
- Sprint 3: nils-cli release validation, then agent-runtime-kit Docker smoke and
  project-dev validation.

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | done | Lock the command contract and fixtures first | Implemented zsh-kit setup command contract, text/JSON output envelopes, refusal codes, hook discovery, mutation model, and fixture-backed tests. | Specify setup flags, text/JSON output, refusal codes, hook discovery, mutation model, and fixture repositories. |
| 1.2 | done | Implement setup orchestration | Implemented clone/update orchestration, destination validation, .zshenv write option, hook dispatch, dry-run mutation boundary, credential redaction, and safety refusals; cargo test -p nils-zsh-kit passed. | Clone/update destination, validate state, optionally write bootstrap, dispatch to repo hook, and keep dry-run mutation-free. |
| 1.3 | blocked | Workspace integration, completions, and release packaging | Added workspace member, completions, completion matrix, release order, README/docs entries, third-party artifacts, workspace binary inventory coverage, publish dry-run preparation, and local-fast validation.; scripts/publish-crates.sh --dry-run --crate nils-zsh-kit reaches packaging but fails because nils-build-info 1.0.6 is not yet published on crates.io; control dry-run for existing nils-agent-docs fails the same way, while cargo publish -p nils-build-info --dry-run --locked passes. | Completion assets, release order, docs, third-party artifacts, and binary inventory are complete; publish dry-run remains gated on the Sprint 3 release sequence publishing internal 1.0.6 dependencies first. |
| 2.1 | pending | Add a stable setup hook to the Zsh repository | - | Keep shell-specific setup behavior outside nils-cli and validate with the Zsh repo's checks. |
| 3.1 | pending | Release nils-cli with zsh-kit | - | Produce release artifacts containing `zsh-kit` and completion assets. |
| 3.2 | pending | Update agent-runtime-kit Docker and docs | - | Add only public `zsh` prerequisite, bump nils-cli pin, document runtime setup, and smoke the image. |

## Session Log

- 2026-06-03: Authored this bundle after the operator approved the hybrid
  design: nils-cli provides a stable `zsh-kit` entrypoint, the Zsh repository
  owns shell behavior, and `agent-runtime-kit` only adds public prerequisites
  such as `zsh` without baking personal shell config or private repositories
  into the image.

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `plan-tooling validate --file docs/plans/2026-06-03-zsh-kit-setup-entrypoint/zsh-kit-setup-entrypoint-plan.md --format text --explain` | pass | Plan-source bundle validated with zero errors. | local |
| `bash scripts/ci/plan-bundle-validate.sh --strict --file docs/plans/2026-06-03-zsh-kit-setup-entrypoint/zsh-kit-setup-entrypoint-plan.md` | pass | Repository plan-bundle validator passed for this new bundle. | local |
| `agent-run exec --cwd /Users/terry/Project/sympoies/nils-cli -- bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` | pass | Docs placement, hygiene, markdown lint, cli-output contract lint, and forge-cli fixture lint passed. | local |
| `agent-run exec --cwd /Users/terry/Project/sympoies/nils-cli -- bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` | pass | Local-fast selected docs-only mode for this three-file plan bundle and passed. | local |
| `plan-issue --repo sympoies/nils-cli --format json --dry-run record open --profile tracking --bundle ...` | pass | Dry-run preview rendered source, plan, and state lifecycle comments from commit `a9e34e7`. | local |
| `plan-issue --repo sympoies/nils-cli --format json record open --profile tracking --bundle ...` | pass | Opened tracker issue #762 and posted source, plan, and state lifecycle comments. | <https://github.com/sympoies/nils-cli/issues/762> |
| `plan-issue --format json tracking run init --provider-repo sympoies/nils-cli --issue 762 --bundle ...` | pass | Initialized typed run state `20260603T124900Z-issue-762` for branch `feat/zsh-kit-setup-entrypoint`. | local |
| `plan-issue --format json record audit --profile tracking --expect-visible` | pass | Read-back audit recognized source, plan, and state roles with visible lint passing. | local |
| `plan-issue --format json tracking status --provider-repo sympoies/nils-cli --issue 762 --run-state ... --expect-visible` | pass | FSM is `RECORD_OPEN_INITIAL`; safe transition is `checkpoint_progress`; run-state is available. | local |
| `cargo test -p nils-zsh-kit` | pass | zsh-kit setup orchestration integration tests passed. | local |
| `zsh -n completions/zsh/_zsh-kit` | pass | Generated Zsh completion syntax check passed. | local |
| `bash -n completions/bash/zsh-kit` | pass | Generated Bash completion syntax check passed. | local |
| `bash scripts/workspace-bins.sh --release-default \| rg '^zsh-kit$'` | pass | Release-default binary inventory includes `zsh-kit`. | local |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` | pass | Docs, third-party artifact audit, formatting, clippy, workspace nextest, and doctests passed. | local |
| `scripts/publish-crates.sh --dry-run --crate nils-zsh-kit` | blocked | Packaging reaches crates.io dependency resolution and fails because `nils-build-info 1.0.6` is not yet published. | local |
| `scripts/publish-crates.sh --dry-run --crate nils-agent-docs` | blocked | Existing publishable crate fails for the same unreleased `nils-build-info 1.0.6` dependency, confirming the blocker is release-sequence state rather than zsh-kit-specific packaging. | local |
| `cargo publish -p nils-build-info --dry-run --locked` | pass | Internal dependency dry-run packaging succeeds and can be published first in Sprint 3. | local |
