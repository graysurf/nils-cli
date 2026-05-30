# repo-retro Report v2 — Signal-vs-Noise Pre-Digestion Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: Sprint 1 complete — the repo-retro v2 pre-digestion layer landed on
  `feat/repo-retro-report-v2` and passes the local-fast gate; PR1 delivery in
  progress. Sprint 2 (release) and Sprint 3 (consumer + pin refresh) remain.
- Target scope: `repo-retro` report pre-digestion layer + schema v2 in
  `crates/agent-workflow-primitives` (`sympoies/nils-cli`), then a release and
  the lockstep `agent-runtime-kit` consumer / pin refresh.
- Execution window: Sprint 1 (pre-digestion + schema v2, PR1) → Sprint 2
  (release + tap bump) → Sprint 3 (agent-runtime-kit consumers + pin bump),
  serial.
- Current task: Task 2.1 — cut release + Homebrew tap bump.
- Next task: Task 3.1 — agent-runtime-kit consumer refresh + surface-pin bump.
- Last updated: 2026-05-31
- Branch/commit/PR: `feat/repo-retro-report-v2`; Sprint 1 implementation commit
  `0c0aad9`; PR1 delivery in progress.
- Source document: docs/plans/repo-retro-report-v2-predigest/repo-retro-report-v2-predigest-plan.md
- Direct source-doc execution waiver: not applicable
- Tracking issue: sympoies/nils-cli#693
- Source snapshot: posted by `create-plan-tracking-issue` at issue open
- Plan snapshot: posted by `create-plan-tracking-issue` at issue open
- Initial state snapshot: posted by `create-plan-tracking-issue` at issue open

## Validation Plan

- Sprint 1: `cargo test` for the `agent-workflow-primitives` crate covering the
  path-class classifier + override (AC5), `churnByClass` reconciliation (AC2),
  commit-frequency ranking + `class` / `netDeleted` flags (AC3), the
  archival-aware follow-up guard (AC1), the theme rewrite (AC4), and the v2
  schema strings (AC6); JSON-contract + completion-matrix updated; clippy
  `-D warnings`, fmt, `rumdl` clean; `bash
  scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` and `gh pr checks`
  green.
- Sprint 2: release workflow green; `repo-retro --version` and a v2 report
  confirmed on the published binary; Homebrew tap resolves.
- Sprint 3: `agent-runtime doctor --class version-alignment` clean; the
  `meta:repo-retro` / `reporting:project-retro` consumers read v2 without
  removed v1 fields (AC7); agent-runtime-kit project-dev validation green.
- Cross-cutting: every executed task populates its `Evidence` cell; waived
  tasks are marked `waived` with a reason. The closeout comment is preceded by
  a final `tracking run update --note "<closing summary>"` event.

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | done | Path-class classifier + config (replace `top_level_area`) | classify_path + --path-class-config JSON override; tests classify_path_uses_built_in_defaults, path_class_config_override_takes_precedence; commit 0c0aad9 | Classes source/tests/productDocs/processArtifacts/other; built-in defaults + repo-local override; absent conventions yield empty classes. |
| 1.2 | done | L2 fields: `churnByClass`, `archival`, commit-frequency hotspots | churnByClass (reconciles to summary), archival (net-deletion primary), commit-frequency topFiles w/ class+netDeleted; tests churn_by_class_reconciles_to_summary_total, hotspots_rank_by_commit_count_and_carry_class_and_net_deleted, net_deletion_drives_archival_facts; commit 0c0aad9 | Depends on 1.1. Class sums reconcile to `summary.changedLines`; net-deletion is the primary archival signal; `topFiles` ranked by `commits` with `class` + `netDeleted`. |
| 1.3 | done | Schema v2 bump + noise-aware L3 rewrite | schema v2 (no v1 dual-emit) + themes/follow-ups read class split with net-deletion guard; test analysis_skips_net_deleted_files_and_splits_churn_by_class; e2e on agent-runtime-kit no longer nominates archived plan; commit 0c0aad9 | Depends on 1.2. `...report.v2`, no v1 dual-emit; themes lead with source/tests churn; follow-ups never nominate a `netDeleted` file. |
| 1.4 | done | Contract, completion, tests, PR1 delivery | integration tests + crate README to v2; nils-cli-checks-entrypoint.sh --local-fast green (122/122, fmt+clippy+doc-tests); completion matrix unchanged (clap-derived flag) | Depends on 1.3. Update `cli-output-contract-v1.md` + completion matrix; local-fast gate + `gh pr checks` self-gated. |
| 2.1 | todo | Cut release + Homebrew tap bump | — | Depends on 1.4. Published surface must carry repo-retro v2. |
| 3.1 | todo | agent-runtime-kit consumer refresh + pin bump | — | Depends on 2.1. Refresh `meta:repo-retro` + `reporting:project-retro`; bump surface pin via `meta:nils-cli-bump`; EXACT-match gate forces lockstep. |

