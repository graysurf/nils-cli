# Unified `git-cli worktree` Surface + Direct-`git worktree` Ban Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: complete; tracking issue #712 closed on 2026-05-31 after nils-cli
  PR #715, agent-runtime-kit PR #213, and release v0.31.5 shipped.
- Target scope: Phase 1 — `crates/git-cli` (`nils-git-cli`) `worktree` surface
  in `sympoies/nils-cli`; Phase 2 — `block-direct-git-worktree` hook +
  `AGENT_HOME.md` policy in `graysurf/agent-runtime-kit`. Cross-repo; the
  tracking issue lives in `sympoies/nils-cli`.
- Execution window: Sprint 1 (`worktree add|list|remove|prune` + convention +
  removal consolidation + docs, PR1, nils-cli) -> Sprint 2 (hook ban + policy +
  heuristic promotion, PR2, agent-runtime-kit), completed serially.
- Current task: complete.
- Next task: archive the closed plan bundle.
- Last updated: 2026-05-31
- Branch/commit/PR: delivered through PR sympoies/nils-cli#715
  (`29e490c`) and PR graysurf/agent-runtime-kit#213 (`83de974`);
  released as nils-cli v0.31.5.
- Source document: docs/plans/2026-05-31-git-cli-worktree-surface/git-cli-worktree-surface-plan.md
- Direct source-doc execution waiver: not applicable
- Tracking issue: <https://github.com/sympoies/nils-cli/issues/712>
- Source snapshot:
  <https://github.com/sympoies/nils-cli/issues/712#issuecomment-4585976468>
- Plan snapshot:
  <https://github.com/sympoies/nils-cli/issues/712#issuecomment-4585976506>
- Initial state snapshot:
  <https://github.com/sympoies/nils-cli/issues/712#issuecomment-4586270506>

## Validation Plan

- Sprint 1: `cargo test -p nils-git-cli` path-formula + arg-parse cases (stable
  path, same-named-repo isolation), `add`/`list`/`remove`/`prune` integration
  against a tempdir repo (incl. skip-primary / skip-in-use safety), and the
  single-helper consolidation (no duplicate porcelain parser in `git-cli`); if
  `plan-issue` folds in, its `cleanup-worktrees` cases; `--format json`
  golden fixtures for each subcommand; `--help` / completion snapshot updated;
  clippy `-D warnings`, fmt, `rumdl`, docs-placement audit clean;
  `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` and
  `gh pr checks` green; Phase 1 release shipped and Homebrew tap bumped.
- Sprint 2: hook fixture test for block / allow / override / read-only-pass
  cases (env-prefixed, path-qualified, wrapped forms); `agent-runtime` doctor /
  audit clean; `agent-docs audit` and markdown lint for the `AGENT_HOME.md`
  policy + namespace reconciliation; `heuristic-inbox` operation record for the
  `worktree-unsigned-commit-config-drift` promotion decision.
- Cross-cutting: every executed task populates its `Evidence` cell; waived tasks
  are marked `waived` with a reason. The closeout comment is preceded by a final
  `tracking run update --note "<closing summary>"` event.

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | done | `worktree` CLI subtree + deterministic path convention | Added git-cli worktree add/list/remove/prune with deterministic AGENT_HOME worktree paths; validated by cargo test -p nils-git-cli worktree_ and local-fast. | Implemented in nils-cli PR1; Phase 2 hook remains gated. |
| 1.2 | done | `remove` / `prune` + consolidate worktree-removal into one helper | Shared worktree porcelain parsing/removal via git-cli worktree helpers and branch cleanup --remove-worktrees reuse; validated by cargo test -p nils-git-cli worktree_ and cargo test -p nils-git-cli. | plan-issue cleanup-worktrees remains separate by documented issue-root convention. |
| 1.3 | done | JSON contract, completion, and crate docs | Added JSON envelopes, completion coverage, README updates, and crate-local worktree convention spec; validated by completion syntax checks and local-fast. | Generated completion adapters unchanged; completion export coverage verifies the worktree subcommands. |
| 2.1 | done | `block-direct-git-worktree` hook with override escape hatch | PR graysurf/agent-runtime-kit#213 merged `83de974`; validation record shows `bash tests/hooks/run.sh` passed. | Depends on Sprint 1 shipped + brew-upgraded. Mirrored `block-direct-git-commit.py`; blocks mutating `worktree` ops, allows read-only `list`; includes `ALLOW_DIRECT_GIT_WORKTREE=1` override; registered in `settings.hooks.jsonc` + `link-map.yaml`. |
| 2.2 | done | `AGENTS.md` / `AGENT_HOME.md` policy + namespace reconciliation + heuristic promotion | PR graysurf/agent-runtime-kit#213 merged `83de974`; closeout records release v0.31.5 and hook/policy rollout complete. | Depends on 2.1. Policy clause + convention doc; reconciled "`$AGENT_HOME` artifacts only" note for `worktrees/`; heuristic retention handled during closeout. |

