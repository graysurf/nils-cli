# Plan: Unified `git-cli worktree` Surface + Direct-`git worktree` Ban

## Overview

Make git worktrees the primary agent-isolation workflow ergonomic and governed,
removing two recurring pains: raw `git worktree` is awkward (a branch can be
checked out in only one worktree, so agents fight the "one branch" rule) and
agents scatter worktree folders because no placement convention is enforced.

The solution mirrors how this runtime kit already governs `git commit` and
`gh pr create`: one sanctioned CLI surface plus a hook that forbids the raw
mutating command. Phase 1 adds a `worktree` subcommand group to the existing
`git-cli` crate (`add`/`list`/`remove`/`prune`) with a deterministic path
convention `$AGENT_HOME/worktrees/<repo-key>/<branch-slug>` — the agent never
picks a path, and `add` always creates a fresh branch so the one-branch rule is
invisible. Phase 2 adds a global PreToolUse hook in `agent-runtime-kit` that
blocks raw mutating `git worktree`, with an explicit override escape hatch, and
the `AGENTS.md` / `AGENT_HOME.md` policy — enabled only after Phase 1 ships and
coverage is proven.

This is cross-repo: Phase 1 (Sprint 1) lands in `sympoies/nils-cli`; Phase 2
(Sprint 2) lands in `graysurf/agent-runtime-kit`. The tracking issue lives in
`sympoies/nils-cli` and tracks the whole initiative.

## Read First

