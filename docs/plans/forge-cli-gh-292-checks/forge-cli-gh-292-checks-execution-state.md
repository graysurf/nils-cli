<!-- execute-from-tracking-issue:state:v1 -->
# forge-cli GitHub Checks Compatibility Execution State

## Execution State

- Status: validating
- Target scope: whole issue
- Execution window: whole issue
- Current task: PR delivery for Tasks 1.1 through 3.2
- Next task: Task 4.1 release `nils-cli` `0.17.0` after PR merge
- Last updated: 2026-05-22 19:17 Asia/Taipei
- Branch/commit/PR: `feat/issue-439-gh-checks-compat`; PR not opened yet
- Source document:
  docs/plans/forge-cli-gh-292-checks/forge-cli-gh-292-checks-plan.md
- Direct source-doc execution waiver: not applicable

<details>
<summary>Full task ledger</summary>

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| Task 1.1 | done | Add gh 2.92.0 field-set fixtures | `cargo test -p nils-forge-cli pr_checks_github` pass | Fixtures omit `conclusion` and `isRequired`; added required-only fixture. |
| Task 1.2 | done | Pin required-only behavior expectations | `cargo test -p nils-forge-cli pr_wait_checks`; `cargo test -p nils-forge-cli required_check_gate` pass | Required-only gating uses explicit `gh pr checks --required` evidence. |
| Task 2.1 | done | Replace unsupported GitHub checks fields | `cargo test -p nils-forge-cli pr_checks` pass | Backend command no longer requests `conclusion` or `isRequired`; state normalization handles bucket/state. |
| Task 2.2 | done | Implement required-only gating source | Targeted forge-cli tests pass | No-required-check backend errors are normalized to zero required checks. |
| Task 2.3 | done | Verify merge and deliver consumers | `cargo test -p nils-forge-cli pr_merge`; `cargo test -p nils-forge-cli pr_deliver` pass | `pr deliver` dry-run now uses the fixed required checks command shape. |
| Task 3.1 | done | Update docs and changelog for compatibility fix | `rg -n "gh 2\\.92\\.0|0\\.17\\.0|checks" crates/forge-cli docs` pass | No root `CHANGELOG.md` exists; recorded release note in forge-cli README/specs. |
| Task 3.2 | done | Run full local gate | `bash scripts/ci/nils-cli-checks-entrypoint.sh` pass | Includes nextest 3484/3484 and workspace doc tests. |
| Task 4.1 | pending | Cut nils-cli 0.17.0 | Planned: standard release workflow; `forge-cli --version`; `gh release view v0.17.0 --repo sympoies/nils-cli` | Release only after merged fix and green gates. |
| Task 4.2 | pending | Record downstream handoff | Planned: comment on `graysurf/agent-runtime-kit` issue #26 | Downstream owns skill floor/backlog update after release evidence exists. |

</details>

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

## Runtime Findings

- `fix-now`: released `forge-cli 0.16.0` requests unsupported GitHub checks JSON
  fields with `gh 2.92.0`; fixed in the current PR branch.
- `fix-now`: local gate found markdown table alignment drift in the forge-cli
  spec after docs edits; fixed with `rumdl fmt`.
- `fix-now`: plan-bundle gate required the new execution state to include
  `Source document`; fixed in this ledger.

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