## Session Log

- 2026-05-31: Authored this bundle (discussion-source + plan + execution-state)
  for a unified `git-cli worktree` surface plus a direct-`git worktree` ban.
  Feasibility findings: `forge-cli` is remote-only and touches local git
  read-only, so it is the wrong home ([F1]); `git-cli` already groups git
  workflow helpers and already removes worktrees (`branch cleanup
  --remove-worktrees`, `linked_worktrees_by_branch()`), making a `worktree`
  group a natural sibling with no new-crate tax ([F2]); worktree logic is
  already scattered across `git-cli`, `plan-issue cleanup-worktrees` (with
  its own `$ISSUE_ROOT/worktrees/<mode>` convention), and the
  `meta:worktree-triage` skill ([F3]); the hook-ban mechanism is proven and
  global (`block-direct-git-commit.py` / `block-direct-pr-create.py` registered
  in `settings.hooks.jsonc`) ([F4]); `agent-out` owns only `$AGENT_HOME/out/`
  and `$AGENT_HOME/worktrees/` already exists but is empty/unused, so the chosen
  convention formalizes an idle namespace ([F5]); the harness `EnterWorktree`
  path (`.claude/worktrees/`) is a separate, out-of-scope creator a Bash hook
  cannot/should not catch ([F6]); and the open heuristic
  `worktree-unsigned-commit-config-drift` (recurring unsigned-commit / identity
  drift under worktrees) has promotion criterion (c) that this work directly
  targets ([F7]). Decisions locked by user: home = `git-cli worktree` group
  (not `forge-cli`, not a new crate); convention =
  `$AGENT_HOME/worktrees/<repo-key>/<branch-slug>`; sequencing = ship/prove the
  CLI first, then enable the hook ban with an override escape hatch. No
  implementation started; this state is prepared so `create-plan-tracking-issue`
  can open the tracker. Authored in an isolated worktree off `main` to avoid
  disturbing the shared `nils-cli` checkout.
- 2026-05-31: Tracking issue #712 was completed and closed. Lifecycle audit
  found source, plan, state, session, validation, review, and closeout records
  visible and complete. Delivery landed through PR #715 in nils-cli and
  graysurf/agent-runtime-kit#213, followed by nils-cli release v0.31.5.

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `cargo test -p nils-git-cli worktree_` | pass | Targeted failing-first coverage for worktree add/list/remove/prune and branch-cleanup linked-worktree removal. | local |
| `cargo test -p nils-git-cli` | pass | Full `nils-git-cli` test suite passed. | local |
| `cargo clippy -p nils-git-cli --all-targets -- -D warnings` | pass | Crate-specific clippy gate passed after implementation cleanup. | local |
| `cargo fmt -p nils-git-cli -- --check` | pass | Crate-specific formatting gate passed. | local |
| `zsh -n completions/zsh/_git-cli` / `bash -n completions/bash/git-cli` | pass | Existing generated completion adapters remain syntactically valid. | local |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` | pass | Docs audits, third-party artifact audit, workspace fmt/clippy, 4444 nextest tests, and doctests passed. | local |
| `gh pr checks` | pass | Closeout records PR #715 and PR #213 checks as passing. | <https://github.com/sympoies/nils-cli/issues/712#issuecomment-4586272231> |
| hook block/allow/override fixture (Sprint 2) | pass | Sprint 2 validation passed via agent-runtime-kit hook tests. | <https://github.com/graysurf/agent-runtime-kit/pull/213> |

## Closeout

- Status: complete.
- Closed issue:
  <https://github.com/sympoies/nils-cli/issues/712>
- Closeout comment:
  <https://github.com/sympoies/nils-cli/issues/712#issuecomment-4586272231>
- Review decision: approve at
  <https://github.com/sympoies/nils-cli/issues/712#issuecomment-4586270627>.
- Final validation:
  <https://github.com/sympoies/nils-cli/issues/712#issuecomment-4586270587>.

## Notes

- Cross-repo tracker: Phase 1 lands in `sympoies/nils-cli`, Phase 2 in
  `graysurf/agent-runtime-kit`. The issue tracks the whole initiative; the
  Phase 2 hook PR is opened in `agent-runtime-kit` and linked back.
- The Phase 2 hook is enabled globally only after the Phase 1 release is
  brew-upgraded on the host (version-pin gate) and `worktree add` coverage is
  confirmed against real agent usage. The override escape hatch
  (`ALLOW_DIRECT_GIT_WORKTREE=1`) ships from day one so a host without the new
  CLI is never trapped.
- `worktree add` must respect the `worktree-unsigned-commit-config-drift`
  finding: never enable `extensions.worktreeConfig`, never set per-worktree
  identity, and preserve `HOME`/`XDG` so the global signing config stays visible
  inside the new worktree.
