# `plan-issue record audit` Label Contract — Source

| Field | Value |
| --- | --- |
| Status | Ready for implementation |
| Date | 2026-05-26 |
| Source issue | sympoies/nils-cli#535 (`plan-issue record audit rejects expected label flags`) |
| Intended next step | Land Sprint 1 in this plan to close out #535 by documenting the contract instead of extending `record audit`. |

## Purpose

`plan-issue record audit` rejects `--label` and therefore cannot serve as a
single typed check for both lifecycle-comment coverage and expected
provider-issue labels. Issue #535 framed two contract options: extend
`record audit` to accept expected labels, or formally keep label verification
outside `record audit` and document the boundary. This plan commits to the
second option (documentation-only contract clarification) because it matches
current call-site usage in the repository and preserves the narrow scope of
the audit command.

## Confirmed facts

- `nils-plan-issue-cli` 0.23.0's `RecordAuditArgs` exposes only `--body-file`,
  `--comments-json`, `--profile`, and `--expect-visible`; the help output adds
  the global `--repo`, `--dry-run`, `--force`, `--format`, and `--state-dir`
  flags. `--label` is not accepted.
  (`crates/plan-issue-cli/src/commands/record.rs:122-143`,
  `plan-issue record audit --help`.) [F1]
- `run_record_audit` reads body and comments JSON, then calls
  `lifecycle_record::audit_record(body, comments, profile)`. The audit core
  parses lifecycle markers and structured payloads from comments only; it
  has no access to provider-issue labels.
  (`crates/plan-issue-cli/src/execute.rs:1567-1587`,
  `crates/plan-issue-cli/src/lifecycle_record.rs:396`.) [F2]
- The v2 record contract spec describes `plan-issue record audit` in terms of
  marker URL, timestamp, profile, role, status, and parsed payload per role.
  It does not mention labels.
  (`crates/plan-issue-cli/docs/specs/issue-backed-plan-record-contract-v2.md:257-268`.)
  [F3]
- `RecordOpenArgs` accepts repeatable `--label`; `record post` and
  `record close` accept `--add-label` / `--remove-label`. Label mutation is
  consistently a write-path concern in plan-issue-cli, not a read-path
  concern.
  (`crates/plan-issue-cli/src/commands/record.rs:179-188`, `:266-273`,
  `:341-346`.) [F4]
- Repository sweep finds no remaining `record audit ... --label` invocations
  in skills, scripts, runbooks, or fixtures. The downstream workflow friction
  described in #535 has already been removed from active callers; only the
  contract itself is undocumented.
  (`grep -RIn "record audit.*--label" --include='*.md' --include='*.rs'
  --include='*.sh'` returns no matches.) [F5]
- `--expect-visible` (added in 0.22.4) extends `record audit` with the
  visible-completeness lint, broadening the command beyond raw marker
  parsing. That extension already moved the line on "what does audit cover";
  this plan deliberately does not move it further to also cover label
  validation. [F6]

## Decisions

1. **Keep labels out of `plan-issue record audit`.** The audit command stays
   bounded to lifecycle markers, payloads, and the visible-completeness lint.
   Label verification is a separate provider-state concern.
2. **Document the boundary in v2 spec.** Add explicit language to the
   `plan-issue record audit` section of
   `issue-backed-plan-record-contract-v2.md` stating that label verification
   is out of scope, and direct callers to perform label checks via the
   provider (e.g. `gh issue view --json labels`, `forge-cli pr view`, or an
   equivalent provider-native call).
3. **Record the decision in the crate CHANGELOG.** Note that #535 is
   closed by contract clarification rather than by adding a flag, so future
   readers do not re-open the same proposal.
4. **No CLI source changes.** No new flags, no new fixtures, no behavior
   change. This is a docs-only fix landing on `main` once spec wording is
   agreed.

## Out of scope

- Adding `--label` (or any new flag) to `record audit`.
- Adding typed provider-label fetching to `lifecycle_record::audit_record`.
- Changing how `record open` / `record post` / `record close` accept or
  apply labels — those write paths are unaffected.
- Building a separate `plan-issue` subcommand for label verification — if
  one is needed in future, it should be opened as a separate proposal.

## Validation strategy

- `plan-tooling validate` on this bundle passes.
- `cargo test -p nils-plan-issue-cli` continues to pass (no production code
  changes; only docs and CHANGELOG).
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` passes for the
  docs additions.
- Issue #535 is closed with a comment linking to the tracking issue's
  closeout, citing the v2 spec change as the contract decision.

## Open questions carried into execution

- none

## Linked records

- Source issue: <https://github.com/sympoies/nils-cli/issues/535>
- Affected spec: `crates/plan-issue-cli/docs/specs/issue-backed-plan-record-contract-v2.md`

## Execution

- Recommended plan: docs/plans/plan-issue-record-audit-label-contract/plan-issue-record-audit-label-contract-plan.md
- Recommended execution state: docs/plans/plan-issue-record-audit-label-contract/plan-issue-record-audit-label-contract-execution-state.md