- Primary source:
  `docs/plans/2026-05-31-git-cli-worktree-surface/git-cli-worktree-surface-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Repo anchors:
  - `crates/git-cli/src/cli.rs` (command surface to extend), `src/main.rs`
    (dispatch), `src/branch.rs` (`branch cleanup --remove-worktrees`,
    `linked_worktrees_by_branch()` — the porcelain-parse / remove path to share)
  - `crates/git-cli/docs/` (crate docs placement), `crates/git-cli/tests/`
    (integration + completion snapshot)
  - `crates/plan-issue/src/execute.rs` (`cleanup-worktrees`,
    `list_linked_worktrees()` — the second removal path; evaluate sharing)
  - `crates/plan-issue/docs/specs/plan-issue-contract-v2.md`
    (the dispatch-flow `$ISSUE_ROOT/worktrees/<mode>` convention to stay
    compatible with)
  - `docs/runbooks/new-cli-crate-development-standard.md`,
    `docs/runbooks/cli-completion-development-standard.md`,
    `docs/specs/crate-docs-placement-policy.md` (surface-quality rules; crate is
    pre-existing so scaffold/publish-order items are N/A)
  - agent-runtime-kit `core/hooks/shared/block-direct-git-commit.py` (hook
    pattern to mirror), `core/hooks/claude/settings.hooks.jsonc` (registration),
    `targets/claude/link-map.yaml` (symlink wiring), `AGENT_HOME.md` (policy)
  - agent-runtime-kit
    `core/policies/heuristic-system/error-inbox/worktree-unsigned-commit-config-drift/ENTRY.md`
- Key decisions carried into execution:
  - Home is the existing `git-cli` crate, not `forge-cli` and not a new crate.
  - Convention is `$AGENT_HOME/worktrees/<repo-key>/<branch-slug>`, beside
    `out/` (owned by `agent-out`), not under it.
  - `add` auto-creates a fresh branch; the agent never supplies a path.
  - Two phases: ship and prove the CLI first; enable the hook ban only after,
    and always with an override escape hatch.
- Open questions carried into execution: exact `repo-key` hash basis (lean
  toplevel path); whether the hook blocks read-only `worktree list` (lean: block
  mutation only); whether `plan-issue` removal folds into the shared helper
  or documents a divergence (see the source doc for detail).

## Scope

- In scope:
  - **Sprint 1 (Phase 1, nils-cli)**: `git-cli worktree add|list|remove|prune`,
    the deterministic path convention, removal-logic consolidation, JSON
    contract + completion + crate docs.
  - **Sprint 2 (Phase 2, agent-runtime-kit)**: a global `block-direct-git-worktree`
    PreToolUse hook with an override escape hatch, plus the `AGENTS.md` /
    `AGENT_HOME.md` policy and namespace reconciliation; evaluate promoting the
    `worktree-unsigned-commit-config-drift` heuristic finding.
- Out of scope (Future Work): surfacing harness-created `.claude/worktrees/`
  entries in `worktree list`; a `worktree move`/`switch`/`lock` surface beyond
  add/list/remove/prune; migrating existing scattered worktrees into the new
  convention; any cross-repo automation that opens the Phase 2 hook PR
  automatically.

## Assumptions

1. `AGENT_HOME` is set in agent sessions (env from `~/.zshenv`); when unset,
   `git-cli worktree` resolves a deterministic fallback (e.g. the platform
   state dir) rather than failing, and reports the resolved root.
2. `agent-out` owns only `$AGENT_HOME/out/`; `$AGENT_HOME/worktrees/` is free to
   adopt (verified at planning; re-confirm at implementation via `agent-out`).
3. `cargo test`, `cargo clippy -D warnings`, `cargo fmt`, the `--help` /
   completion snapshot, `rumdl`, and `bash scripts/ci/nils-cli-checks-entrypoint.sh
   --local-fast` are the gating validation surface for Sprint 1, self-checked via
   `gh pr checks` before merge.
4. The Phase 2 hook is enabled globally only after the Phase 1 release is
   brew-upgraded on the host (version-pin gate) and worktree-add coverage is
   confirmed against real agent usage; the override escape hatch ships from day
   one so a host without the new CLI is never trapped.
5. The Claude harness `EnterWorktree` path (`.claude/worktrees/`) is a separate,
   out-of-scope creator; the hook targets Bash `git worktree` mutations only.

## Sprint 1: `git-cli worktree` Surface And Path Convention (Phase 1)

**Goal**: Ship `git-cli worktree add|list|remove|prune` with a deterministic
`$AGENT_HOME/worktrees/<repo-key>/<branch-slug>` convention, consolidate
worktree removal into one helper, and document the surface. Lands in
`sympoies/nils-cli`.

**PR grouping intent**: group (PR1)
**Execution Profile**: serial

### Task 1.1: `worktree` CLI subtree + deterministic path convention

- **Location**:
  - `crates/git-cli/src/cli.rs`
  - `crates/git-cli/src/main.rs`
  - `crates/git-cli/src/worktree.rs` (new)
- **Description**: Add a top-level `Worktree` command group with `add`, `list`,
  `remove`, and `prune` subcommands and their arg structs, then wire dispatch.
  `add` takes a slug positional and an optional `--from` base; `remove` takes a
  slug-or-path. Implement the path helper: resolve `$AGENT_HOME` (env, with a
  deterministic fallback when unset), compute a `repo-key` (toplevel basename
  plus a short stable hash of the absolute toplevel path so distinct or
  same-named repos never collide) and a `branch-slug`, and return the path
  `$AGENT_HOME/worktrees/{repo-key}/{branch-slug}`. `add` creates a fresh branch
  `feat/{slug}` from the `--from` base (default the repo default branch or
  current HEAD) and runs `git worktree add` at the computed path with that
  branch; the agent never supplies a path. `add` must not enable
  `extensions.worktreeConfig` and must not set per-worktree identity (respect
  the signing-drift finding [F7]).
- **Dependencies**:
  - none
- **Complexity**: 3
- **Acceptance criteria**:
  - `git-cli worktree add foo` creates a worktree at the computed deterministic
    path on a fresh branch `feat/foo`; the path formula is stable across runs
    (unit test) and isolates two repos with the same basename; re-running `add`
    for an existing slug either resumes idempotently or fails with a clear
    message, never at a random path.
- **Validation**:
  - `cargo test -p nils-git-cli` path-formula + arg-parse unit cases and an
    `add`/`list` integration test against a tempdir git repo.

### Task 1.2: `remove` / `prune` + consolidate worktree-removal into one helper

- **Location**:
  - `crates/git-cli/src/worktree.rs`
  - `crates/git-cli/src/branch.rs` (rewire `cleanup --remove-worktrees`)
- **Description**: Implement `worktree remove` (slug-or-path)
  (`git worktree remove --force` + `git worktree prune`) and `worktree prune`.
  Extract the porcelain-parse / list / remove primitive into ONE helper and
  rewire `git-cli branch cleanup --remove-worktrees`
  (`linked_worktrees_by_branch()`) onto it so there is no duplicate parser inside
  `git-cli`. Evaluate folding `plan-issue cleanup-worktrees`
  (`list_linked_worktrees()`) onto the same helper; if cross-crate sharing is
  disproportionate, document the divergence explicitly rather than forcing it —
  but `git-cli`'s own two paths must share one helper.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 3
- **Acceptance criteria**:
  - `worktree remove`/`prune` work and never touch the primary checkout or an
    in-use worktree; `git-cli` has a single worktree-removal/listing code path
    (`branch cleanup --remove-worktrees` delegates to it); existing
    `git-cli` (and, if folded, `plan-issue`) worktree tests still pass; any
    intentional divergence is documented.
- **Validation**:
  - `cargo test -p nils-git-cli` remove/prune + safety (skip primary / in-use)
    cases; if folded, the relevant `cargo test -p nils-plan-issue`
    cleanup-worktrees cases; clippy `-D warnings`, fmt.

### Task 1.3: JSON contract, completion, and crate docs

- **Location**:
  - `crates/git-cli/src/worktree.rs` (versioned JSON envelope)
  - `crates/git-cli/tests/` (`--help` / completion snapshot + golden fixtures)
  - `crates/git-cli/docs/specs/git-cli-worktree-convention.md` (new, crate-local)
  - `crates/git-cli/README.md` (or crate docs README)
- **Description**: Add `--format json` versioned envelopes for `add`/`list`/
  `remove`/`prune` per the JSON-contract guideline (no secret leakage, stable
  error envelope); update the `--help` / completion snapshot following the
  completion standard; document the `worktree` surface and the
  `$AGENT_HOME/worktrees/<repo-key>/<branch-slug>` convention in a crate-local
  spec and the README. `git-cli` is a pre-existing crate, so new-crate
  scaffold / publish-order items are N/A — only surface-quality rules apply.
- **Dependencies**:
  - Task 1.1, Task 1.2
- **Complexity**: 2
- **Acceptance criteria**:
  - Each subcommand emits a documented, versioned envelope in `--format json`
    and a clean human default; the completion snapshot covers the new
    subcommands and flags; the convention is documented; `--local-fast`,
    `rumdl`, and the docs-placement audit are green.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`;
    `bash scripts/ci/docs-placement-audit.sh --strict`; `rumdl check`;
    `gh pr checks` green.

