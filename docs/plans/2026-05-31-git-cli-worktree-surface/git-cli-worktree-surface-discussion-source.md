# Unified `git-cli worktree` Surface + Direct-`git worktree` Ban — Implementation Handoff

- Status: decisions settled; ready for plan generation.
- Date: 2026-05-31
- Source: a feasibility discussion on making git worktrees the primary
  agent-isolation workflow without two recurring pains — (1) raw `git worktree`
  is awkward (a branch can be checked out in only one worktree, so agents fight
  the "one branch" rule) and (2) agents scatter worktree folders in arbitrary
  locations because no convention is enforced. The proposed shape mirrors how
  this runtime kit already governs `git commit` and `gh pr create`: one
  sanctioned CLI surface plus a hook that forbids the raw command.
- Intended next step: open an L2 plan-tracking issue from this bundle. This is a
  source artifact, not an implementation plan.

## Execution

- Recommended plan: docs/plans/2026-05-31-git-cli-worktree-surface/git-cli-worktree-surface-plan.md
- Recommended execution state: docs/plans/2026-05-31-git-cli-worktree-surface/git-cli-worktree-surface-execution-state.md
- Status: decisions settled; plan generation is the next step.
- Next-task source: this document

## Purpose

Make `git worktree` the primary agent-isolation workflow ergonomic and
governed. Two user pains drive it:

- **P1 — raw worktree CLI is awkward / "only one main branch".** Git forbids
  checking out the same branch in two worktrees. Agents repeatedly hit this.
  The fix is a wrapper whose `add` always creates a fresh branch, so the
  one-branch-per-worktree rule becomes invisible — which is also the correct
  isolation model.
- **P2 — agents scatter worktree folders.** No placement convention is
  enforced, so every agent invents its own location. The fix is a CLI that
  computes the path deterministically; the agent never picks it.

The chosen mechanism is the kit's existing governance pattern, applied to
worktrees: a single sanctioned CLI surface (`git-cli worktree`) plus a
PreToolUse hook that blocks the raw mutating `git worktree` command, with an
explicit override escape hatch and an `AGENTS.md` clause.

## Confirmed Facts (current behaviour)

- [F1] `forge-cli` is scoped to **remote, provider-neutral** forge operations
  (PR/MR, issue, label, inbox via `gh`/`glab` subprocess wrappers) and touches
  local git **read-only** for validation (`worktree_clean`,
  `git_status_porcelain` in `crates/forge-cli/src/validations.rs`). Its
  README/spec explicitly state "thin wrapper, every action delegates to
  `gh`/`glab`". Local-git *mutation* is outside its boundary → it is **not** the
  home for worktree management.
- [F2] `git-cli` is the dispatcher for local git workflow helpers
  (`utils/reset/commit/branch/ci/open`) and **already touches worktrees**:
  `branch cleanup --remove-worktrees` and `linked_worktrees_by_branch()`
  (`crates/git-cli/src/branch.rs`) parse `git worktree list --porcelain` and run
  `git worktree remove --force`. A `worktree` group is a natural sibling of
  `branch` and reuses existing git plumbing — no new-crate tax.
- [F3] Worktree logic is already scattered across three places:
  `git-cli branch cleanup --remove-worktrees`; `plan-issue-cli cleanup-worktrees`
  (`crates/plan-issue-cli/src/execute.rs`), which carries its **own** convention
  `$ISSUE_ROOT/worktrees/<mode>/<id>` for the dispatch flow
  (`plan-issue-cli-contract-v2.md`); and the `meta:worktree-triage` skill. A new
  surface must consolidate or layer above these, not become a fourth scatter
  point / third removal code path.
- [F4] The hook-ban mechanism is proven and global:
  `core/hooks/shared/block-direct-git-commit.py` and `block-direct-pr-create.py`
  in `agent-runtime-kit`, registered in `core/hooks/claude/settings.hooks.jsonc`
  (PreToolUse, `matcher: "Bash"`), synced into `~/.claude/settings.json`. They
  parse the command string (handling env prefixes, path-qualified git, wrapper
  commands) via a `git_subcommand()` helper and `emit_block()` with a reason.
  Adding `block-direct-git-worktree.py` (or extending the commit detector) is a
  well-trodden, low-risk change applied globally — not per-repo.
