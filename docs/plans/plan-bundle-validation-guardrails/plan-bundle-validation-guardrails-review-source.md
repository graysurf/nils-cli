# Plan Bundle Validation Guardrails Review Source

- Status: open, ready for implementation planning
- Date: 2026-05-19
- Source: local release/body repair and docs commit workflow in this repository.
- Scope: avoid repeated manual fixes for plan bundle markdown formatting issues.
- Crate under review: `crates/plan-tooling/`.

## Execution

- Recommended plan:
  docs/plans/plan-bundle-validation-guardrails/plan-bundle-validation-guardrails-plan.md
- Recommended execution state:
  docs/plans/plan-bundle-validation-guardrails/plan-bundle-validation-guardrails-execution-state.md

## Problem

During a docs commit, an extra `plan-tooling validate` pass caught two
plan-bundle issues after normal docs-only checks had already passed:

1. Markdown formatting had collapsed sprint metadata into one physical line:
   `**PR grouping intent**: group **Execution Profile**: parallel-x2`.
2. An execution-state file pointed directly at a review-source doc while using
   `Direct source-doc execution waiver: not applicable`.

The second issue was correct validator behavior and should not be relaxed. The
first issue is formatter-induced friction: valid-looking markdown became
unparseable because the sprint metadata parser expects one canonical field per
physical line.

## Current Evidence

- `scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` delegates to
  `.agents/skills/nils-cli-verify-required-checks/scripts/nils-cli-verify-required-checks.sh`.
- The docs-only required-check path currently runs docs placement, docs hygiene,
  and markdownlint audits, but it does not run `plan-tooling validate` on
  changed plan bundles.
- `crates/plan-tooling/src/parse.rs::parse_any_field_line` parses one bold
  field per line.
- `crates/plan-tooling/src/validate.rs::validate_sprint_metadata` requires both
  `PR grouping intent` and `Execution Profile` when either is present.
- `crates/plan-tooling/src/fix.rs::fix_text` already handles mechanical fixes
  for bundle pointer labels and dependencies, but not sprint metadata
  canonicalization.
- `crates/plan-tooling/src/bundle.rs::has_direct_source_doc_waiver` correctly
  rejects `not applicable`, `n/a`, and `none` when execution-state directly
  points at the source doc.

## Desired Outcome

- Docs-only checks should catch invalid touched plan bundles automatically.
- Formatter-produced same-line sprint metadata should be accepted by
  `plan-tooling validate`.
- `plan-tooling validate --fix` should rewrite same-line sprint metadata into
  the canonical two-line shape.
- The direct source-doc waiver rule should stay strict; examples and tests
  should make the intended shape obvious.

## Non-goals

- Do not make `not applicable` a valid direct source-doc waiver.
- Do not introduce a broad markdown AST parser for sprint metadata.
- Do not force unrelated historical plans to block every docs-only change.
- Do not change existing JSON output schemas.

## Acceptance Notes

The durable behavior should be: normal authors run the usual docs-only check,
invalid changed plan bundles fail early, and the common formatter-induced sprint
metadata shape is either accepted or fixed mechanically without manual editing.
