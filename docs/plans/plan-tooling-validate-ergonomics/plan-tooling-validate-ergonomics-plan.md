# Plan: plan-tooling validate Ergonomics

## Overview

Implement 8 UX improvements (F1–F8) against the `plan-tooling` crate's validate / explain / fix
surfaces. The plan is split into three sprints in ROI order: Sprint 1 ships the high-leverage fixes
(`--explain` coverage + source-doc label normalization); Sprint 2 ships format relaxations
(`Dependencies` notes, directory `Location`s, grouped diagnostics); Sprint 3 ships new surfaces
(`spec` subcommand, `validate --fix`, optional `--watch`). Each finding lands as one PR via `group`
grouping intent. Within-sprint tasks may run in parallel where they touch independent files.

## Read First

- Primary source:
  docs/plans/plan-tooling-validate-ergonomics/plan-tooling-validate-ergonomics-review-source.md
- Source type: discussion-to-implementation-doc
- Open questions carried into execution:
  - Should `plan-tooling spec` (F6) ship as JSON schema or flat catalog? Default to flat catalog
    this round; schema is follow-up.
  - Should free-form dep notes (F3) round-trip in `to-json` output? Default to yes; surface as
    `notes` field.
  - Are there other downstream consumers of `Location` that assume "file only" beyond `split-prs`?
    Audit subtask inside Task 2.2 must answer.

## Scope

- In scope:
  - `crates/plan-tooling/src/validate.rs` — extend `EXPLAIN_CATALOG`, loosen `Dependencies` parser,
    allow directory `Location` paths, add class-grouped text output.
  - `crates/plan-tooling/src/bundle.rs` — extend source-doc label parser to accept the four
    documented markdown variants.
  - `crates/plan-tooling/src/main.rs` — register new `spec` subcommand; new `--fix` and `--watch`
    flags on `validate`.
  - New modules: `crates/plan-tooling/src/spec.rs`, `crates/plan-tooling/src/fix.rs`.
  - Completions: `completions/zsh/_plan-tooling`, `completions/bash/plan-tooling` (Sprint 3).
  - Tests for every above behavior change, including a property-style fix-then-validate fixed-point
    test.
- Out of scope:
  - Changing `--format json` output shape (stable contract).
  - Changing exit codes (`0` ok / `1` errors / `2` usage).
  - Updates to claude-kit's `PLAN_AUTHORING_BASELINE.md`. Document-side companion change is tracked
    separately.
  - Other `nils-cli` crates.
  - Replacing the current accepted source-doc label shape (must remain accepted alongside the new
    variants).

## Assumptions

1. Rust toolchain and `cargo` (+`cargo-nextest`) are available per `DEVELOPMENT.md`.
2. `plan-tooling` 0.8.9 is the pre-change baseline. These changes will cut a new minor (`0.9.0`)
   given the public-surface extensions to `--explain` / `--help`.
3. `notify` crate (for F8) is or will be available in the workspace; if not, F8 may be deferred
   without blocking F1–F7.
4. `pretty_assertions` is the test-assertion convention per `AGENTS.md` and already in
   dev-dependencies.
5. Existing test plan fixtures live alongside `validate.rs` test module; new tests extend that
   module.

## Sprint 1: --explain coverage + source-doc label normalization

**Goal**: Close the silent-no-op gap on `--explain` and make source-doc labels accept the four
common markdown variants. Highest ROI; both tasks touch independent files and can land in parallel
as two PRs.

**Demo/Validation**:

- Commands:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh`
  - `cargo test -p plan-tooling`
  - Manual: `plan-tooling validate --explain --file <plan-with-bundle-errors>` prints canonical
    examples or explicit "no example registered" notes.
  - Manual: each of the four shape variants from review source §"Concrete shape comparisons" parses
    to the same accepted source-doc path.
- Verify: All four shape variants pass validation; `--explain` never silently no-ops on any emitted
  error class.

**PR grouping intent**: `group`
**Execution Profile**: `parallel-x2`

### Task 1.1: F1 — Extend EXPLAIN_CATALOG to cover bundle source-doc errors

- **Location**:
  - `crates/plan-tooling/src/validate.rs`
  - `crates/plan-tooling/src/bundle.rs`
- **Description**: Add `EXPLAIN_CATALOG` entries (in `validate.rs` near the existing catalog around
  line 688) for the two bundle-emitted error patterns currently uncovered:
  `bundle Primary source must be an accepted sibling source doc` (from `bundle.rs:65`) and
  `source doc missing 'Recommended plan'` / `source doc missing 'Recommended execution state'` (from
  `bundle.rs::validate_source_links`). Each new entry must have a concrete `example` showing the
  canonical accepted shape. Add a `KNOWN_UNCATALOGUED` allowlist (or equivalent) so a new test can
  assert every emitted error pattern is either in `EXPLAIN_CATALOG` or explicitly opted out. When
  `--explain` is invoked and the emitted error has no catalog entry and is not in the allowlist,
  print `note: no canonical example registered for error class X` instead of silent no-op.
- **Dependencies**:
  - none
- **Complexity**: 4
- **Acceptance criteria**:
  - `plan-tooling validate --explain` on a plan that triggers bundle Primary source mismatch prints
    the canonical example.
  - `plan-tooling validate --explain` on a plan that triggers source-doc missing label prints the
    canonical example.
  - A new test asserts every error pattern produced by any test fixture is bound either to an
    `EXPLAIN_CATALOG` entry or an explicit opt-out.
  - Help text for `--explain` remains accurate.
- **Validation**:
  - `cargo test -p plan-tooling` passes.
  - Manual: trigger each new error class against a crafted plan; verify example appears.

### Task 1.2: F2 — Normalize markdown wrappers when parsing source-doc labels

- **Location**:
  - `crates/plan-tooling/src/bundle.rs`
- **Description**: Update `read_source_doc_links()` (the function consuming `RECOMMENDED_PLAN_LABEL`
  and `RECOMMENDED_EXECUTION_STATE_LABEL` constants at `bundle.rs:6-7`) to normalize a label line
  before matching. Steps: strip leading list markers (`-`, `*`, `+`), strip `**bold**` wrappers
  around the label, strip backticks and `[text](link)` wrappers from the value. The
  currently-accepted shape (`- Recommended plan: <bare path>`) must continue to parse. New unit
  tests must cover each of the four variants listed in the review source.
- **Dependencies**:
  - none
- **Complexity**: 3
- **Acceptance criteria**:
  - All four variants in the review source "Concrete shape comparisons" parse to the same accepted
    path.
  - Existing currently-accepted shape still parses (regression test).
  - Unit tests with `pretty_assertions` cover each variant including malformed shapes that should
    still be rejected (e.g. label without colon).
- **Validation**:
  - `cargo test -p plan-tooling` passes.
  - Manual: paste a `**Recommended plan**: \`path\`` variant into a calling repo's review-source
    doc; validation passes.

## Sprint 2: format relaxations (Dependencies / Location / grouped output)

**Goal**: Stop rejecting valid author intent: Dependencies trailing notes (F3), directory `Location`
paths (F4), and the unreadable text-output wall (F5). All three touch `validate.rs`, but different
parsers/formatters — split into three PRs. F4 includes an audit subtask for downstream consumers.

**Demo/Validation**:

