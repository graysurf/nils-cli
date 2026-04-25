# plan-issue Gate Matrix v1

## Purpose

Define the canonical gate matrix for `plan-issue` v1 commands so transition checks and failure
semantics remain consistent with the current shell orchestration flow.

This document is normative for which gates apply to each command and when those gates must block
execution.

## Scope

- In scope:
  - Command-to-gate applicability.
  - Required validation inputs per gate.
  - Failure behavior by gate category.
- Out of scope:
  - Full output formatting details.
  - Internal Rust module boundaries.

## Gate Catalog

| Gate ID | Gate name | Applies to | Pass criteria | Fail code |
| --- | --- | --- | --- | --- |
| `G0` | Argument/usage validation | all commands | required flags/options parse successfully | `2` |
| `G1` | Body structure invariants | `start-plan` (rendered body), `status-plan`, `link-pr` (post-mutation), `ready-plan`, `close-plan`, `cleanup-worktrees`, `start-sprint`, `ready-sprint`, `accept-sprint` | task table exists with required columns; valid `Status`/`Execution Mode` values; row owner policy and shared-lane Owner/Branch/Worktree alignment hold | `1` |
| `G2` | PR normalization + presence | `start-sprint` (previous sprint checks), `accept-sprint`, `close-plan` | required rows have concrete PR references (no placeholders) and normalize to canonical `#<number>` where possible | `1` |
| `G3` | Previous sprint merge gate | `start-sprint` with `N > 1` | sprint `N-1` rows are all `done` and all referenced PRs are merged | `1` |
| `G4` | Sprint acceptance gate | `accept-sprint` | valid `--approved-comment-url`, sprint PRs merged, sprint rows sync to `done` | `1` |
| `G5` | Plan close gate | `close-plan` | valid final approval URL; close-after-review invariants pass (`done`, merged PRs, owner policy) | `1` |
| `G6` | Worktree cleanup gate | `cleanup-worktrees`, successful `close-plan` | targeted linked worktrees removed, prune succeeds, no targeted residues | `1` |
| `G7` | Dry-run non-mutation gate | all commands with `--dry-run` | command prints intended actions and performs no GitHub mutation | `1` |
| `G8` | Close-plan dry-run body-file gate | `close-plan --dry-run` | `--body-file` is provided for local gate evaluation | `2` |
| `G9` | Runtime-truth drift gate | `start-sprint` (live binary path) | sprint issue rows match plan-derived runtime lane metadata before artifact render | `1` |
| `G10` | Link-pr target selection gate | `link-pr` | `--task` resolves to a row (or shared-lane group); `--sprint` resolves to a single PR lane (or `--pr-group` selects one) | `1` |

## Command-to-Gate Matrix

| Command | G0 | G1 | G2 | G3 | G4 | G5 | G6 | G7 | G8 | G9 | G10 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `build-task-spec` | required | - | - | - | - | - | - | optional | - | - | - |
| `build-plan-task-spec` | required | - | - | - | - | - | - | optional | - | - | - |
| `start-plan` | required | required (rendered body) | - | - | - | - | - | optional | - | - | - |
| `status-plan` | required | required | - | - | - | - | - | optional | - | - | - |
| `link-pr` | required | required (post-mutation) | - | - | - | - | - | optional | - | - | required |
| `ready-plan` | required | required | - | - | - | - | - | optional | - | - | - |
| `close-plan` | required | required | required | - | - | required | required (on success path) | optional | required when dry-run | - | - |
| `cleanup-worktrees` | required | required | - | - | - | - | required | optional | - | - | - |
| `start-sprint` | required | required | required for `N > 1` | required for `N > 1` | - | - | - | optional | - | required (live binary path) | - |
| `ready-sprint` | required | required (live binary path) | - | - | - | - | - | optional | - | - | - |
| `accept-sprint` | required | required | required | - | required | - | - | optional | - | - | - |
| `multi-sprint-guide` | required | - | - | - | - | - | - | optional | - | - | - |

## Gate Evaluation Order (Normative)

1. `G0` usage/argument validation.
2. Command-specific structural checks (`G1`) before remote mutations. For `link-pr`, `G1` runs after the in-memory PR/status mutation so
   the post-update body is validated before any GitHub write.
3. Selection/progression/merge/drift gates (`G10`, `G2`, `G3`, `G4`, `G5`, `G9`) in command-specific order.
4. Cleanup gate (`G6`) only after command gate success where cleanup is required.
5. Dry-run behavior (`G7`, `G8`) wraps command execution and must preserve non-mutation semantics.

## Data Sources and Inputs

- Plan and sprint metadata: parsed from plan files and command flags.
- Task decomposition rows: sourced from live issue body or `--body-file`.
- PR merge state: sourced from GitHub for referenced PR numbers.
- Approval URLs: validated against GitHub issue/pull comment URL format.
- Worktree targets: resolved from task `Branch` and `Worktree` columns.

## Failure Contract

- `exit 2`: usage errors (`G0`, `G8`) and invalid required inputs.
- `exit 1`: gate failures (`G1` through `G7`, `G9`, `G10`) and runtime dependency failures.
- `exit 0`: all applicable gates pass.
