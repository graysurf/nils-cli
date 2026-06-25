# agent-memory CLI Capabilities Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: tracking issue open (#940); Sprint 1 `check` delivered as PR #941
  (open, ready, all CI green, author graysurf) — awaiting merge. Task 1.5
  deferred to post-release (cross-repo, gated on a released nils-cli with
  `check`).
- Target scope: add four structural / scaffolding subcommands (`check`, `add`,
  `list --json`/`--type`, `search`) to the `nils-agent-memory` crate in
  `sympoies/nils-cli`, per the frozen `graysurf/agent-memory`
  `docs/cli-contract-proposed.md` contract.
- Execution window: Sprint 1 (`check` MVP + collapse the skill bash) -> Sprint 2
  (`add` atomic writer) -> Sprint 3 (`list --json`/`search`, docs, delivery,
  optional release), serial.
- Current task: Sprint 1 PR #941 open + green; awaiting merge.
- Next task: merge #941, then Sprint 2 (`add`); Task 1.5 after a release.
- Last updated: 2026-06-25
- Branch/commit/PR: branch `feat/agent-memory-cli-capabilities`; commits
  `8ee7767` + `a8ea4e4` (bundle), `532ac65` (`check`), `69d3127` (ledger),
  `15bca42` (completion regen); PR
  <https://github.com/sympoies/nils-cli/pull/941>.
- Source document:
  `docs/plans/2026-06-25-agent-memory-cli-capabilities/agent-memory-cli-capabilities-discussion-source.md`
- Plan document:
  `docs/plans/2026-06-25-agent-memory-cli-capabilities/agent-memory-cli-capabilities-plan.md`
- Direct source-doc execution waiver: not applicable.
- Tracking issue: <https://github.com/sympoies/nils-cli/issues/940>
- Source snapshot:
  <https://github.com/sympoies/nils-cli/issues/940#issuecomment-4796671780>
- Plan snapshot:
  <https://github.com/sympoies/nils-cli/issues/940#issuecomment-4796672142>
- Initial state snapshot:
  <https://github.com/sympoies/nils-cli/issues/940#issuecomment-4796672556>

## Validation Plan

- Bundle creation: validate the plan-source bundle before opening the tracker.
- Tracker creation: dry-run `plan-issue record open`, then live-create only if
  labels, title, issue body, lifecycle comments, and repo are correct.
- Initial read-back: audit the live issue with `record audit --profile tracking
  --expect-visible`.
- Sprint 1: targeted `nils-agent-memory` tests for the `check` surface,
  structural checks, frontmatter schema, JSON output, and exit codes.
- Sprint 2: `add` write + atomic index-line append tests, with a post-write
  `check` assertion.
- Sprint 3: `list --json`/`--type` and `search` tests, docs-only validation,
  local-fast validation, and provider PR checks.

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | done | Define the `check` command surface | `532ac65` cli.rs | `--all`/`--strict` + canonical `--format` with hidden `--json` alias (per CLI output contract). |
| 1.2 | done | Implement the structural checks | `532ac65` check.rs | Index/file parity, dangling `[[links]]` (warn), broken index links. |
| 1.3 | done | Implement frontmatter schema validation | `532ac65` check.rs | Required name/description/metadata.type+enum; warn-level node_type/originSessionId; hand-parsed (no new dep). |
| 1.4 | done | JSON output, exit codes, and report | `532ac65` check.rs | `--format json` findings under `cli.agent-memory.check.v1`; `--strict` promotes; exit 0/1/64. |
| 1.5 | deferred | Collapse review-global-memory.sh onto the command | pending | Cross-repo (graysurf/agent-memory); gated on a released nils-cli with `check`. |
| 2.1 | todo | Define `add` and write the note file | pending | Frontmatter writer; enum + duplicate-slug refusal. |
| 2.2 | todo | Atomic index-line append | pending | `check` clean after `add`; no half-writes. |
| 3.1 | todo | `list --json` and `--type` | pending | Stable JSON; default output unchanged. |
| 3.2 | todo | `agent-memory search` | pending | Body + description match across scopes. |
| 3.3 | todo | Docs, help text, and completion | pending | Update crate docs + memory-repo README; no private paths. |
| 3.4 | todo | Validate and deliver the nils-cli PR(s) | pending | local-fast + provider checks; link PRs to tracker. |
| 3.5 | todo | Release and runtime-surface follow-up | pending | Release if needed to unblock 1.5; else record deferral. |

