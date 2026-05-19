# `plan-tooling validate` Ergonomics Improvement Record

- Status: open, awaiting implementation
- Date: 2026-05-18
- Source: hands-on session authoring `docs/plans/sip-automation-refactor-triggers/` in
  `livekit-agents` repo; required ~10 Edit iterations before `plan-tooling validate` accepted the
  plan + sibling review-source doc.
- Tool version: `nils-cli` 0.8.9 (`plan-tooling` 0.8.9), Homebrew-installed binary at
  `/opt/homebrew/Cellar/nils-cli/0.8.9/bin/plan-tooling`.
- Crate under review: `crates/plan-tooling/` (primarily `src/validate.rs` and `src/bundle.rs`).

## Execution

- Recommended plan:
  docs/plans/plan-tooling-validate-ergonomics/plan-tooling-validate-ergonomics-plan.md
- Recommended execution state:
  docs/plans/plan-tooling-validate-ergonomics/plan-tooling-validate-ergonomics-execution-state.md

## Purpose

Capture concrete UX deficiencies in `plan-tooling validate` (and the adjacent `--explain` / `--fix`
gaps) so a follow-up `create-plan` run can implement the fixes against the `plan-tooling` crate.
Reader audience: the implementer who picks up the next plan. The goal is to make plan authoring less
iterative for both humans and LLM agents.

## Current Judgment

The CLI works correctly end-to-end (validation passes a well-formed plan) but is unfriendly to
first-time authors and to LLMs that don't have access to the source code. Most friction comes from
three patterns:

1. **`--explain` only covers the catalogued error classes**, and the source-doc bundle errors
   emitted by `bundle.rs` are not in the catalog. The flag silently no-ops for those errors instead
   of saying "no canonical example registered".
2. **Format rigidity**: source-doc labels accept exactly one markdown shape (list item, no bold, no
   backticks). Dependency entries reject any trailing free-form note even when the `Task N.M` ID is
   unambiguous. Location entries reject directory paths even when the natural anchor for a task is a
   directory tree.
3. **Diagnostic shape**: errors are printed one-per-occurrence with no class-level grouping in text
   mode; humans get a 14-line wall of repetition for what is structurally 5 root causes.

None of these are blockers — the CLI is usable. They are UX papercuts that compound when an LLM is
iterating quickly.

## Findings