- Commands:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh`
  - `cargo test -p plan-tooling`
  - Smoke: re-run validation against `livekit-agents` repo's
    `docs/plans/sip-automation-refactor-triggers/sip-automation-refactor-triggers-plan.md` with
    intentional dep notes + directory `Location`; should pass.
- Verify: Author-friendly forms accepted; legacy strict forms still pass; downstream consumers
  unaffected.

**PR grouping intent**: `group`
**Execution Profile**: `parallel-x3`

### Task 2.1: F3 — Allow free-form annotation after `Task N.M` in Dependencies

- **Location**:
  - `crates/plan-tooling/src/validate.rs`
  - `crates/plan-tooling/src/parse.rs`
  - `crates/plan-tooling/src/parse/to_json.rs`
- **Description**: Change the dependency parser (near `validate.rs:530`) from strict-match to
  anchor-only regex: `^\s*-?\s*Task\s+(\d+\.\d+)\b(.*)$`. Capture the trailing free-form note;
  surface it as a `notes` field per-dependency in `to-json` output to round-trip. Keep the bare
  `- Task 1.1` form working. Update `EXPLAIN_CATALOG` example for `invalid dependency` to mention
  the allowed annotation form.
- **Dependencies**:
  - none
- **Complexity**: 4
- **Acceptance criteria**:
  - `- Task 1.1 (only when X flagged)` validates; the trailing note is captured.
  - `- Task 1.1` still validates with empty note.
  - Malformed forms (e.g. `- 1.1`, `- Task X.Y`) remain rejected with the same error class.
  - `plan-tooling to-json --file <plan>` emits a `notes` field per dependency (empty string when
    bare).
  - Unit tests cover bare / annotated / invalid / malformed forms.
- **Validation**:
  - `cargo test -p plan-tooling` passes.
  - Manual: re-run the calling repo's `sip-automation-refactor-triggers-plan.md` with annotated deps
    reverted to original; should pass.

### Task 2.2: F4 — Allow directory `Location` paths with downstream audit

- **Location**:
  - `crates/plan-tooling/src/validate.rs`
  - `crates/plan-tooling/src/split_prs.rs`
  - `crates/plan-tooling/src/batches.rs`
  - `crates/plan-tooling/src/artifact_audit.rs`
- **Description**: Two sub-steps.
  - Sub-step A (audit, no behavior change): grep all crates for consumers that expect `Location` to
    be a file (especially `split_prs.rs`, `batches.rs`, `artifact_audit.rs`); document findings in
    the execution-state doc. If any consumer assumes file-only and would break with a directory
    path, plan the fix as part of this task.
  - Sub-step B (behavior change): update `Location` parser (near `validate.rs:477`). When the path
    ends with `/`, accept if `path.is_dir()`. Emit `Location directory not found` diagnostic when
    missing (a new error class — add `EXPLAIN_CATALOG` entry). For paths without trailing `/`,
    preserve the existing file-only check. Update existing `Location must be a file path` catalog
    entry to clarify the directory escape hatch.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 6
- **Acceptance criteria**:
  - Audit document section appended to the execution-state doc; verdict: file-only consumers
    documented and either fixed in this task or explicitly out of scope.
  - `Location: - sip_automation/results/rounds/` validates when directory exists.
  - `Location: - sip_automation/results/missing-dir/` emits the new "directory not found"
    diagnostic.
  - File-only path `Location: - some/file.rs` continues to validate via the file branch.
  - Unit tests cover dir-exists, dir-missing, file-exists, file-missing branches.
- **Validation**:
  - `cargo test -p plan-tooling` passes.
  - Manual: re-run `sip-automation-refactor-triggers-plan.md` with `sip_automation/results/rounds/`
    substituted into Task 1.2 Location; should pass.

### Task 2.3: F5 — Class-grouped text output with `--no-group` escape hatch

- **Location**:
  - `crates/plan-tooling/src/validate.rs`
- **Description**: When the text formatter emits more than two errors sharing the same `class`
  (resolved via `EXPLAIN_CATALOG.pattern` match), group them under a class header
  (`Task Dependency format wrong (x7)`) followed by per-occurrence locations (file + task + line).
  Single-occurrence classes print unchanged. JSON format must be byte-identical. Add `--no-group`
  flag (default false) that restores per-occurrence flat output.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 5
- **Acceptance criteria**:
  - 14-error wall from the calling repo first validation run renders as 5 grouped headers with
    counts.
  - `--format json` output is byte-identical to before this change (golden test).
  - `--no-group` produces the legacy flat output.
  - Unit tests cover: single-occurrence (no grouping), exactly-2 occurrences (no grouping per "more
    than two" rule), 3+ occurrences (grouped), mixed classes.
- **Validation**:
  - `cargo test -p plan-tooling` passes.
  - JSON golden test confirms output stability.

## Sprint 3: new surfaces (spec subcommand, --fix, --watch)

**Goal**: Ship the three new-surface features. Each is a separable PR. F7 is the most invasive (file
rewriting) so it lands after F6 (which formalizes the catalog as a public artifact). F8 is optional
and may be deferred if F7 sufficiently reduces iteration cost.

**Demo/Validation**:

- Commands:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh`
  - `cargo test -p plan-tooling`
  - `zsh -n completions/zsh/_plan-tooling`
  - `bash -n completions/bash/plan-tooling`
  - Manual: `plan-tooling spec --format json | jq .`, `plan-tooling spec --format text`,
    `plan-tooling validate --fix --file <broken-plan>`,
    `plan-tooling validate --watch --file <plan>` (control-C to stop).
- Verify: `spec` emits a stable JSON shape; `--fix` rewrites mechanical violations and is a fixed
  point; `--watch` re-validates on save.

**PR grouping intent**: `group`
**Execution Profile**: `parallel-x2`

### Task 3.1: F6 — `plan-tooling spec` subcommand

- **Location**:
  - `crates/plan-tooling/src/main.rs`
  - `crates/plan-tooling/src/spec.rs`
  - `crates/plan-tooling/src/lib.rs`
  - `completions/zsh/_plan-tooling`
  - `completions/bash/plan-tooling`
