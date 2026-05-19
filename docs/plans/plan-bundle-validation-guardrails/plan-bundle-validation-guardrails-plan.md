# Plan: Plan Bundle Validation Guardrails

## Overview

Reduce repeated manual cleanup around plan bundle formatting by moving the
failure earlier and making the validator/fixer handle formatter-stable markdown
shapes. The first sprint adds a docs-only CI/local gate for touched
`docs/plans/**` bundles. The second sprint teaches `plan-tooling` to parse
same-line sprint metadata pairs. The third sprint extends `validate --fix` to
rewrite same-line sprint metadata into the canonical two-line shape.

## Read First

- Primary source:
  docs/plans/plan-bundle-validation-guardrails/plan-bundle-validation-guardrails-review-source.md
- Source type: review-to-improvement-doc
- Open questions carried into execution:
  - Should the plan-bundle gate scan only changed files in CI, or all
    `docs/plans/**/*-plan.md` locally? Default to changed files in CI and all
    plans when no diff base is available.
  - Should same-line sprint metadata be accepted only for the two canonical
    fields? Default to yes; do not create a general markdown field parser.

## Scope

- In scope:
  - Add a small plan-bundle validation script and wire it into docs-only checks.
  - Detect touched `docs/plans/SLUG/SLUG-plan.md` files from git state.
  - Keep manual override behavior simple for local runs without a diff base.
  - Extend `plan-tooling` sprint metadata parsing for same-line
    `PR grouping intent` plus `Execution Profile`.
  - Extend `plan-tooling validate --fix` to split same-line sprint metadata into
    canonical lines.
  - Add targeted unit/integration tests for parser and fixer behavior.
- Out of scope:
  - Relaxing the direct source-doc waiver rule.
  - Replacing markdownlint or changing formatter configuration.
  - Reworking all `docs/plans/` cleanup policy.
  - Changing `plan-tooling` JSON contracts for existing commands.
  - Adding new command-line flags unless existing surfaces are insufficient.

## Assumptions

1. `plan-tooling` is available in local and CI environments that run required
   checks.
2. Docs-only checks may invoke `plan-tooling validate` without requiring Rust
   build/test dependencies.
3. Same-line sprint metadata is mechanically recoverable only when both fields
   are canonical bold labels on one physical line.
4. `Direct source-doc execution waiver: not applicable` should continue to fail
   when `Source document` points directly at a source doc.

## Sprint 1: Plan bundle validation gate

**Goal**: Make malformed plan bundles fail during docs-only checks, before a
semantic commit or release workflow spends time on unrelated validation.

**Demo/Validation**:

- Command(s):
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
  - `bash scripts/ci/plan-bundle-validate.sh --strict`
- Verify: touched plan bundles are validated with `plan-tooling validate`, and
  docs-only checks fail with actionable output when a changed plan is invalid.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 1.1: Add plan-bundle validation script

- **Location**:
  - scripts/ci/nils-cli-checks-entrypoint.sh
  - scripts/ci/plan-bundle-validate.sh
- **Description**: Add a Bash script that discovers plan files under
  `docs/plans/**` and runs `plan-tooling validate --file` on each selected
  `*-plan.md`. The script should support a strict mode, print each command
  before running it, and produce a clear no-op message when no plan files are
  selected. Prefer changed plan files when git can identify them; fall back to
  all plan files for local worktrees without an appropriate base.
- **Dependencies**:
  - none
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - New script exits 0 when no plan files are selected.
  - New script validates a changed `docs/plans/SLUG/SLUG-plan.md`.
  - New script validates all existing plan files when requested directly.
  - Failure output includes the failing plan path and the `plan-tooling
    validate` stderr.
- **Validation**:
  - `bash scripts/ci/plan-bundle-validate.sh --strict`
  - `bash scripts/ci/plan-bundle-validate.sh --all --strict`

### Task 1.2: Wire gate into docs-only checks

- **Location**:
  - .agents/skills/nils-cli-verify-required-checks/scripts/nils-cli-verify-required-checks.sh
  - scripts/ci/nils-cli-checks-entrypoint.sh
  - DEVELOPMENT.md
- **Description**: Call the new plan-bundle validation script from the docs-only
  required-check path after markdownlint. Document that docs-only checks now
  validate touched plan bundles in addition to placement, hygiene, and
  markdownlint. Keep the full-check path covered through the same required
  script so local and CI behavior match.
- **Dependencies**:
  - Task 1.1
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - `--docs-only` fails when a changed plan bundle is invalid.
  - `--docs-only` passes for the current repository state.
  - Help text and `DEVELOPMENT.md` list the plan-bundle gate.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`

## Sprint 2: Same-line sprint metadata parsing

**Goal**: Treat formatter-produced same-line sprint metadata as valid input
while preserving the canonical two-line output shape.

**Demo/Validation**:

- Command(s):
  - `cargo test -p nils-plan-tooling`
  - `plan-tooling validate --file tests/fixtures/same-line-metadata-plan.md`
- Verify: a sprint with same-line `PR grouping intent` plus
  `Execution Profile` validates and produces the same metadata as the canonical
  two-line form.

**PR grouping intent**: `group`
**Execution Profile**: `parallel-x2`

### Task 2.1: Parse same-line metadata pairs

- **Location**:
  - crates/plan-tooling/src/parse.rs
  - crates/plan-tooling/src/validate.rs
  - crates/plan-tooling/tests/integration/validate.rs
- **Description**: Extend sprint metadata parsing so one physical line can
  contain both canonical bold fields, such as
  `**PR grouping intent**: \`group\` **Execution Profile**: \`parallel-x2\``.
  Limit this behavior to recognized metadata fields and preserve current error
  handling for malformed or misspelled labels.