| #   | Priority | Issue                                                                                                   | Evidence                                                                                                                                                                                                                                                                                                                                                                   | Fix location                                                                                                                                                                                                                                                 | Acceptance                                                                                                                                                                                                                                                                                                             |
| --- | -------- | ------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| F1  | High     | `--explain` silently no-ops on bundle-source-doc error classes                                          | `bundle.rs:65` emits `bundle Primary source must be an accepted sibling source doc...` and the source-doc-missing-label errors (lines ~110–140 area); none of these patterns are registered in `EXPLAIN_CATALOG` at `validate.rs:~688`. Help text advertises "Output is independent of exit code (also prints on success)" but produces zero extra output on these errors. | `crates/plan-tooling/src/validate.rs` (extend `EXPLAIN_CATALOG`); `crates/plan-tooling/src/bundle.rs` (emit pattern strings that match new catalog entries).                                                                                                 | Running `plan-tooling validate --explain` on a plan that triggers either bundle error class prints a canonical example. If an error class is intentionally uncatalogued, `--explain` prints `note: no canonical example registered for error class X` instead of silent no-op.                                         |
| F2  | High     | Source-doc `Recommended plan` / `Recommended execution state` labels accept only one undocumented shape | Tried 4 markdown variants over consecutive runs; only ` - Recommended plan: docs/...` (list marker, no bold, no backticks) was accepted. Baseline policy doc (`PLAN_AUTHORING_BASELINE.md` in claude-kit) does not document the required shape. Label constants live at `bundle.rs:6-7`.                                                                                   | `crates/plan-tooling/src/bundle.rs` `read_source_doc_links()` (the function consuming `RECOMMENDED_*_LABEL`). Strip `**bold**`, `` `code` ``, and `[text](link)` wrappers from the value before matching the path. Also normalize bullet/no-bullet variants. | The four variants in finding §1 below all parse to the same accepted path and validation passes. Add unit tests covering each variant in `validate.rs` test module.                                                                                                                                                    |
| F3  | High     | Dependency entries reject free-form annotation                                                          | `validate.rs:530` emits `invalid dependency (expected 'Task N.M', e.g. 'Task 1.2')` for inputs like `- 1.1 (only run when Task 1.1 fires)`. Authors lose the ability to record WHY a dependency exists inline.                                                                                                                                                             | `crates/plan-tooling/src/validate.rs` dependency parser (near line 510-530). Change regex from strict match to anchor-only: `^\s*Task\s+(\d+\.\d+)\b(.*)$`, capture the trailing note, attach as metadata or just discard for matching purposes.             | A dep line like `- Task 1.1 (only when X flagged)` is accepted. The bare form `- Task 1.1` continues to work. Test coverage in `validate.rs` tests.                                                                                                                                                                    |
| F4  | Medium   | `Location` field rejects directory paths                                                                | `validate.rs:477` emits `Location must be a file path (not a directory): ...`. But evidence-diff or per-directory tasks legitimately anchor on a directory tree (e.g. `sip_automation/results/rounds/` in the round-baseline workflow). Workaround forces authors to substitute a less-accurate single-file proxy.                                                         | `crates/plan-tooling/src/validate.rs` Location parser. Two options: (a) allow trailing `/` to mark a directory and accept if the path exists as a dir, (b) add `dir:` / `glob:` prefix support. Option (a) is the smaller change.                            | A `Location: - sip_automation/results/rounds/` entry validates when the directory exists. Failure mode (path missing) yields a clear "directory not found" diagnostic, not the current "must be a file" message.                                                                                                       |
| F5  | Medium   | Errors not deduplicated by class in text output                                                         | First validation produced 14 error lines from 5 root causes: `Task Location is directory ×3`, `Task Dependency format wrong ×7`, `Source path uses markdown link ×1`, `Source doc missing label ×2`, `Task description placeholder ×1`. Readers must visually group repeated patterns. `--format json` already gives structured output; text mode is what most humans run. | `crates/plan-tooling/src/validate.rs` text formatter (search for where errors are flushed in plain-text mode).                                                                                                                                               | When more than 2 errors share a class, text output prints a class header (`Task Dependency format wrong (×7)`) followed by per-occurrence locations. JSON mode is unchanged. Add a `--no-group` flag for the old behavior.                                                                                             |
| F6  | Medium   | No spec/schema dump command                                                                             | To discover validation rules I inspected the compiled binary with `strings $(which plan-tooling)` and grep. No `plan-tooling spec`, no embedded JSON schema, no human-readable catalog dump.                                                                                                                                                                               | New subcommand `plan-tooling spec` in `crates/plan-tooling/src/main.rs` plus a new `src/spec.rs` module that introspects `EXPLAIN_CATALOG` plus other validation rule constants.                                                                             | `plan-tooling spec --format json` dumps all error classes, patterns, rules, and examples in JSON. `plan-tooling spec --format text` dumps a readable table. Becomes the single source of truth for humans, LLM agents, and downstream doc generators (e.g. `PLAN_AUTHORING_BASELINE.md` can be generated from `spec`). |
| F7  | Low      | No `--fix` mode for mechanical violations                                                               | In this session ~80% of Edit operations were pure mechanical translations: `1.1, 1.2` → `- Task 1.1\n  - Task 1.2`; `[path](link)` → bare path; `` `path` `` → bare path. These transformations are unambiguous.                                                                                                                                                           | `crates/plan-tooling/src/validate.rs` plus new `src/fix.rs` module. Re-use `EXPLAIN_CATALOG` to attach per-pattern rewriters (`fn fix(&self, raw: &str) -> Option<String>`).                                                                                 | `plan-tooling validate --fix` rewrites mechanical violations in-place, leaves ambiguous ones (e.g. directory→file choice in F4) as remaining errors. Documented in `--help`. Test coverage exercising each rewriter.                                                                                                   |
| F8  | Low      | No watch mode                                                                                           | Iterative authoring requires manual re-run after each Edit.                                                                                                                                                                                                                                                                                                                | New `--watch` flag using `notify` crate; re-run validation when the watched plan files change.                                                                                                                                                               | `plan-tooling validate --watch <plan>` keeps a process alive and re-validates on file change. Optional; defer if `--fix` (F7) reduces iteration count enough.                                                                                                                                                          |

## Concrete shape comparisons (Finding F2 evidence)

| Markdown input                                   | Validator verdict                   | Canonical equivalent           |
| ------------------------------------------------ | ----------------------------------- | ------------------------------ |
| `Recommended plan: [path](path)`                 | REJECTED — bundle Primary source... | `- Recommended plan: path`     |
| `Recommended plan: docs/...` (no list marker)    | REJECTED — source doc missing       | `- Recommended plan: docs/...` |
| `**Recommended plan**: ` + backtick-wrapped path | REJECTED — source doc missing       | `- Recommended plan: path`     |
| `- Recommended plan: docs/...`                   | ACCEPTED                            | (same)                         |

The accepted shape is **a markdown list item, no bold formatting, no inline code formatting, value
as bare path**. None of these constraints are documented in `PLAN_AUTHORING_BASELINE.md` or in
`--help` / `--explain`.

## Ownership Boundary

- All fix surfaces are inside `crates/plan-tooling/`. No cross-crate refactor needed.
- `PLAN_AUTHORING_BASELINE.md` lives in claude-kit (separate repo). A documentation-side companion
  change can land later; not blocking this work.