- **Description**: Add a new `spec` subcommand that introspects `EXPLAIN_CATALOG` (and any other
  validation-rule constants worth exposing). Two output formats: `--format json` emits an array of
  objects `{class, pattern, rule, example}` with stable field order; `--format text` emits a
  human-readable table. Wire `#[command(version)]` on the parser per `AGENTS.md`. `--help` must show
  `-V, --version`. Update zsh and bash completions to advertise the new subcommand and its
  `--format` flag.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 6
- **Acceptance criteria**:
  - `plan-tooling spec --format json` produces JSON parseable by `jq`; field order stable across
    runs.
  - `plan-tooling spec --format text` produces readable output.
  - `plan-tooling spec -V` / `plan-tooling spec --version` works.
  - `plan-tooling spec --help` lists `-V, --version`.
  - Completions exit clean under `zsh -n` and `bash -n`.
  - Golden test for the JSON shape (stable across reordering of catalog entries by sorting on
    `class`).
- **Validation**:
  - `cargo test -p plan-tooling` passes.
  - Completions syntax-check passes.
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh` passes.

### Task 3.2: F7 — `plan-tooling validate --fix` mechanical-rewrite mode

- **Location**:
  - `crates/plan-tooling/src/validate.rs`
  - `crates/plan-tooling/src/fix.rs`
  - `crates/plan-tooling/src/lib.rs`
  - `completions/zsh/_plan-tooling`
  - `completions/bash/plan-tooling`
- **Description**: Add `--fix` flag to `validate`. Each `EXPLAIN_CATALOG` entry may optionally bind
  a `fixer: fn(raw: &str) -> Option<String>` (or define a parallel `FIX_CATALOG` keyed by class).
  Wire fixers for at minimum these mechanical violations: dependency form `1.1` to `Task 1.1`;
  dependency comma-list `1.1, 1.2` to multi-line bulleted form; bare path inside markdown link
  `[label](path)` to bare `path` for Primary source / source-doc labels; backtick-wrapped path to
  bare path. Skip ambiguous violations (e.g. `Location must be a file` cannot be fixed without
  choosing a target). On `--fix` runs, apply fixers in-place, then re-validate; report the
  still-remaining errors. Add a property-style test asserting `validate(fix(plan))` is a subset of
  `validate(plan)` AND `fix(fix(plan)) == fix(plan)` (fixed point). Update completions.
- **Dependencies**:
  - Task 1.1
  - Task 2.1
  - Task 2.2
  - Task 2.3
- **Complexity**: 8
- **Acceptance criteria**:
  - `plan-tooling validate --fix --file <plan>` rewrites all mechanical violations from the calling
    repo first-run errors (the 10-Edit sequence the human had to do manually).
  - Property test: for every test-fixture plan, `fix(fix(p)) == fix(p)`.
  - Property test: `validate(fix(p))` produces a subset of `validate(p)` errors (never introduces
    new ones).
  - Ambiguous violations (file path missing, directory-to-file decision) remain as errors after
    `--fix`.
  - `--fix` is documented in `--help`.
  - Completions advertise `--fix`.
- **Validation**:
  - `cargo test -p plan-tooling` passes (including new property tests).
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh` passes.
  - Completions syntax-check passes.

### Task 3.3: F8 — `plan-tooling validate --watch` mode (optional)

- **Location**:
  - `crates/plan-tooling/src/validate.rs`
  - `crates/plan-tooling/Cargo.toml`
  - `completions/zsh/_plan-tooling`
  - `completions/bash/plan-tooling`
- **Description**: Add `--watch` flag using `notify` crate to re-run validation on file change of
  the supplied `--file` paths. Decision gate at the start of the task: re-evaluate whether `--fix`
  (Task 3.2) has reduced iteration cost enough to defer F8 to a follow-up. If proceeding, integrate
  `notify` (add to workspace dependencies if absent), implement watch loop, print a divider line on
  each re-validation, exit cleanly on SIGINT/SIGTERM.
- **Dependencies**:
  - Task 1.1
  - Task 3.2
- **Complexity**: 4
- **Acceptance criteria**:
  - Decision recorded: proceed or defer. If defer, the open question is documented in the
    execution-state doc.
  - If proceeding: `plan-tooling validate --watch --file <plan>` re-validates on save; control-C
    exits cleanly.
  - Completions advertise `--watch`.
- **Validation**:
  - `cargo test -p plan-tooling` passes.
  - Completions syntax-check passes.
  - Manual: edit a watched plan; re-validation fires within ~1 second.

