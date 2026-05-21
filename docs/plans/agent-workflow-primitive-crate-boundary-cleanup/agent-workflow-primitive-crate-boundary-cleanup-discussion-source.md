# Agent Workflow Primitive Crate Boundary Cleanup Source

- Status: open, ready for implementation planning
- Date: 2026-05-21
- Source issue: <https://github.com/sympoies/nils-cli/issues/425>

## Execution

- Recommended plan: docs/plans/agent-workflow-primitive-crate-boundary-cleanup/agent-workflow-primitive-crate-boundary-cleanup-plan.md
- Recommended execution state: docs/plans/agent-workflow-primitive-crate-boundary-cleanup/agent-workflow-primitive-crate-boundary-cleanup-execution-state.md

## Purpose

The workflow evidence primitive area has one concrete publish-order defect and
one low-cost crate-boundary cleanup opportunity. The target is a narrow
repository cleanup that keeps the public `test-first-evidence` binary contract
stable while removing the standalone `nils-test-first-evidence` package
boundary before that package is first published.

## Confirmed Facts

- GitHub issue #425 is open and marked `ready-for-implementation`.
- `crates/agent-workflow-primitives` owns local-first workflow primitive
  binaries such as `browser-session`, `canary-check`, `docs-impact`,
  `heuristic-inbox`, `model-cross-check`, `repo-retro`,
  `review-evidence`, `review-specialists`, and `skill-usage`.
- `crates/test-first-evidence` currently owns the `test-first-evidence`
  binary as a separate workspace package named `nils-test-first-evidence`.
- `crates/test-first-evidence` exposes JSON schemas and command names under
  the public `test-first-evidence` CLI contract.
- `completions/bash/test-first-evidence` and
  `completions/zsh/_test-first-evidence` already exist as generated
  completion assets.
- `crates/agent-workflow-primitives/Cargo.toml` depends on `nils-term`.
- `release/crates-io-publish-order.txt` currently lists
  `nils-agent-workflow-primitives` before `nils-term`, which is invalid for
  local dependency publish order.
- A prior dry run in issue #425 showed
  `scripts/publish-crates.sh --dry-run --crates "nils-agent-workflow-primitives nils-term"`
  failing because the dependent package appears before its dependency.
- A prior crates.io status check in issue #425 showed workspace version
  `0.15.0` missing for `nils-test-first-evidence` and
  `nils-agent-workflow-primitives`; `nils-test-first-evidence` does not yet
  exist on crates.io.

## Decisions

1. Move the `test-first-evidence` binary implementation into
   `crates/agent-workflow-primitives`.
2. Preserve the installed binary name `test-first-evidence`.
3. Preserve JSON schema version strings, exit-code semantics, help shape,
   completion behavior, and record file names.
4. Remove the standalone `crates/test-first-evidence` package boundary from
   the workspace after the binary is rehomed.
5. Fix `release/crates-io-publish-order.txt` so `nils-term` appears before
   `nils-agent-workflow-primitives`.
6. Keep `web-evidence` standalone because it carries HTTP and network
   behavior with `reqwest` dependencies.
7. Keep `agent-scope-lock` standalone because it is an edit-scope guardrail,
   not a workflow evidence record.

## Scope

In scope:

- Workspace metadata changes needed to remove the standalone
  `nils-test-first-evidence` package.
- A new `test-first-evidence` binary target under
  `nils-agent-workflow-primitives`.
- Source, tests, docs, README content, completion assets, and coverage matrix
  updates needed for the binary rehome.
- Publish-order correction for `nils-term` and
  `nils-agent-workflow-primitives`.
- Targeted regression tests that prove the CLI contract did not change.

Out of scope:

- Any semantic redesign of the `test-first-evidence` record format.
- Renaming the public `test-first-evidence` binary.
- Publishing crates, tagging a release, or bumping Homebrew.
- Moving `web-evidence` or `agent-scope-lock`.
- Changing `agent-kit` workflow behavior in this repository.

## Requirements

- `test-first-evidence -V` and `test-first-evidence --help` must continue to
  work from the workspace build.
- Existing JSON schema version strings must remain byte-stable.
- Existing data and usage exit codes must remain byte-stable.
- `test-first-evidence completion zsh` and `completion bash` must continue to
  render valid completion scripts.
- The workspace binary inventory must still include `test-first-evidence`.
- The workspace must no longer include a standalone
  `crates/test-first-evidence` package after the move.
- The publish-order list must not include `nils-test-first-evidence` after the
  package is removed.
- The publish-order list must place `nils-term` before
  `nils-agent-workflow-primitives`.

## Acceptance Criteria

- `cargo test -p nils-agent-workflow-primitives test_first_evidence` passes.
- `cargo test -p nils-agent-workflow-primitives --test integration` passes.
- `cargo run -p nils-agent-workflow-primitives --bin test-first-evidence -- --help`
  prints the expected command surface.
- `zsh -n completions/zsh/_test-first-evidence` passes.
- `bash -n completions/bash/test-first-evidence` passes.
- `bash scripts/workspace-bins.sh` still reports `test-first-evidence`.
- `bash scripts/publish-crates.sh --dry-run --crates "nils-term nils-agent-workflow-primitives"`
  no longer fails because of local dependency order.
- Required docs-only and full workspace gates remain green for the touched
  surface.

## Risks And Guardrails

- Preserve the public CLI contract first. The implementation may move, but
  downstream skills should not need a command, schema, or exit-code migration.
- Remove stale package metadata in the same change set as the move so the
  workspace does not advertise a non-buildable package.
- Do not leave completion assets tied to the deleted package boundary.
- Do not hard-code user-local paths in docs, tests, or generated artifacts.
- Keep release and Homebrew work for a separate release workflow after this
  cleanup has merged.

## Open Questions

- Should the old crate README be deleted outright or folded into
  `crates/agent-workflow-primitives/README.md`? Recommended default: fold the
  user-facing contract into the multi-binary crate README and delete the old
  package docs with the package.
- Should the publish dry run include only the affected pair or the full publish
  order? Recommended default: use the affected pair during implementation and
  rely on the required full gate before delivery.

## Retention Intent

This source doc is execution coordination for issue #425. It can be removed
with the sibling plan after the implementation is complete and any durable CLI
contract details have been promoted into crate docs or runbooks.
