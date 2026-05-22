<!-- execute-from-tracking-issue:state:v1 -->
# forge-cli GitHub Checks Compatibility Execution State

## Execution State

- Status: complete
- Target scope: whole issue
- Execution window: whole issue
- Current task: complete
- Next task: downstream Plan 05 Sprint 6 resume in `graysurf/agent-runtime-kit`
- Last updated: 2026-05-22 20:09 Asia/Taipei
- Branch/commit/PR/release: `main` at `c1a86d6`; merged PR
  [#440](https://github.com/sympoies/nils-cli/pull/440); release
  [v0.17.0](https://github.com/sympoies/nils-cli/releases/tag/v0.17.0)
- Source document:
  docs/plans/forge-cli-gh-292-checks/forge-cli-gh-292-checks-plan.md
- Direct source-doc execution waiver: not applicable

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| Task 1.1 | done | Add gh 2.92.0 field-set fixtures | `cargo test -p nils-forge-cli pr_checks_github` pass | Fixtures omit `conclusion` and `isRequired`; added required-only fixture. |
| Task 1.2 | done | Pin required-only behavior expectations | `cargo test -p nils-forge-cli pr_wait_checks`; `cargo test -p nils-forge-cli required_check_gate` pass | Required-only gating uses explicit `gh pr checks --required` evidence. |
| Task 2.1 | done | Replace unsupported GitHub checks fields | `cargo test -p nils-forge-cli pr_checks` pass | Backend command no longer requests `conclusion` or `isRequired`; state normalization handles bucket/state. |
| Task 2.2 | done | Implement required-only gating source | Targeted forge-cli tests pass | No-required-check backend errors are normalized to zero required checks. |
| Task 2.3 | done | Verify merge and deliver consumers | `cargo test -p nils-forge-cli pr_merge`; `cargo test -p nils-forge-cli pr_deliver` pass | `pr deliver` dry-run now uses the fixed required checks command shape. |
| Task 3.1 | done | Update docs and changelog for compatibility fix | `rg -n "gh 2\\.92\\.0|0\\.17\\.0|checks" crates/forge-cli docs` pass | No root `CHANGELOG.md` exists; recorded release note in forge-cli README/specs. |
| Task 3.2 | done | Run full local gate | `bash scripts/ci/nils-cli-checks-entrypoint.sh` pass | Includes nextest 3484/3484 and workspace doc tests. |
| Task 4.1 | done | Cut nils-cli 0.17.0 | `.agents/skills/nils-cli-bump-version-tag-release/scripts/nils-cli-bump-version-tag-release.sh --version 0.17.0 --ci-gate-main`; `forge-cli --version`; `gh release view v0.17.0 --repo sympoies/nils-cli` pass | Source release and Homebrew tap release completed; local brew install reports `forge-cli 0.17.0`. |
| Task 4.2 | done | Record downstream handoff | Comment posted to `graysurf/agent-runtime-kit` issue #26 | Downstream issue now records PR #440, release `v0.17.0`, local binary verification, and Sprint 6 unblock action. |

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `agent-docs --docs-home "$HOME/.config/agent-kit" resolve --context startup --strict --format checklist` | pass | Required startup preflight passed earlier in this execution session. | terminal log |
| `agent-docs --docs-home "$HOME/.config/agent-kit" resolve --context project-dev --strict --format checklist` | pass | Project-dev preflight passed earlier in this execution session. | terminal log |
| `agent-docs --docs-home "$HOME/.config/agent-kit" resolve --context task-tools --strict --format checklist` | pass | Task-tools preflight passed earlier in this execution session before GitHub operations. | terminal log |
| `cargo test -p nils-forge-cli pr_checks_github` | pass | 8 GitHub checks integration tests passed. | terminal log |
| `cargo test -p nils-forge-cli pr_wait_checks` | pass | `pr wait-checks` unit/integration filter passed. | terminal log |
| `cargo test -p nils-forge-cli required_check_gate` | pass | Required-check gate unit/integration filter passed. | terminal log |
| `cargo test -p nils-forge-cli pr_merge` | pass | Merge dry-run/unit integration filter passed. | terminal log |
| `cargo test -p nils-forge-cli pr_deliver` | pass | Deliver dry-run/full-chain integration filter passed. | terminal log |
| `cargo test -p nils-forge-cli pr_checks` | pass | 33 unit + 16 integration checks tests passed. | terminal log |
| `cargo test -p nils-forge-cli` | pass | 221 unit + 94 integration + doc-test pass. | terminal log |
| `cargo clippy -p nils-forge-cli --all-targets -- -D warnings` | pass | No warnings. | terminal log |
| `bash scripts/ci/plan-bundle-validate.sh --strict` | pass | Plan bundle validation passed after adding `Source document` to execution state. | terminal log |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh` | pass | All required checks passed; nextest 3484/3484, workspace doc tests pass. | terminal log |
| `deliver-github-pr.sh --kind bug wait-checks --pr 440 --max-wait-seconds 60 --poll-seconds 10` | pass | Required GitHub checks gate passed after shared helper repair for optional skipped fallback checks. | terminal log |
| `deliver-github-pr.sh --kind bug close --pr 440` | pass | PR #440 marked ready, merged, branch cleaned up, and local `main` fast-forwarded. | PR [#440](https://github.com/sympoies/nils-cli/pull/440) |
| `.agents/skills/nils-cli-bump-version-tag-release/scripts/nils-cli-bump-version-tag-release.sh --version 0.17.0 --ci-gate-main` | pass | Main CI gate was green; release commit/tag pushed; source release and Homebrew tap release completed. | release [v0.17.0](https://github.com/sympoies/nils-cli/releases/tag/v0.17.0) |
| `forge-cli --version` | pass | Reported `forge-cli 0.17.0`. | terminal log |
| `gh release view v0.17.0 --repo sympoies/nils-cli --json tagName,url,publishedAt` | pass | Release is published at `2026-05-22T12:06:04Z`. | release [v0.17.0](https://github.com/sympoies/nils-cli/releases/tag/v0.17.0) |

## Runtime Findings

- `fix-now`: released `forge-cli 0.16.0` requests unsupported GitHub checks JSON
  fields with `gh 2.92.0`; fixed in the current PR branch.
- `fix-now`: local gate found markdown table alignment drift in the forge-cli
  spec after docs edits; fixed with `rumdl fmt`.
- `fix-now`: plan-bundle gate required the new execution state to include
  `Source document`; fixed in this ledger.
- `fix-now`: remote CI found MD033 inline HTML in this execution-state ledger;
  replaced the collapsible ledger with pure Markdown.
- `fix-now`: shared `agent-kit` GitHub PR delivery helper treated optional
  skipped checks as failed when a repository reports no required checks; fixed
  in `graysurf/agent-kit@09bf4d9`, then re-ran the PR #440 checks gate.

## Blockers

- none

## Session Log

### 2026-05-22 18:08 Asia/Taipei

- Read issue #439 source and plan snapshots.
- Confirmed no prior `execute-from-tracking-issue:state:v1` comment existed.
- Confirmed branch `feat/issue-439-gh-checks-compat` contains local plan commit
  `b7b5e45` and one working-tree production edit in `pr_checks.rs`.
- Initialized issue-backed execution state before continuing tests and PR work.

### 2026-05-22 19:17 Asia/Taipei

- Changed GitHub checks backend to request only the `gh 2.92.0` supported
  fields and to use `gh pr checks --required` for required-only gating.
- Updated fixtures/tests for all-success, mixed-failure, pending, cancelled,
  empty, and no-required-check cases without `conclusion` / `isRequired`.
- Updated `pr deliver` dry-run planning and forge-cli docs/specs to use the
  fixed checks command shape.
- Validated targeted forge-cli filters, full `nils-forge-cli`, and full
  `bash scripts/ci/nils-cli-checks-entrypoint.sh`.
- Fixed two small local gate issues: markdown table alignment and missing
  execution-state `Source document`.
- Next: commit, open PR, run mandatory specialist review and remote checks.

### 2026-05-22 19:29 Asia/Taipei

- Opened draft PR
  [#440](https://github.com/sympoies/nils-cli/pull/440)
  for Tasks 1.1 through 3.2.
- Remote CI exposed strict markdown lint failure on this execution-state file
  (`MD033` for `<details>/<summary>`); fixed by converting the ledger to plain
  Markdown.

### 2026-05-22 20:09 Asia/Taipei

- Repaired the shared `agent-kit` GitHub PR checks helper so optional skipped
  checks do not block the no-required-checks fallback path.
- Re-ran the PR #440 delivery checks gate, posted the mandatory delivery review
  outcome comment, and merged PR #440.
- Released `nils-cli` `0.17.0` using the standard release skill; source
  release and Homebrew tap release completed, and local `forge-cli --version`
  reports `0.17.0`.
- Posted downstream release handoff to
  [agent-runtime-kit issue #26](https://github.com/graysurf/agent-runtime-kit/issues/26#issuecomment-4518516482).