## Testing Strategy

- Unit: `cargo test -p plan-tooling` per sprint, using `pretty_assertions::{assert_eq, assert_ne}`
  per `AGENTS.md`. Each task adds tests for its new behavior and at least one regression test for
  the existing accepted shape.
- Integration: `bash scripts/ci/nils-cli-checks-entrypoint.sh` per sprint.
- Property: Task 3.2 (F7) adds property-style tests for the fixed-point invariant and the
  no-new-errors invariant. Use `proptest` or fixture-based property tests, depending on what the
  crate already uses.
- Completion linting: Tasks 3.1, 3.2, 3.3 each verify `zsh -n completions/zsh/_plan-tooling` and
  `bash -n completions/bash/plan-tooling` succeed.
- Smoke: After each sprint lands on `main` (or a release branch), re-run validation against an
  external calling repo plan — concretely the `livekit-agents` repo
  `docs/plans/sip-automation-refactor-triggers/sip-automation-refactor-triggers-plan.md` — to
  confirm the friction it originally exposed is gone.

## Risks & gotchas

- EXPLAIN_CATALOG is a public-surface contract: LLM agents and downstream tooling read it via
  `--explain`. Any additions in Task 1.1 are additive and safe, but the wider work (especially Task
  3.1 `spec` subcommand) formalizes this surface. Cut a new minor version (`0.9.0`) at release time
  per `nils-cli` versioning policy.
- F2 markdown normalization regression risk: Task 1.2 must keep the currently-accepted
  `- Recommended plan: <bare-path>` shape working. Add an explicit regression test, not just
  new-variant tests.
- F7 `--fix` is the most likely file-munger: Task 3.2 must guard against rewriting authors' work
  incorrectly. The property test `fix(fix(p)) == fix(p)` is the load-bearing invariant — if it fails
  for any fixture, the fixer is broken. Also: `--fix` must never produce a malformed Markdown
  structure (test that round-tripping through a Markdown parser preserves shape).
- F4 directory `Location`s may break downstream consumers: Task 2.2 includes a mandatory audit
  sub-step. Don't skip it. Downstream consumers of `Location` to check:
  `crates/plan-tooling/src/split_prs.rs`, `crates/plan-tooling/src/batches.rs`,
  `crates/plan-tooling/src/artifact_audit.rs`, plus any external tool that consumes `to-json` output
  `location` field.
- JSON output stability: Several tasks (F5 text grouping, F1 catalog extensions, F6 new subcommand)
  must not change the JSON shape of existing commands. Each affected task includes a JSON golden
  test.
- Exit code stability: Existing `0` / `1` / `2` semantics must not change. F7 (`--fix`) must decide
  its own exit-code policy carefully: suggested — exit `0` if all errors fixed, `1` if errors remain
  (same as plain validate), never a new code. Document the decision in the task PR description.
- `--watch` (F8) crate addition risk: `notify` is a substantial dependency. If the workspace doesn't
  already use it, adding it bumps build time and binary size. Task 3.3 decision gate must weigh
  this.
- Pre-commit hook impact: `nils-cli` pre-commit setup (per `DEVELOPMENT.md`) runs the default
  checks. Verify each PR passes locally before pushing — pre-commit gates will block otherwise.
- AGENTS.md compliance for new subcommand: `plan-tooling spec` must set `#[command(version)]` on the
  root `Parser` and surface `-V, --version` in `--help`. This is non-negotiable per `AGENTS.md` Repo
  Conventions section.

## Rollback plan

- Each finding lands as one PR; revert individually via `git revert <sha>`.
- F1, F3, F5, F6: additive changes; revert restores prior behavior with no plan-file changes
  required.
- F2: revert restores strict source-doc label parsing; any newly-written review-source docs using a
  normalized variant (bold / backticks / markdown link / no-bullet) will then fail validation until
  updated to the legacy shape.
- F4: revert restores file-only `Location`s; any plan that adopted directory `Location`s after F4
  lands will need a hand-edit to the legacy substitution pattern (e.g. `index.md` + `run_suite.py`
  proxy files).
- F7: revert removes the `--fix` flag; no plan-file changes persist outside `--fix` invocations.
- F8: revert removes the `--watch` flag; no plan-file changes persist outside `--watch` invocations.
- If a property test in F7 fails post-merge, the rollback procedure is: revert F7 immediately, file
  a bug with the failing fixture, re-attempt F7 in a follow-up PR. Do not patch-fix `--fix` under
  pressure — file rewriters are too high-risk for hot fixes.