- The `--explain` flag and its catalog are the LLM-facing contract; treat additions to the catalog
  as a public surface and version-bump the crate accordingly.

## Backlog (concrete next fixes)

In ROI order requested by the reviewer:

1. **F1** Fix `--explain`: extend `EXPLAIN_CATALOG` to cover bundle-source-doc errors; for any
   uncatalogued error, print a `note: no canonical example registered for error class X`. (highest
   ROI; smallest change)
2. **F2** Normalize markdown rendering before label matching in
   `bundle.rs::read_source_doc_links()`. (clear scope, single function)
3. **F7** `validate --fix` mode for mechanical rewrites. (depends on catalog extensions from F1 to
   be ergonomic)

Then, in any order:

4. **F3** Loosen dependency parser to accept trailing notes.
5. **F4** Allow directory Locations (option a: trailing `/` + filesystem check).
6. **F5** Class-grouped text output with `--no-group` escape hatch.
7. **F6** `plan-tooling spec` subcommand (largest scope; consider after F1–F5 settle).
8. **F8** Watch mode (lowest priority; F7 may reduce the need).

## Retention Intent

- This document is a **plan-source artifact** with execution coordination value.
- Treat as cleanup-eligible after the corresponding plan is fully executed (i.e. all 8 findings have
  either landed or been explicitly dropped from scope).
- Do not promote into `docs/runbooks/` or `docs/specs/` — the durable surface is the user-facing
  `--help` and `plan-tooling spec` output (F6).

## Validation Gate

For each finding's fix PR:

- `bash scripts/ci/nils-cli-checks-entrypoint.sh` (default local checks)
- Targeted: `cargo test -p plan-tooling` with new test cases covering the accepted-shape variants
  and the `--explain` no-op-then-note path.
- For F6 (new subcommand) and F7 (new flag): regenerate completions and run
  `zsh -n completions/zsh/_plan-tooling` and `bash -n completions/bash/plan-tooling` per `AGENTS.md`
  policy.
- For F1: add a test that ensures every emitted error pattern has either a matching
  `EXPLAIN_CATALOG` entry or is explicitly opted out (e.g. via a `KNOWN_UNCATALOGUED` list).

## Do-Not-Do / Guardrails

- Do not break `--format json` output shape; that is a stable contract for tooling.
- Do not change exit codes (0 / 1 / 2 currently). The script ecosystem assumes them.
- Do not change the canonical accepted shape silently in `bundle.rs` — extend it to accept more
  shapes; preserve the old one.
- Do not auto-fix anything ambiguous in F7. Mechanical means: (a) idempotent, (b) no semantic loss,
  (c) reproducible. Anything else stays as an error.
- Do not add a `--strict` flag to opt out of any of these improvements unless requested. The default
  should improve; opting out is the deviation.

## What Worked Well (out of scope to change)

- `scaffold` is clean and one-shot — no friction.
- `batches` / `split-prs` output well-shaped JSON with correctly-computed `blocked_by_external`
  (verified by Sprint 5 of the calling plan, which depends on `Task 2.3` or `Task 3.3`).
- Validation exit codes (0 / 1 / 2) are scriptable and well-documented in `--help`.
- Pre-commit hook integration via `semantic-commit` was friction-free in the calling repo.

## Open Questions

1. Should F6 (`plan-tooling spec`) ship as JSON schema or as a flat error-class catalog? JSON schema
   is more powerful but adds complexity; flat catalog matches `EXPLAIN_CATALOG` directly.
   Recommendation: flat catalog first, JSON schema follow-up if downstream consumers ask.
2. For F3 (free-form dependency notes), should the captured trailing note appear in `to-json` output
   as a `notes` field? If yes, it becomes a stable contract; if no, it's lossy parsing.
   Recommendation: surface as `notes` in `to-json` so the information round-trips.
3. For F4 (directory Locations), are there downstream consumers (e.g. `split-prs`) that assume
   `Location` is always a file path? Audit before relaxing.

## Source Material

- `crates/plan-tooling/src/bundle.rs:6-7` — `RECOMMENDED_*_LABEL` constants.
- `crates/plan-tooling/src/bundle.rs:65` —
  `bundle Primary source must be an accepted sibling source doc...` error.
- `crates/plan-tooling/src/validate.rs:80` — `--explain` argument parsing.
- `crates/plan-tooling/src/validate.rs:477` — `Location must be a file path (not a directory): ...`.
- `crates/plan-tooling/src/validate.rs:530` —
  `invalid dependency (expected 'Task N.M', e.g. 'Task 1.2'): ...`.
- `crates/plan-tooling/src/validate.rs:~688` — `EXPLAIN_CATALOG` static array of
  `ExplainCatalogEntry { pattern, explain }`.
- Hands-on calling repo: `livekit-agents` plan `docs/plans/sip-automation-refactor-triggers/` (10
  Edit iterations to satisfy validator).