- [F5] `AGENT_HOME=~/.local/state/agent-runtime-kit`. `agent-out` owns
  `$AGENT_HOME/out/` ("Generate canonical project-scoped AGENT_HOME/out run
  directories"). `$AGENT_HOME/worktrees/` **already exists but is empty and used
  by no convention** (created 2026-05-23). So the chosen convention formalizes an
  already-reserved-but-idle namespace rather than inventing one; it sits beside
  `out/`, not under it.
- [F6] The Claude Code harness has its own `EnterWorktree`/`ExitWorktree` tools
  that create worktrees under `<repo>/.claude/worktrees/` (a `CLAUDE_BASE`
  marker, per the heuristic entry below). That path is **not** a Bash
  `git worktree` call, so a Bash hook neither catches nor should catch it. The
  ban disciplines the shell-out path only; the harness tool is a second,
  out-of-scope creator. Coexistence must be stated, not unified.
- [F7] Heuristic entry `worktree-unsigned-commit-config-drift` (open, medium):
  agent commits recur **unsigned** on `main` specifically under worktree
  workflows, and agents have changed `user.email` in those contexts.
  Promotion criterion (c) is "a durable policy/hook prevents per-worktree
  identity/signing drift (e.g. an `AGENTS.md` rule + a worktree-config drift
  audit wired into `pre-push`)". A controlled `worktree add` (never enabling
  `extensions.worktreeConfig`, never setting per-worktree identity, preserving
  `HOME`/`XDG` so global signing config stays visible) + the `AGENTS.md` clause
  in this plan directly target that criterion, so this work can promote/close
  that finding.

## Decisions (locked by user)

- **Home**: extend the existing `git-cli` crate with a `worktree` subcommand
  group. **Not** `forge-cli` (wrong domain, [F1]); **not** a new crate (avoids
  the 4 new-crate CI gates + a second brew binary + version-pin churn).
- **Convention**: `$AGENT_HOME/worktrees/<repo-key>/<branch-slug>`. `<repo-key>`
  isolates repos (and same-named repos) via toplevel basename + a short stable
  hash; `<branch-slug>` derives from the branch. Centralized, not in-repo and
  not a sibling dir.
- **Sequencing**: two phases. Phase 1 ships the CLI and proves coverage against
  real agent usage; Phase 2 adds the hook ban **only after** Phase 1 is released
  and brew-upgraded everywhere, and the hook ships **with** an override escape
  hatch so a machine without the new CLI is never trapped.

## Open Questions Carried Into Execution

- Exact `<repo-key>` hash basis — short hash of the absolute toplevel path vs
  the primary remote URL. Pick at implementation; both give stable, collision-
  resistant keys. Toplevel path is always available; remote URL is more
  portable across clones. Lean: toplevel path (always present, deterministic).
- Whether the hook blocks read-only `git worktree list`/`--porcelain` or only
  the mutating subcommands (`add`/`remove`/`move`/`prune`/`repair`/`lock`). Lean:
  block mutation only; let reads through.
- Whether `plan-issue-cli cleanup-worktrees` is folded into the shared removal
  helper (cross-crate share) or kept separate with a documented divergence if
  cross-crate sharing is disproportionate. Decide at implementation; at minimum
  `git-cli`'s own two paths (`worktree` group + `branch cleanup`) must share one
  helper.
- Coexistence with the harness `EnterWorktree` path (`.claude/worktrees/`): the
  ban targets Bash mutations only; whether `git-cli worktree list` should also
  surface harness-created worktrees is a nice-to-have, not required for v1.

## Constraints

- This is **cross-repo**: Phase 1 lands in `sympoies/nils-cli` (`git-cli`);
  Phase 2 lands in `graysurf/agent-runtime-kit` (hook + `AGENT_HOME.md` policy).
  The tracking issue lives in `sympoies/nils-cli` and tracks the whole
  initiative; the Phase 2 hook PR is opened in `agent-runtime-kit`.
- The path convention must be verified against `agent-out`'s actual reserved
  paths before it is finalized (confirmed [F5]: `agent-out` owns only `out/`).
- The signing-drift risk [F7] must be actively respected by `worktree add`, not
  merely documented.

## Retention Intent

Plan-source coordination document. Cleanup-eligible after the `git-cli
worktree` surface and the Phase 2 hook ship and the tracker closes and
archives.
