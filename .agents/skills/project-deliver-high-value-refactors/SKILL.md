---
name: project-deliver-high-value-refactors
description: Find and implement high-value test/stability/shared-foundation refactors across crates, then deliver through the current GitHub PR workflow.
---

# Nils CLI Deliver High Value Refactors

## Contract

Prereqs:

- Run inside the `nils-cli` git work tree.
- Rust toolchain available on `PATH` (`cargo`, `rustfmt`, `clippy`).
- `git`, `gh`, `agent-runtime`, `forge-cli`, `semantic-commit`, and
  `review-specialists` available when delivering the PR end-to-end.
- Use this skill together with:
  - `$deliver-github-pr` as the canonical delivery policy.
  - `$create-github-pr` / `$close-github-pr` only when the delivery workflow
    explicitly needs a narrower PR lifecycle surface.

Inputs:

- Optional scope hints:
  - target crate(s) to prioritize
  - constraints (time, risk tolerance, out-of-scope areas)
- Optional quality priorities:
  - `coverage-first`, `stability-first`, `shared-extraction-first`

Outputs:

- One of two outcomes:
  - `Implement`: at least one high-value refactor is implemented with tests and
    validation evidence, then delivered via `$deliver-github-pr`.
  - `No Action`: no high-value target found; return concrete recommendations and potential issue list.
- Reporting split (strict):
  - `Implement`: use `$deliver-github-pr` delivery contract end-to-end
    (open PR, wait checks, run the required review gate, merge unless
    explicitly stopped with `--no-merge`).
  - `No Action`: use `.agents/skills/project-deliver-high-value-refactors/references/NO_ACTION_RESPONSE_TEMPLATE.md`.

Exit codes:

- `0`: completed workflow (implemented changes or no-action report)
- `1`: command/runtime failure while executing workflow
- `2`: usage/scope ambiguity that blocks safe execution

Failure modes:

- No candidate passes the value gate (avoid refactor-for-refactor).
- Candidate requires behavior changes that break parity expectations.
- Shared extraction crosses crate boundaries with unclear ownership or high regression risk.
- Unable to run targeted or local-fast validation commands in the current
  environment.

## Scripts (only entrypoints)

- `.agents/skills/project-deliver-high-value-refactors/scripts/render-refactor-response-template.sh` (`No Action` response only)
- Implement delivery uses the released CLIs directly through `$deliver-github-pr`;
  do not call a skill-owned or runtime-home delivery script.

## Workflow

1. Build candidate inventory (all crates, evidence-first)

- Review each crate for:
  - missing tests around observable behavior, edge cases, and error paths
  - flaky or brittle logic (implicit assumptions, weak error handling, unstable output contracts)
  - duplicated domain-neutral helpers that could move into shared foundations crates:
    - `crates/nils-common`
    - `crates/nils-term`
    - `crates/nils-test-support`
- Capture each candidate with concrete evidence (file path + why it matters).

2. Apply the value gate (must pass before any refactor)

- A candidate is implementable only if it satisfies at least one:
  - improves correctness/stability for user-visible behavior
  - adds meaningful coverage for uncovered critical paths
  - removes duplicated logic used by 2+ crates via shared foundations extraction
- Reject candidates that are style-only, cosmetic-only, or low-impact churn.

3. Decide branch

- If one or more candidates pass:
  - choose smallest high-value slice
  - implement with behavior parity preserved
  - add/expand tests first or alongside code changes
- If none pass:
  - do not refactor
  - produce a no-action recommendations report using the no-action template

4. Implementation rules (when branch is `Implement`)

- Prefer characterization tests before moving logic.
- Keep crate-local adapters for user-facing messages/exit-code policy when extracting shared helpers.
- Extract only domain-neutral primitives into shared foundations crates.
- Avoid bundling unrelated cleanup in the same change set.

5. Validation

- Run targeted tests for touched crates first.
- Run the default changed-scope gate:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
- If debugging CI parity or release-quality risk locally, run the full stack
  explicitly:
  - `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh`
- Report exact commands and pass/fail status.

6. Delivery (required for implemented changes)

- Confirm the working tree contains only the intended refactor change set. If
  the active checkout has unrelated dirty state, isolate the work in a clean
  sibling worktree from `main`.
- Create or reuse a feature branch from the confirmed `main` base. Use
  lowercase, hyphenated names such as `feat/<slug>` unless the active project
  rules require a more specific prefix.
- Commit with `semantic-commit`; direct `git commit` is not the delivery path.
- Render the PR body with `agent-runtime pr-body render`, including concrete
  `## Summary` and `## Test plan` content grounded in the actual diff and
  validation evidence.
- Deliver through `$deliver-github-pr` / `forge-cli pr deliver`:
  - open a PR against `main`;
  - wait for required provider checks;
  - run the mandatory `code-review-pre-merge-gate` scope with at least
    `testing` and `maintainability`;
  - repair concrete findings on the same branch and rerun affected validation;
  - merge through `forge-cli pr merge` unless the user explicitly requested
    `--no-merge`.
- After delivery, restore branch/worktree state:
  - in the primary checkout, switch back to `main`;
  - in a linked or temporary worktree, do not leave `main` checked out; detach
    the worktree at `HEAD` or remove the worktree after confirming no local
    changes remain.
- The `Implement` branch is not complete until the PR URL, check state, review
  gate outcome, merge or `--no-merge` stop state, and final branch/worktree
  state are known.

7. Response contract (always required)

- `Implement` path: report `$deliver-github-pr` artifacts:
  - PR URL
  - check status summary
  - pre-merge review gate outcome
  - merge commit SHA, or the explicit `--no-merge` stop state
  - final branch/worktree state
- `No Action` path: use the no-action template with concrete recommendation
  list and potential issues; do not open a PR when no repo changes were made.
- Render helpers:
  - `./.agents/skills/project-deliver-high-value-refactors/scripts/render-refactor-response-template.sh --mode no-action`