## Session Log

- 2026-05-31: Authored this bundle (discussion-source + plan +
  execution-state) for the repo-retro report v2 signal-vs-noise pre-digestion
  work. Findings grounded in `crates/agent-workflow-primitives/src/repo_retro.rs`:
  hotspots ranked by raw `changed_lines` (`992-999`); `top_level_area`
  (`1095-1097`) collapses all `docs/**` into one bucket; `themes` emits the
  bare "`<area>` largest movement" off `top_areas.first()` (`1747-1752`); the
  follow-up nominates `top_files.first()` with no net-deletion guard
  (`1808-1813`) — observed nominating an archived 0-insertion / 751-deletion
  plan. `FileChangeSummary` (`324-330`) already carries `commits`, so
  commit-frequency ranking and `netDeleted` are derived, not new collection.
  Conclusion: add a deterministic L2 pre-digestion layer, bump schema to v2
  (no backward compat), and rebuild L3 to read L2; stage as nils-cli ship →
  release → agent-runtime-kit consumer + pin refresh. No implementation
  started; this state is prepared so `create-plan-tracking-issue` can open the
  tracker. Authored in an isolated worktree
  (`~/Project/sympoies/nils-cli-wt/repo-retro-report-v2-predigest`,
  `feat/repo-retro-report-v2`) to avoid disturbing the shared `nils-cli` main
  checkout.
- 2026-05-31: Implemented Sprint 1 in the worktree. Added `PathClass` +
  `classify_path` with a `--path-class-config` JSON override
  (`{ "<class>": ["<prefix>", ...] }` merged over built-in defaults; Markdown
  is classified before the `is_test_path` heuristic so `docs/specs` is not
  mistaken for tests). Added `git.churnByClass` (reconciling to the summary
  total), `git.archival` (net-deletion primary, with a secondary
  `plansArchivedEstimate` commit-subject heuristic), and re-ranked
  `fileHotspots.topFiles` by commit-touch count with `class` + `netDeleted`.
  Bumped both schema strings to `...report.v2` (no v1 dual-emit) and rewrote
  the analysis layer to read the class split and skip net-deleted files. All
  4 Sprint 1 tasks `done` (commit `0c0aad9`); integration tests + crate README
  updated to v2; golden fixtures re-blessed. `--local-fast` gate green
  (122/122). e2e against agent-runtime-kit confirms the previously-nominated
  archived plan is no longer surfaced for review. PR1 delivery and the full
  `gh pr checks` self-gate are next.

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `cargo test -p nils-agent-workflow-primitives --lib repo_retro` | pass | 8 repo_retro unit tests green incl. classifier+override, churn reconciliation, commit-frequency ranking, archival, and the analysis net-deletion guard. | local |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` | pass | 122/122 package tests, fmt, clippy `-D warnings`, doc-tests, markdown lint all green. | local |
| `repo-retro report --repo <agent-runtime-kit> --days 3 --format json` | pass | v2 envelope; churnByClass separates source/process; the previously-nominated archived plan is no longer in followUps. | local e2e |

## Notes

- Schema v2 is intentionally not backward compatible ([D3]); every consumer
  updates in lockstep. agent-runtime-kit's EXACT-match surface-pin gate blocks
  its pushes until the pin matches the released v2 surface, enforcing the
  Sprint 3 lockstep.
- The handoff first landed as a crate-local `transient-dev-record` at
  `crates/agent-workflow-primitives/docs/reports/repo-retro-report-v2-predigest.md`
  and was graduated (moved) into this `docs/plans/` bundle on L2 escalation;
  the original reports-path copy is retired.
- Authored in worktree
  `~/Project/sympoies/nils-cli-wt/repo-retro-report-v2-predigest` on branch
  `feat/repo-retro-report-v2`; the shared `nils-cli` main checkout was not
  disturbed.
