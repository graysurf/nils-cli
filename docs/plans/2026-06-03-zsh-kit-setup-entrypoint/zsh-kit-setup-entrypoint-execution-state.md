# zsh-kit Setup Entrypoint Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: complete; tracking issue closed
- Target scope: nils-cli `zsh-kit` setup entrypoint, the repo-owned Zsh setup
  hook it dispatches to, and the follow-up agent-runtime-kit Docker/pin/docs
  consumption after nils-cli release.
- Execution window: Sprint 1 (`nils-cli` CLI contract, implementation, tests,
  completions, release packaging, PR1) -> Sprint 2 (Zsh repository setup hook,
  PR2) -> Sprint 3 (nils-cli release and agent-runtime-kit consumption, PR3),
  serial.
- Current task: none - implementation complete.
- Next task: none - tracker closed.
- Last updated: 2026-06-03
- Branch/commit/PR: sympoies/nils-cli#763 merged
  (<https://github.com/sympoies/nils-cli/pull/763>); graysurf/zsh-kit#71
  merged (<https://github.com/graysurf/zsh-kit/pull/71>);
  sympoies/nils-cli#765 merged
  (<https://github.com/sympoies/nils-cli/pull/765>);
  graysurf/agent-runtime-kit#268 merged
  (<https://github.com/graysurf/agent-runtime-kit/pull/268>)
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
| 1.3 | done | Workspace integration, completions, and release packaging | Added workspace member, completions, completion matrix, release order, README/docs entries, third-party artifacts, workspace binary inventory coverage, publish dry-run preparation, and local-fast validation.; scripts/publish-crates.sh --dry-run --crate nils-zsh-kit reaches packaging but fails because nils-build-info 1.0.6 is not yet published on crates.io; control dry-run for existing nils-agent-docs fails the same way, while cargo publish -p nils-build-info --dry-run --locked passes.; Release sequence completed for 1.0.7: GitHub release v1.0.7 produced release assets, Homebrew tap updated, local nils-cli/zsh-kit reports 1.0.7, and crates.io publish workflow 26890782084 published nils-zsh-kit plus all internal dependencies at 1.0.7. | Publish dry-run blocker resolved by real Sprint 3 release and crates.io publish sequence. |
| 2.1 | done | Add a stable setup hook to the Zsh repository | Zsh repository setup hook shipped in graysurf/zsh-kit#71 (merged as a993e6aa20c8b988ef0eb0af2954054446bfdffd); ./tools/check.zsh, ./tools/check.zsh --smoke, ./tests/run.zsh, markdownlint audit, and nils-zsh-kit apply dispatch smoke passed. | Stable hook path is bootstrap/zsh-kit-setup.zsh; AGENTS branch policy now targets main. |
| 3.1 | done | Release nils-cli with zsh-kit | Released nils-cli v1.0.7 in PR #765/tag v1.0.7; release workflow 26889552946 completed successfully with 8 release assets; tap workflow 26890615943 completed; local Homebrew nils-cli and zsh-kit are 1.0.7. | Release artifacts include the workspace binary set, including zsh-kit and completion assets. |
| 3.2 | done | Update agent-runtime-kit Docker and docs | Updated agent-runtime-kit Docker/pin/docs in graysurf/agent-runtime-kit#268 (merged as 9f9effdfa3bff8dada2682d66d3acb9324e4c9d1): pinned nils-cli v1.0.7, added zsh-kit floor, installed public zsh/openssh-client prerequisites, documented runtime setup with operator-supplied repo URL/auth, and validated Docker build/smoke plus repo gates. | Dockerfile does not copy private Zsh repositories; runtime setup fetches operator-supplied shell repos via zsh-kit. |

## Session Log

- 2026-06-03: Authored this bundle after the operator approved the hybrid
  design: nils-cli provides a stable `zsh-kit` entrypoint, the Zsh repository
  owns shell behavior, and `agent-runtime-kit` only adds public prerequisites
  such as `zsh` without baking personal shell config or private repositories
  into the image.
- 2026-06-03: Delivered Sprint 2 in the Zsh repository: added
  `bootstrap/zsh-kit-setup.zsh`, documented runtime setup, removed stale
  `nils-cli` branch-base policy from `AGENTS.md`, validated locally, and merged
  <https://github.com/graysurf/zsh-kit/pull/71> into `main`.
- 2026-06-03: Released nils-cli v1.0.7 with `zsh-kit`, completed the Homebrew
  tap update, verified local `zsh-kit --version`, and published 36/36
  workspace crates to crates.io, including `nils-zsh-kit` and its internal
  dependencies.
- 2026-06-03: Delivered downstream agent-runtime-kit consumption in
  <https://github.com/graysurf/agent-runtime-kit/pull/268>: bumped the
  `nils-cli` pin to v1.0.7, added the `zsh-kit` floor, installed public Docker
  shell prerequisites, documented runtime setup with operator-supplied repo
  URL/auth, and validated Docker plus repo gates.

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `plan-tooling validate --file docs/plans/2026-06-03-zsh-kit-setup-entrypoint/zsh-kit-setup-entrypoint-plan.md --format text --explain` | pass | Plan-source bundle validated with zero errors. | local |
| `bash scripts/ci/plan-bundle-validate.sh --strict --file docs/plans/2026-06-03-zsh-kit-setup-entrypoint/zsh-kit-setup-entrypoint-plan.md` | pass | Repository plan-bundle validator passed for this new bundle. | local |
| `agent-run exec --cwd $HOME/Project/sympoies/nils-cli -- bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` | pass | Docs placement, hygiene, markdown lint, cli-output contract lint, and forge-cli fixture lint passed. | local |
| `agent-run exec --cwd $HOME/Project/sympoies/nils-cli -- bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` | pass | Local-fast selected docs-only mode for this three-file plan bundle and passed. | local |
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
| `./tools/check.zsh` | pass | Zsh repository default check passed after adding `bootstrap/zsh-kit-setup.zsh` and updating `AGENTS.md` branch policy to `main`. | local |
| `./tools/check.zsh --smoke` | pass | Zsh isolated startup smoke check passed with the setup hook present. | local |
| `./tests/run.zsh` | pass | Zsh test suite passed, including `zsh-kit-setup.test.zsh` dry-run, invalid feature, and smoke coverage. | local |
| `bash ./scripts/ci/markdownlint-audit.sh --strict` | pass | Zsh markdown lint audit passed for `AGENTS.md` and README setup documentation changes. | local |
| `cargo run -p nils-zsh-kit -- setup --repo <zsh repo> --dest <agent-out dest> --apply --features docker --install-tools skip --format json` | pass | Cross-repo apply dispatch smoke passed: `nils-zsh-kit` cloned the committed Zsh hook into agent-out and executed it. | local |
| `.agents/skills/project-bump-version-tag-release/scripts/project-bump-version-tag-release.sh --version 1.0.7` | pass | Release PR #765 merged, tag `v1.0.7` pushed, release workflow `26889552946` passed, Homebrew tap workflow `26890615943` passed, and local Homebrew `nils-cli`/`zsh-kit` are `1.0.7`. | local/provider |
| `.agents/skills/project-dispatch-crates-io-publish/scripts/publish-crates-io.sh --all --ref main --wait` | pass | Publish workflow `26890782084` completed successfully and status snapshot reported 36/36 workspace crates published at `1.0.7`. | local/provider |
| `scripts/crates-io-status.sh --crate nils-build-info --crate nils-zsh-kit --crate nils-agent-docs --format text` | pass | `nils-build-info`, `nils-zsh-kit`, and `nils-agent-docs` all resolved as published at `1.0.7`. | local |
| `gh release view v1.0.7 --repo sympoies/nils-cli --json tagName,url,assets` | pass | Release `v1.0.7` exists with 8 assets: 4 platform tarballs and 4 SHA256 sidecars. | provider |
| `zsh-kit --version && brew list --versions nils-cli` | pass | Local Homebrew installation reports `nils-cli 1.0.7`, and `zsh-kit` reports `1.0.7`. | local |
| `agent-runtime doctor --class version-alignment --pin docs/source/nils-cli-pin.yaml --format text` | pass | agent-runtime-kit v1.0.7 pin and required CLI floors passed, including `zsh-kit >= 1.0.7`. | local |
| `docker/build.sh -t agent-runtime-kit:zsh-kit-runtime-setup` | pass | agent-runtime-kit Docker image built with nils-cli v1.0.7; build verified `agent-runtime`, `zsh-kit`, and `zsh` versions. | local |
| `docker run --rm -e AGENT_RUNTIME_KIT_QUIET=1 agent-runtime-kit:zsh-kit-runtime-setup zsh-kit setup --repo https://example.invalid/operator/zsh.git --dest /tmp/zsh-kit-dry-run --dry-run --features docker --install-tools skip --format json` | pass | Container dry-run smoke returned `cli.zsh-kit.setup.v1`, planned clone/hook actions, and made no runtime shell repo or private-skill copy. | local |
| `bash scripts/ci/all.sh` | pass | agent-runtime-kit full local CI passed all 13 positions after the Docker/pin/docs update. | local |
| `bash tests/hooks/run.sh` | pass | agent-runtime-kit shared hook contract suite passed 38/38 tests. | local |
| `forge-cli pr deliver --kind feature --base main --title "Consume zsh-kit runtime setup in Docker"` | pass | Delivered and merged agent-runtime-kit PR #268 as squash merge `9f9effdfa3bff8dada2682d66d3acb9324e4c9d1`. | provider |
