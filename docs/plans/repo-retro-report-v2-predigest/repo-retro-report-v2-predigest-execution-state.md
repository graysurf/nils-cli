# repo-retro Report v2 — Signal-vs-Noise Pre-Digestion Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: not started — bundle authored so `create-plan-tracking-issue` can
  open the tracker with a populated ledger.
- Target scope: `repo-retro` report pre-digestion layer + schema v2 in
  `crates/agent-workflow-primitives` (`sympoies/nils-cli`), then a release and
  the lockstep `agent-runtime-kit` consumer / pin refresh.
- Execution window: Sprint 1 (pre-digestion + schema v2, PR1) → Sprint 2
  (release + tap bump) → Sprint 3 (agent-runtime-kit consumers + pin bump),
  serial.
- Current task: Task 1.1 — path-class classifier + config.
- Next task: Task 1.2 — L2 fields (`churnByClass`, `archival`,
  commit-frequency hotspots).
- Last updated: 2026-05-31
- Branch/commit/PR: bundle on `feat/repo-retro-report-v2`; no implementation
  commits yet; PR1 not yet opened.
- Source document: docs/plans/repo-retro-report-v2-predigest/repo-retro-report-v2-predigest-plan.md
- Direct source-doc execution waiver: not applicable
- Tracking issue: to be assigned by `create-plan-tracking-issue` at open
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
| 1.1 | todo | Path-class classifier + config (replace `top_level_area`) | — | Classes source/tests/productDocs/processArtifacts/other; built-in defaults + repo-local override; absent conventions yield empty classes. |
| 1.2 | todo | L2 fields: `churnByClass`, `archival`, commit-frequency hotspots | — | Depends on 1.1. Class sums reconcile to `summary.changedLines`; net-deletion is the primary archival signal; `topFiles` ranked by `commits` with `class` + `netDeleted`. |
| 1.3 | todo | Schema v2 bump + noise-aware L3 rewrite | — | Depends on 1.2. `...report.v2`, no v1 dual-emit; themes lead with source/tests churn; follow-ups never nominate a `netDeleted` file. |
| 1.4 | todo | Contract, completion, tests, PR1 delivery | — | Depends on 1.3. Update `cli-output-contract-v1.md` + completion matrix; local-fast gate + `gh pr checks` self-gated. |
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

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| (pending) | — | No implementation run yet; bundle authored for tracker open. | — |

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
