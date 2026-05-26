<!-- execute-from-tracking-issue:state:v1 -->
# plan-issue closeout `Required` column rendering fix — Execution State

## Execution State

- Status: ready
- Target scope: whole plan
- Execution window: whole plan (single sprint)
- Current task: Task 1.1
- Next task: Introduce `GhRunner` abstraction and migrate all `Self::run` call sites
- Last updated: 2026-05-26 Asia/Taipei
- Branch/commit/PR/release: `feat/plan-issue-closeout-required-check-rendering-bundle`; PR pending
- Source document: docs/plans/plan-issue-closeout-required-check-rendering/plan-issue-closeout-required-check-rendering-plan.md
- Discussion source document: docs/plans/plan-issue-closeout-required-check-rendering/plan-issue-closeout-required-check-rendering-discussion-source.md
- Source issue: sympoies/nils-cli#541 (post-merge closeout-comment observation; bug surfaces visible at <https://github.com/sympoies/nils-cli/issues/541#issuecomment-4543937296>)
- Tracking issue: pending (this bundle drives `plan-issue record open`)
- Source snapshot: pending
- Plan snapshot: pending
- Initial execution state snapshot: pending
- Direct source-doc execution waiver: not applicable

## Task Ledger

| ID       | Status  | Task                                                                    | Evidence | Notes                                                              |
| -------- | ------- | ----------------------------------------------------------------------- | -------- | ------------------------------------------------------------------ |
| Task 1.1 | pending | Introduce `GhRunner` abstraction and migrate all `Self::run` call sites | —        | scope expansion past `pr_required_summary` per Decision 5          |
| Task 1.2 | pending | Repair `pr_required_summary` live probe                                 | —        | drops `conclusion` from `--json`; adds `no required checks` branch |
| Task 1.3 | pending | Widen renderer to five-label `Required` column                          | —        | `none required` / `pass (N)` / `fail (N)` / `none` / `unknown`     |
| Task 1.4 | pending | Probe and renderer integration coverage                                 | —        | injected-runner unit test + closeout-render integration assertions |
| Task 1.5 | pending | CHANGELOG, manual live verification, and PR                             | —        | embeds before/after rendering of `Required` column                 |

## Validation

| Command                                                                                                                                                             | Status  | Summary                                                                              | Artifact |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------- | ------------------------------------------------------------------------------------ | -------- |
| `plan-tooling validate --file docs/plans/plan-issue-closeout-required-check-rendering/plan-issue-closeout-required-check-rendering-plan.md --format text --explain` | pending | bundle gate                                                                          | —        |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`                                                                                                         | pending | docs hygiene, placement, rumdl, plan-bundle, cli-output-contract, forge-cli fixture  | —        |
| `cargo test -p plan-issue-cli`                                                                                                                                      | pending | per-crate gate (must pass without workspace `serde_json/preserve_order` unification) | —        |
| `cargo nextest run --workspace`                                                                                                                                     | pending | workspace gate                                                                       | —        |
| `cargo clippy -p plan-issue-cli --all-targets --all-features -- -D warnings`                                                                                        | pending | crate-scope clippy                                                                   | —        |

## Blockers

- none

## Session Log

- 2026-05-26: Bundle drafted. Source doc previously merged at PR #558
  (commit `8741a79` on `main`). Decisions locked at source-doc fold-in
  `0d09ccd`: label string `none required`; `GhRunner` covers every
  `github.rs` `Self::run` caller; GitLab fallback at
  `forge_cli_adapter.rs:353-354` explicitly out of scope (tracked at
  sympoies/nils-cli#557). Repository sweep confirms no other consumer
  of the closeout-table `Required` cell strings (free-form Markdown
  only).
