---
name: project-maintain-test-coverage
description: Maintain nils-cli test coverage from coverage evidence while prioritizing high-value behavioral tests and stale-test cleanup.
---

# Nils CLI Maintain Test Coverage

## Contract

Prereqs:

- Run inside the `nils-cli` git work tree.
- Follow `DEVELOPMENT.md` for Rust tooling and coverage prerequisites,
  especially `cargo-nextest` and `cargo-llvm-cov`.
- Start from current coverage evidence before editing tests or production code:
  `target/coverage/lcov.info`, `target/coverage/coverage-summary.md`, CI
  coverage artifacts, or a freshly generated local coverage run.
- Treat stale coverage artifacts as invalid when they predate the relevant
  code or test changes.

Inputs:

- Optional target hints: crate, module, CLI, command, failing coverage job, or
  coverage artifact path.
- Optional scope constraints: time budget, bug-fix allowance, or areas that
  must remain untouched.

Outputs:

- Coverage evidence summary: artifact path, total line coverage when known,
  and selected hotspot files/modules/functions.
- A small test-maintenance change set that improves behavioral confidence,
  not only the percentage.
- Focused validation for touched crates/tests, followed by fresh coverage
  evidence from CI or an explicit local coverage run when the change is ready.
- When the workflow creates repository changes, a PR delivery path using
  `semantic-commit`, `$deliver-github-pr`, and `forge-cli pr deliver` unless the
  user explicitly requested local-only maintenance.
- Follow-up issue evidence only when a clear bug or unresolved work is found
  outside the coverage-maintenance scope.

Failure modes:

- Coverage evidence is missing, stale, or cannot be regenerated in the current
  environment.
- Candidate tests only exercise shallow happy paths or duplicate existing
  coverage without adding behavioral value.
- A stale or brittle test is removed without evidence that it is obsolete,
  redundant, harmful, or contract-invalid.
- A discovered bug requires separate product or workflow ownership and cannot
  be safely fixed in the current scope.

## Scripts

- `.agents/skills/project-maintain-test-coverage/scripts/coverage-hotspots.sh`

Use the helper to rank file-level LCOV hotspots from existing evidence:

```bash
.agents/skills/project-maintain-test-coverage/scripts/coverage-hotspots.sh \
  --lcov target/coverage/lcov.info \
  --limit 20
```

The helper is read-only. It does not run tests, generate coverage, or change
repository files.

## Workflow

1. Establish evidence before changing tests.

- If a relevant `target/coverage/lcov.info` already exists, summarize it:
  `bash scripts/ci/coverage-summary.sh target/coverage/lcov.info`.
- If the artifact is missing or stale and the environment has the required
  tooling, generate explicit local coverage with:

```bash
NILS_CLI_TEST_RUNNER=nextest \
  bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage
```

- Inspect hotspots with the skill helper, CI artifact, and optional HTML report
  from `cargo llvm-cov report --html --output-dir target/coverage`.

2. Choose a high-value target.

- Prefer uncovered or weakly covered behavior in:
  - error paths and invalid input handling
  - boundary conditions and edge cases
  - regression cases tied to real failures or issue evidence
  - CLI help/version/exit-code contracts
  - JSON, Markdown, or machine-readable output contracts
  - state transitions, filesystem mutation rules, and cleanup/rollback paths
- Reject targets whose only value is raising the line-coverage percentage.
- Keep the slice small enough to validate locally without unrelated cleanup.

3. Maintain tests deliberately.

- Add or revise tests near the behavior owner and follow existing crate test
  style.
- In Rust tests, prefer `pretty_assertions::{assert_eq, assert_ne}` when
  comparing structured or multi-line output.
- Remove or rewrite a stale test only when evidence shows it is obsolete,
  redundant, brittle, or asserting an invalid contract. State that evidence in
  the final report.
- Avoid broad snapshots, noisy end-to-end tests, sleeps, network dependence, or
  fixtures that make the suite harder to diagnose.

4. Handle discovered bugs.

- If the bug is tightly scoped to the coverage target, fix it with a regression
  test in the same change.
- If the bug is broader, risky, or needs product/workflow ownership, preserve
  the evidence and open or update a follow-up with `$issue-follow-up`.
- Do not invoke `$issue-follow-up` for every coverage run; use it only for a
  clear bug or unresolved follow-up.

5. Validate in increasing scope.

- Run the smallest focused command first, such as:

```bash
cargo nextest run -p <crate> <test-filter>
cargo test -p <crate> <test-filter>
```

- Run related crate checks and the default local-fast gate after the focused
  loop is green.
- Finish non-doc coverage-maintenance changes with CI coverage evidence, or run
  explicit local coverage when the feedback is needed before pushing:

```bash
NILS_CLI_TEST_RUNNER=nextest \
  bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage
```

6. Deliver changed repo state through PR.

- If the workflow produced test or product-code changes, confirm the working
  tree contains only the intended coverage-maintenance change set. Use a clean
  sibling worktree from `main` when the active checkout has unrelated dirty
  state.
- Commit with `semantic-commit`; direct `git commit` is not the delivery path.
- Render the PR body with `agent-runtime pr-body render`. Put the coverage
  artifact path, hotspot summary, focused test commands, local-fast result, and
  any explicit coverage run in `## Test plan`.
- Deliver through `$deliver-github-pr` / `forge-cli pr deliver` against `main`.
  Wait for required checks and the required pre-merge review gate unless the
  user explicitly requested `--no-merge`.
- After delivery, restore branch/worktree state:
  - in the primary checkout, switch back to `main`;
  - in a linked or temporary worktree, do not leave `main` checked out; detach
    the worktree at `HEAD` or remove the worktree after confirming no local
    changes remain.
- If no repo changes were made, do not open a PR; report the coverage evidence
  and rejected candidates instead.

## Boundary

This skill guides coverage-maintenance work only. It does not lower the 85%
coverage gate, edit CI thresholds, broaden snapshots for percentage-only gains,
or require follow-up issues when no clear bug or unresolved work exists. PR
delivery is limited to the coverage-maintenance changes this workflow created.
If hotspot extraction needs to become a stable repository command, first
extract that deterministic behavior into released `nils-cli` tooling and keep
this skill as the orchestration layer.
