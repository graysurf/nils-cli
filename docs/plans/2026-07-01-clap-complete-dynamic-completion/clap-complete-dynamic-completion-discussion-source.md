# Source: clap_complete CompleteEnv dynamic completion rollout

## Origin

Graduated from the deferred follow-up plan
<https://github.com/sympoies/nils-cli/issues/999> ("Roll out clap_complete
CompleteEnv dynamic completion across CLIs"), whose own body directs: "When
scheduled, graduate this follow-up into a `docs/plans/<date>-<slug>/` bundle via
`create-plan-tracking-issue` -> `execute-plan-tracking-issue` -> closeout." This
document freezes the converged intent, the proven Phase 0 feasibility, and the
tier rationale.

## Motivating gap

`git-cli worktree go <TAB>` can only fall back to filesystem completion today,
because clap's static completion cannot enumerate the live worktree list at TAB
time (`ValueHint::AnyPath` is the best a static model can do). The `gxwcd` shell
helper works around this for one alias, but the general capability — runtime-
computed candidates (worktree names, branches, remotes, tags, live paths) — is
missing across the workspace.

The fix is `clap_complete` **dynamic** completion: `CompleteEnv` +
`engine::ArgValueCandidates`, behind the `unstable-dynamic` feature.

## Phase 0 spike — DONE, feasible

Verified in a throwaway standalone crate pinned to the workspace versions
(`clap =4.6.1` + `clap/unstable-ext`, `clap_complete =4.6.5` +
`unstable-dynamic`), using a builder-style `Command` that mirrors git-cli's
`completion.rs` `build_command_model()` shape (real parsing is hand-rolled; the
clap `Command` is only a completion model):

- Compiles cleanly against the pinned versions. `unstable-dynamic` pulls
  `clap_lex` (already in tree), `shlex 1.3.0`, and `is_executable 1.0.6` (all
  `MIT OR Apache-2.0`).
- Registration emits for zsh and bash via `COMPLETE=<shell> <bin>`. The zsh stub
  is a `compdef _clap_dynamic_completer_<bin>`; the callback re-invokes the
  binary at TAB time with
  `COMPLETE=<shell> _CLAP_COMPLETE_INDEX=<n> <bin> -- <words>`. Registration
  embeds `current_exe()`, so generating it live from the installed binary keeps
  the path correct.
- Dynamic candidates work: an `ArgValueCandidates` closure that shells out to
  `git worktree list --porcelain` returned live worktree basenames + branch
  names at completion time.
- Prefix filtering works: `go ni<TAB>` -> only `nils-cli`; `go ma<TAB>` -> only
  `main`. Subcommand-level completion (`worktree <TAB>` -> `go` with
  description) also works.
- Zero cost when idle: with `COMPLETE` unset, `.complete()` short-circuits and
  the normal hand-rolled app path runs unchanged.

Conclusion: technically sound. The cost is not the clap wiring — it is that
git-cli's completion is governed by workspace-wide audits and a shared runtime
adapter that all assume static `generate()`. A git-cli-only change collides with
that framework, so the framework must learn a first-class "dynamic" mode before
any CLI migrates cleanly.

## Tier rationale — L2 plan tracking

- Committed, multi-step, plan worth freezing; multiple PRs (framework -> pilot
  -> rollout); resumable state across sessions -> above L0/L1.
- Not L3: the phases are strictly sequential (framework must land before the
  pilot, pilot before rollout). Only the final per-CLI rollout phase could
  optionally fan out into parallel lanes; that is a Sprint 3 sub-decision, not
  the shape of the whole effort.

## Converged decisions

- Static `generate()` stays the default; dynamic mode is opt-in per CLI, never
  forced. CLIs with no runtime candidates keep static completion.
- Dynamic mode must be documented as a first-class completion engine that
  extends the clap-first baseline — not an alternate dispatch — so the
  single-completion-path policy and its grep checks do not read it as a
  violation.
- The exact `clap_complete =4.6.5` pin stays; the unstable stub + binary ship
  from one Homebrew tarball so they cannot drift. Revisit on every
  `clap_complete` bump.
- `shlex 1.3.0` is duplicate against the existing `shlex 2.0.1` and cannot
  unify; it is added to the `deny.toml` skip list with an explicit reason
  rather than relaxing `multiple-versions = "deny"`.
- Completion stays invisible to agents; `.complete()` is zero-cost when idle, so
  there is no agent-facing behavior change at any phase.

## Acceptance (whole plan)

- Sprint 1: the framework recognizes `completion_engine = static | dynamic`; a
  synthetic dynamic fixture passes freshness/flag-parity/asset audits and the
  zsh registration + alias test; existing static CLIs are unaffected;
  `cargo deny check` passes with the shlex skip.
- Sprint 2: git-cli completes `worktree go`/`remove` targets from the live
  worktree list; full CI green; released patch verified on a real zsh + bash
  install including alias completion.
- Sprint 3: per-CLI opt-in providers added where they help, migrated CLI-by-CLI
  with per-CLI audit + release validation; unmigrated CLIs byte-identical.

## References

- Phase 0 spike (throwaway, not committed): standalone crate reproducing
  `CompleteEnv` + `ArgValueCandidates` against pinned workspace versions.
- Prior worktree ergonomics work: #995 (`worktree go`, `gxwcd`) and its 1.20.3
  autoload completion fix (#997).
- Completion framework: `docs/runbooks/cli-completion-development-standard.md`,
  `docs/specs/completion-coverage-matrix-v1.md`,
  `docs/specs/completion-contract-template.md`, the three
  `scripts/ci/completion-*-audit.sh`, the shared
  `completions/{zsh,bash}/*completion-adapter-common*`, and
  `tests/zsh/completion.test.zsh`.