## Sprint 2: Direct-`git worktree` Ban And Policy (Phase 2)

**Goal**: Add a global PreToolUse hook that blocks raw mutating `git worktree`
(with an override escape hatch) and the `AGENTS.md` / `AGENT_HOME.md` policy,
and evaluate promoting the signing-drift heuristic finding. Lands in
`graysurf/agent-runtime-kit`; enabled only after Sprint 1 ships and coverage is
proven.

**PR grouping intent**: group (PR2)
**Execution Profile**: serial

### Task 2.1: `block-direct-git-worktree` hook with override escape hatch

- **Location**:
  - agent-runtime-kit `core/hooks/shared/block-direct-git-worktree.py` (new)
  - agent-runtime-kit `core/hooks/claude/settings.hooks.jsonc` (register)
  - agent-runtime-kit `targets/claude/link-map.yaml` (symlink wiring)
- **Description**: Mirror `block-direct-git-commit.py`: parse the Bash command
  (env prefixes, path-qualified git, wrapper commands) via the shared
  `git_subcommand()` helper, and block when the subcommand is a **mutating**
  worktree op (`add`/`remove`/`move`/`prune`/`repair`/`lock`/`unlock`) while
  letting read-only `worktree list` through. The block reason points to
  `git-cli worktree`. Provide an explicit override env escape hatch
  (e.g. `ALLOW_DIRECT_GIT_WORKTREE=1`), consistent with the commit hook, so a
  host without the new CLI is never trapped. Register after the commit hook in
  the Bash PreToolUse matcher and wire the symlink in `link-map.yaml`.
- **Dependencies**:
  - Task 1.3 (Phase 1 must be shipped and brew-upgraded first; see Assumption 4)
