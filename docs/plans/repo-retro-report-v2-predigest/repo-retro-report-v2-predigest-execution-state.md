# repo-retro Report v2 — Signal-vs-Noise Pre-Digestion Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: complete — all three sprints delivered and merged; ready for close.
  repo-retro report v2 shipped in `nils-cli v0.31.0` and is adopted by
  agent-runtime-kit; the EXACT-match version-alignment pin gate is cleared.
- Target scope: `repo-retro` report pre-digestion layer + schema v2 in
  `crates/agent-workflow-primitives` (`sympoies/nils-cli`), then a release and
  the lockstep `agent-runtime-kit` consumer / pin refresh.
- Execution window: Sprint 1 (pre-digestion + schema v2, PR1) → Sprint 2
  (release + tap bump) → Sprint 3 (agent-runtime-kit consumers + pin bump),
  serial — all complete.
- Current task: none — closeout.
- Next task: none.
- Last updated: 2026-05-31
- Branch/commit/PR: Sprint 1 `sympoies/nils-cli#694` (squash `1d93bb5`);
  Sprint 2 release tag `v0.31.0` (`7a42080`, #695); Sprint 3
  `graysurf/agent-runtime-kit#201` (squash `442d17b`).
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
| 2.1 | done | Cut release + Homebrew tap bump | released nils-cli v0.31.0 (tag v0.31.0; release.yml + homebrew-tap workflows green; brew upgraded 0.30.2->0.31.0); published repo-retro emits report.v2 with churnByClass | Depends on 1.4. Published surface must carry repo-retro v2. |
| 3.1 | done | agent-runtime-kit consumer refresh + pin bump | agent-runtime-kit PR graysurf/agent-runtime-kit#201 merged (442d17b): pin v0.30.2->v0.31.0 + project-retro migrated to report v2 + goldens re-rendered; scripts/ci/all.sh positions 1-13 green; EXACT-match pin gate cleared | Depends on 2.1. Refresh `meta:repo-retro` + `reporting:project-retro`; bump surface pin via `meta:nils-cli-bump`; EXACT-match gate forces lockstep. |

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
- 2026-05-31: Delivered Sprint 1 as `sympoies/nils-cli#694` —
  `forge-cli pr deliver --no-merge`, self-gated the full `gh pr checks` set
  (test / test_macos / coverage / CodeQL all green), then `ready` + squash-merge
  `1d93bb5`. Posted state + validation checkpoints to #693.
- 2026-05-31: Sprint 2 — released `nils-cli v0.31.0` via the
  bump-version-tag-release flow: all crates `0.30.2 → 0.31.0`, release PR #695
  merged (tag `v0.31.0` at `7a42080`), `release.yml` + `sympoies/homebrew-tap`
  formula workflow green, `brew upgrade` to `0.31.0`. Verified the published
  `repo-retro 0.31.0` emits `cli.repo-retro.report.v2` with `churnByClass`.
- 2026-05-31: Sprint 3 — adopted v2 in agent-runtime-kit
  (`graysurf/agent-runtime-kit#201`, squash `442d17b`): bumped the surface pin
  `v0.30.2 → v0.31.0`, refreshed the surface snapshot, migrated the
  `project-retro` consumer to report v2, updated the runtime-smoke schema
  asserts, and re-rendered the goldens. `scripts/ci/all.sh` positions 1-13
  green; `version-alignment` `block=0`; the EXACT-match pin gate is cleared.
  All sprints complete; reconciled this ledger (`2.1` / `3.1` → done) and the
  tracker is being closed.

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `cargo test -p nils-agent-workflow-primitives --lib repo_retro` | pass | 8 repo_retro unit tests green incl. classifier+override, churn reconciliation, commit-frequency ranking, archival, and the analysis net-deletion guard. | local |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` | pass | 122/122 package tests, fmt, clippy `-D warnings`, doc-tests, markdown lint all green. | local |
| `repo-retro report --repo <agent-runtime-kit> --days 3 --format json` | pass | v2 envelope; churnByClass separates source/process; the previously-nominated archived plan is no longer in followUps. | local e2e |
| `gh pr checks 694` (Sprint 1) | pass | test / test_macos / coverage / CodeQL all green; squash-merged `1d93bb5`. | PR #694 |
| `repo-retro --version` on published `v0.31.0` (Sprint 2) | pass | brew `0.31.0`; emits `cli.repo-retro.report.v2` with `churnByClass`. | release `v0.31.0` |
| `bash scripts/ci/all.sh` in agent-runtime-kit (Sprint 3) | pass | positions 1-13 green; `version-alignment` `block=0` (host `v0.31.0` == pin); PR #201 CI green, squash-merged `442d17b`. | PR #201 |

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