- **Dependencies**:
  - none
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - Same-line metadata pair parses into both metadata fields.
  - Canonical two-line metadata remains accepted.
  - A same-line `per-sprint` plus `parallel-x2` still fails the existing
    coherence rule.
  - Misspelled metadata labels still produce the existing invalid-field
    diagnostics.
- **Validation**:
  - `cargo test -p nils-plan-tooling validate`
  - `cargo test -p nils-plan-tooling`

### Task 2.2: Preserve downstream grouping behavior

- **Location**:
  - crates/plan-tooling/src/split_prs.rs
  - crates/plan-tooling/tests/integration/split_prs.rs
  - crates/plan-tooling/tests/integration/to_json.rs
- **Description**: Verify that `to-json`, `batches`, and `split-prs --strategy
  auto` consume same-line metadata exactly like canonical metadata. Add
  integration coverage only where a downstream command can regress.
- **Dependencies**:
  - Task 2.1
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - `to-json` reports the same sprint metadata for same-line and canonical
    fixtures.
  - `split-prs --strategy auto` resolves grouping from same-line metadata.
  - No existing output schema changes.
- **Validation**:
  - `cargo test -p nils-plan-tooling --test integration`
  - `plan-tooling split-prs --file tests/fixtures/same-line-metadata-plan.md
    --scope sprint --sprint 1 --strategy auto --default-pr-grouping group
    --format json`

## Sprint 3: Metadata canonicalization in validate --fix

**Goal**: Keep parser behavior forgiving while making the repository converge
back to the canonical two-line sprint metadata shape automatically.

**Demo/Validation**:

- Command(s):
  - `cargo test -p nils-plan-tooling`
  - `plan-tooling validate --file tests/fixtures/same-line-metadata-plan.md
    --fix`
- Verify: `--fix` rewrites same-line sprint metadata into two lines, and a
  second `--fix` run is a fixed point.

**PR grouping intent**: `group`
**Execution Profile**: `parallel-x2`

### Task 3.1: Add sprint metadata fixer

- **Location**:
  - crates/plan-tooling/src/fix.rs
  - crates/plan-tooling/tests/integration/validate.rs
- **Description**: Extend `fix_text` with a bounded fixer that detects same-line
  canonical sprint metadata pairs and splits them into two physical lines at
  the same indentation. The fixer must not touch non-canonical labels,
  task-level fields, or prose that merely mentions the metadata names.
- **Dependencies**:
  - Task 2.1
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - `validate --fix` rewrites same-line metadata to canonical two-line
    metadata.
  - `fix_text(fix_text(input)) == fix_text(input)` for new metadata fixtures.
  - The fixer preserves trailing newline shape and indentation.
  - The fixer leaves malformed metadata to validation instead of guessing.
- **Validation**:
  - `cargo test -p nils-plan-tooling`
  - `plan-tooling validate --file tests/fixtures/same-line-metadata-plan.md
    --fix`

### Task 3.2: Add regression fixture for formatter round trip

- **Location**:
  - crates/plan-tooling/tests/integration/validate.rs
  - docs/plans/plan-bundle-validation-guardrails/plan-bundle-validation-guardrails-plan.md
- **Description**: Add a regression test or fixture that mirrors the observed
  formatter behavior: markdown prose wrapping combines the sprint metadata into
  one line. The test should prove both tolerant parsing and canonicalizing
  `--fix` behavior.
- **Dependencies**:
  - Task 3.1
- **Complexity**:
  - 2
- **Acceptance criteria**:
  - Test fails against the current pre-fix behavior.
  - Test passes after parser and fixer changes.
  - The fixture documents why direct source-doc waiver behavior is intentionally
    unchanged.
- **Validation**:
  - `cargo test -p nils-plan-tooling validate`

## Testing Strategy

- Unit: add focused `parse.rs` and `fix.rs` tests for same-line metadata pairs.
- Integration: add `plan-tooling validate`, `to-json`, and `split-prs`
  coverage for same-line metadata behavior.
- CI script: exercise `scripts/ci/plan-bundle-validate.sh` in no-op, changed
  plan, and all-plan modes.
- Docs-only: run `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`.
- Full pre-delivery: for non-doc Rust changes, run
  `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh
  --with-coverage`.

## Risks & gotchas

- Same-line parsing must not become a broad markdown parser. Keep it scoped to
  known sprint metadata labels.
- `extract_primary_token` currently tolerates descriptive suffixes. Same-line
  metadata parsing must split fields before token extraction so the first field
  does not swallow the second field as notes.
- The plan-bundle gate should avoid validating unrelated historical plan files
  in CI unless explicitly requested; otherwise old coordination docs can block
  unrelated docs-only changes.
- `validate --fix` writes files in place. Tests must use temp fixtures and
  verify idempotence before any workflow recommends it as a standard repair.
- Do not weaken the direct source-doc waiver rule. The safer default remains:
  execution state points at the plan unless a real direct-source execution
  reason is recorded.

## Rollback plan

- Sprint 1 rollback: remove the new script from the docs-only required-check
  path. The script can remain unused until repaired.
- Sprint 2 rollback: revert parser changes and same-line metadata tests; plans
  remain valid if they use canonical two-line metadata.
- Sprint 3 rollback: revert fixer changes only. Parser tolerance can remain if
  it is already validated and useful.