- **Complexity**: 2
- **Acceptance criteria**:
  - With the hook active, a Bash `git worktree add ...` (and other mutating
    forms, including env-prefixed / path-qualified / wrapped) is blocked with a
    message pointing to `git-cli worktree`; the override env lets it through;
    `git worktree list` is not blocked.
- **Validation**:
  - Hook unit/fixture test for block + allow + override + read-only-pass cases;
    `agent-runtime` doctor / audit clean; manual block/allow spot check.

### Task 2.2: `AGENTS.md` / `AGENT_HOME.md` policy + namespace reconciliation + heuristic promotion

- **Location**:
  - agent-runtime-kit `AGENT_HOME.md`
  - agent-runtime-kit
    `core/policies/heuristic-system/error-inbox/worktree-unsigned-commit-config-drift/ENTRY.md`
- **Description**: Add the policy clause: agents use `git-cli worktree`; raw
  `git worktree` mutation is forbidden (enforced by the hook, override-gated);
  document the `$AGENT_HOME/worktrees/<repo-key>/<branch-slug>` convention and
  reconcile the "`$AGENT_HOME` is artifacts only" note to acknowledge the
  `worktrees/` namespace. Because the controlled `worktree add` + this clause +
  the hook satisfy promotion criterion (c) of
  `worktree-unsigned-commit-config-drift`, evaluate promoting/closing that
  finding via `heuristic-inbox` (or record why it stays open).
- **Dependencies**:
  - Task 2.1
- **Complexity**: 2
- **Acceptance criteria**:
  - The policy clause and convention are documented in `AGENT_HOME.md`, the
    "artifacts only" note is reconciled, and the heuristic finding is promoted
    with a rationale (or explicitly kept open with the gap named); `agent-docs
    audit` and markdown lint are green.
- **Validation**:
  - `agent-docs audit`; markdown lint / `rumdl`; `heuristic-inbox` operation
    record for the promotion decision.

## Issue Closeout Gate

The tracking issue is complete when:

- `git-cli worktree add|list|remove|prune` exist; `add` creates a worktree at
  the deterministic `$AGENT_HOME/worktrees/<repo-key>/<branch-slug>` path on a
  fresh `feat/<slug>` branch with the agent never supplying a path; the path
  formula is unit-tested and collision-safe across same-named repos.
- `git-cli` has a single worktree-removal/listing helper that
  `branch cleanup --remove-worktrees` delegates to; any `plan-issue`
  divergence is either folded in or documented.
- Each subcommand emits a documented, versioned `--format json` envelope and a
  clean human default; the completion snapshot and crate docs cover the surface
  and the convention.
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`, `rumdl`, the
  docs-placement audit, clippy `-D warnings`, and fmt are green; `gh pr checks`
  is green; the Phase 1 release ships and the Homebrew tap is bumped.
- The Phase 2 `block-direct-git-worktree` hook blocks raw mutating `git worktree`
  (with the override escape hatch and read-only `list` passing) and is enabled
  globally only after Phase 1 is brew-upgraded and coverage is proven; the
  `AGENTS.md` / `AGENT_HOME.md` policy is merged and the `$AGENT_HOME/worktrees/`
  namespace note is reconciled.
- The `worktree-unsigned-commit-config-drift` heuristic finding is promoted with
  a rationale or explicitly kept open with the remaining gap named.
- The `execution-state.md` ledger has every executed row at `done` with a
  non-empty `Evidence` cell; waived rows are marked `waived` with a reason.
- The closeout comment is preceded by a final
  `tracking run update --note "<closing summary>"` event.

## Future Work (Out Of Scope For This Tracker)

- Surface harness-created `.claude/worktrees/` entries in `git-cli worktree
  list` for a unified view.
- A `worktree move` / `switch` / `lock` surface beyond add/list/remove/prune.
- A migration helper that relocates existing scattered worktrees into the
  `$AGENT_HOME/worktrees/` convention.
- A standalone worktree-config drift audit (`git config --worktree --list` +
  shared-config scan) wired into `pre-push`, if the heuristic finding warrants a
  hard gate beyond the controlled `add`.

## Retention Intent

Plan-source coordination document. Cleanup-eligible after both phases ship and
the tracker closes and archives.
