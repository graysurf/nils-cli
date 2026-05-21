# Testing Audits and CLI Contracts Improvement Record

<!-- markdownlint-disable MD013 -->

## Status

- Date: 2026-05-21
- Status: ready for implementation planning
- Source issue: https://github.com/sympoies/nils-cli/issues/421
- Retention intent: plan-source artifact; eligible for cleanup after execution
  if the tracking issue keeps the durable closeout record.

## Purpose

Capture the testing specialist findings from issue #421 as the primary source
for an execution plan. The goal is to make the shared-helper audit trustworthy
again and add narrow CLI-level tests where current coverage proves only partial
behavior.

## Current Judgment

The findings are implementation-ready and should be handled as focused testing
and audit-hardening work. The shared-helper audit should be fixed first because
its stale seeded paths make the report unreliable as a source of truth for
future helper extraction. The remaining items are targeted CLI contract coverage
gaps and should not expand into broad behavior rewrites unless a new test proves
the current implementation violates the intended contract.

## Findings

| Priority | Issue | Evidence | Fix location | Acceptance |
| --- | --- | --- | --- | --- |
| P1 | `shared-helper-adoption-audit.sh` seeds stale integration-test paths, so adoption evidence misses current files. | Issue #421 reports 37 rows with 32 detection misses; local inspection shows stale paths such as `crates/gemini-cli/tests/paths.rs` in the seed manifest while the tree now uses `tests/integration/...` for many crates. | `scripts/dev/shared-helper-adoption-audit.sh`; add or wire a self-test under `scripts/ci/tests/`. | Seeded paths are current or deliberately generated, missing seeded files fail a self-test, and the audit output can be trusted before helper extraction work. |
| P2 | `cli_template_runs_without_subcommand` checks only exit code 0. | `crates/cli-template/tests/integration/cli.rs` asserts `output.code == 0` without pinning stdout/stderr for the no-subcommand path. | `crates/cli-template/tests/integration/cli.rs`; implementation only if current output is not the intended contract. | The no-subcommand happy path pins meaningful stdout/stderr or explicit silence, plus the exit code. |
| P2 | Overlay dry-run CLI coverage proves the stderr announcement but not no-mutation or the post-overlay effective plan. | `crates/agent-runtime-cli/tests/integration/install_flags.rs` has `overlay_consumption_is_announced_on_stderr` but no CLI boundary assertion that dry-run avoids filesystem mutation and reflects overlay-dropped entries in the resolved plan. | `crates/agent-runtime-cli/tests/integration/install_flags.rs`; `crates/agent-runtime-cli/src/commands/install.rs` only if output is insufficient. | CLI tests prove overlay dry-run consumes the overlay, does not mutate live/state homes, and reports the effective plan/action count after overlay merge. |
| P2 | Uninstall recovery evidence is covered at the library layer, not the operator-facing CLI output/error contract. | `crates/agent-runtime-cli/tests/integration/uninstall.rs` verifies `SymlinkSkippedForeign` recovery data in `uninstall::run`, while CLI printing/error mapping remains unpinned. | `crates/agent-runtime-cli/tests/integration/uninstall.rs`; `crates/agent-runtime-cli/src/commands/uninstall.rs` only if output or error mapping drifts. | CLI tests prove foreign symlink recovery output names actual and expected targets, and missing link-map errors map to the intended operator-facing failure contract. |

## Ownership Boundary

- Runtime behavior is in scope only when a new contract test exposes a real
  mismatch. The preferred change shape is test and audit hardening.
- Shared helper extraction is out of scope; this work only restores the audit
  that will guide later extraction.
- Issue #421 remains the original review finding source. The sibling plan owns
  task sequencing and validation gates.

## Executable Backlog

1. Refresh or generate shared-helper audit candidates and add a regression
   self-test for stale seeded paths.
2. Strengthen the `cli-template` no-subcommand test to assert the intended text
   or silence contract.
3. Add overlay dry-run CLI tests for no-mutation and post-overlay effective
   plan behavior.
4. Add uninstall CLI tests for foreign symlink recovery output and missing
   link-map error mapping.

## Validation Gates

- `bash scripts/dev/shared-helper-adoption-audit.sh --format tsv --out target/testing-audits-cli-contracts/shared-helper-adoption.tsv`
- `bash scripts/ci/test-stale-audit.sh --strict`
- `cargo test -p nils-cli-template --test integration cli_template_runs_without_subcommand`
- `cargo test -p agent-runtime-cli --test integration`
- `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage`

## Execution

- Recommended plan: docs/plans/testing-audits-cli-contracts/testing-audits-cli-contracts-plan.md
- Recommended execution state: docs/plans/testing-audits-cli-contracts/testing-audits-cli-contracts-execution-state.md
- Recommended next task source: start with the shared-helper audit fix before
  adding CLI contract tests.

## Guardrails

- Do not treat a passing `test-stale-audit.sh` as proof that
  `shared-helper-adoption-audit.sh` has no stale seed paths; they answer
  different questions.
- Do not widen the plan into helper extraction or runtime installer redesign.
- If a CLI contract is ambiguous, pin the intended operator-facing behavior in
  the test name and assertion before changing production code.

## Open Questions

- None. The defaults above are sufficient for execution.