## Session Log

- 2026-06-25: Operator evaluated the live `agent-memory` CLI surface, agreed the
  daily memory loop bypasses it and the structural checks are duplicated in the
  `review-global-memory` skill bash, authored the frozen contract
  (`graysurf/agent-memory` `docs/cli-contract-proposed.md`), and chose L2 plan
  tracking for all four proposals.
- 2026-06-25: Implemented Sprint 1 `check` test-first (RED captured, then 11
  integration tests green). Calibrated against the live store before coding:
  dropped a `name`-equals-filename check (kebab `name:` vs snake filenames
  differ in 19/30 notes) and made dangling `[[wikilinks]]` warn-level (the
  harness blesses forward references). Adopted the workspace CLI output contract
  (`--format`/hidden `--json`, `schema_version`) after the contract lint flagged
  the initial bare `--json`. `check --all` is clean on the live store (exit 0).
  Committed `532ac65`.
- 2026-06-25: Opened PR #941 (graysurf, via forge-cli routing). First CI run
  failed `test`/`test_macos` on the completion-freshness audit — adding a
  subcommand staled the committed `completions/{bash,zsh}/agent-memory`
  snapshots, which `--local-fast` does not regenerate (it only tests changed
  packages). Regenerated both from the binary, audit passed, committed
  `15bca42`, re-pushed. Second run all green (12 success / 1 skipped); promoted
  the PR to ready. LESSON: any new nils-cli subcommand requires a completion
  regen + full-suite check, not just `--local-fast`.

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `plan-tooling validate --file docs/plans/2026-06-25-agent-memory-cli-capabilities/agent-memory-cli-capabilities-plan.md --format text --explain` | pass | Plan valid; 0 errors. | local |
| `bash scripts/ci/plan-bundle-validate.sh --strict --file docs/plans/2026-06-25-agent-memory-cli-capabilities/agent-memory-cli-capabilities-plan.md` | pass | Strict plan-bundle validation passed. | local |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` | pass | Docs placement, hygiene, markdown lint, plan-bundle, CLI contract passed. | local |
| `plan-issue --repo sympoies/nils-cli --format json --dry-run record open --profile tracking ...` | pass | Dry-run rendered the intended issue body, labels, and source/plan/state comments; no local paths. | local |
| `plan-issue --repo sympoies/nils-cli --format json record open --profile tracking ...` | pass | Opened tracker #940 and posted source, plan, and initial state snapshots. | <https://github.com/sympoies/nils-cli/issues/940> |
| `plan-issue --format json record audit --profile tracking --expect-visible ...` | pass | Read-back clean: recognized_count 3, missing_required [], visible.overall_pass true. | <https://github.com/sympoies/nils-cli/issues/940> |
| `cargo test -p nils-agent-memory` | pass | 38 tests pass (11 new `check` integration tests). | local |
| `cargo clippy -p nils-agent-memory --all-targets -- -D warnings` | pass | No warnings. | local |
| `bash scripts/ci/cli-output-contract-lint.sh --strict` | pass | `--json` is a hidden alias for `--format json`. | local |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` | pass | Docs + package checks (fmt, clippy, nextest, doctests) clean. | local |
| `agent-memory check --all --format json` (live store) | pass | 3 scopes clean, exit 0, `schema_version cli.agent-memory.check.v1`. | local |
| `bash scripts/ci/completion-freshness-audit.sh --strict --bin agent-memory` | pass | After regenerating committed completions for `check` (`15bca42`). | local |
| PR #941 CI (test, test_macos, coverage, Analyze x3, cargo-deny, CodeQL) | pass | 12 success / 1 skipped after the completion regen. | <https://github.com/sympoies/nils-cli/pull/941> |
