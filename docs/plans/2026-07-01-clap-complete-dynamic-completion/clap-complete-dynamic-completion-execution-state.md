# clap_complete CompleteEnv Dynamic Completion Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: active; Sprint 1 (framework dynamic mode) delivered on
  `feat/completion-engine-dynamic-mode` (`d557152`), PR pending; Sprints 2-3
  gated on a nils-cli release carrying Sprint 1.
- Target scope: teach the completion framework a `completion_engine = static |
  dynamic` dimension (Sprint 1), pilot git-cli's migration to `CompleteEnv`
  (Sprint 2), then roll out per-CLI opt-in dynamic value providers (Sprint 3).
- Execution window: Sprint 1 (framework) -> Sprint 2 (git-cli pilot + release)
  -> Sprint 3 (per-CLI rollout), strictly serial; Sprint 2 is gated on a
  released Sprint 1, Sprint 3 on a released Sprint 2.
- Current task: Sprint 1 — framework learns the dynamic completion engine mode.
- Next task: Sprint 2 — git-cli pilot, gated on a nils-cli release carrying
  Sprint 1.
- Last updated: 2026-07-01
- Branch/commit/PR: `feat/completion-engine-dynamic-mode` (Sprint 1 PR pending).
- Source document:
  `docs/plans/2026-07-01-clap-complete-dynamic-completion/clap-complete-dynamic-completion-discussion-source.md`
- Plan document:
  `docs/plans/2026-07-01-clap-complete-dynamic-completion/clap-complete-dynamic-completion-plan.md`
- Direct source-doc execution waiver: not applicable.
- Tracking issue: <https://github.com/sympoies/nils-cli/issues/999>
- Source snapshot:
  <https://github.com/sympoies/nils-cli/issues/999#issuecomment-4850473605>
- Plan snapshot:
  <https://github.com/sympoies/nils-cli/issues/999#issuecomment-4850473807>
- Initial state snapshot:
  <https://github.com/sympoies/nils-cli/issues/999#issuecomment-4850473978>

## Validation Plan

- Bundle creation: validate the plan bundle before graduating the tracker.
- Tracker graduation: dry-run `plan-issue record attach --issue 999`, then live
  attach only if the source/plan/state comments and dashboard are correct;
  reconcile #999 labels to the tracking taxonomy.
- Initial read-back: audit the live issue with `record audit --profile tracking
  --expect-visible`.
- Sprint 1: test-first RED against the current audits (a synthetic dynamic
  fixture must be mis-handled today), then GREEN after the framework learns the
  dynamic mode; run the three completion audits, the zsh completion test,
  `cargo deny check`, and `--local-fast`; docs-only validation for the doc and
  matrix changes.
- Sprint 2: git-cli `cargo test`, full CI (test / test_macos / coverage),
  completion audits under dynamic mode, manual real-install TAB verification,
  release validation.
- Sprint 3: per-CLI `cargo test`, completion audits, manual TAB verification,
  and release validation per migrated CLI.

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | done | Add completion_engine dimension to matrix + contract metadata | d557152 matrix policy note + contract-template metadata tuple/enforcement row | Coverage matrix column + contract-template metadata tuple; all existing rows `static`. |
| 1.2 | done | Enable clap unstable features + gate dependency footprint | d557152 clap unstable-ext + clap_complete unstable-dynamic; deny.toml shlex@1.3.0 + windows-sys@0.60.2 skips; THIRD_PARTY regenerated; cargo deny green; 0 generate() drift | `clap/unstable-ext` + `clap_complete/unstable-dynamic`; `deny.toml` shlex@1.3.0 skip; confirm is_executable license. |
| 1.3 | done | Teach completion-flag-parity-audit the dynamic mode | Sprint 2 (pending); 27b3973 flag-parity skips completion_engine=dynamic CLIs (dynamic_engine_skipped counted); exercised against git-cli real dynamic asset | Landed with Sprint 2 pilot. |
| 1.4 | done | Teach freshness + asset audits the dynamic mode | d557152 freshness audit dynamic skip + test RED->GREEN | asset-audit needs no change (content-agnostic: dynamic stub satisfies present/format checks); flag-parity dynamic assert reassigned to 1.3 |
| 1.5 | done | Extend shared runtime adapter for CompleteEnv + alias rewrite | Sprint 2 (pending); 27b3973 shared adapter _nils_cli_completion_common_load_dynamic_{zsh,bash}; alias rewrite + fail-closed preserved | Landed with Sprint 2 pilot. |
| 1.6 | done | Cover dynamic registration shape in zsh completion test | Sprint 2 (pending); 27b3973 zsh completion test dynamic-shape assertion + fixed padding-intolerant row selector (now exercises all 46 required rows) | Landed with Sprint 2 pilot; review-caught selector bug fixed. |
| 1.7 | done | Document dynamic mode in completion development standard | d557152 completion development standard: dynamic engine subsection + tuple key | Reconcile with single-completion-path policy; name `completion_engine`. |
| 2.1 | done | Wire CompleteEnv into git-cli main dispatch | pending; 27b3973 CompleteEnv::with_factory(build_command_model).complete() in run(); idle path (COMPLETE unset) unchanged; verified via integration tests | Wired; idle no-op verified. |
| 2.2 | done | Attach live worktree candidates to worktree go/remove | pending; 27b3973 ArgValueCandidates over live git worktree list on worktree go/remove targets; empirical smoke returns live slugs+branches | Live candidates verified. |
| 2.3 | done | Emit CompleteEnv registration + update assets and aliases | pending; 27b3973 emit CompleteEnv registration stub; committed zsh/bash assets load via dynamic adapter; gxw->worktree alias; gxwcd note | Assets + aliases reconciled. |
| 2.4 | done | Full CI, release patch, real-install verification | pending; Released nils-cli v1.20.5 (PR #1003, tag v1.20.5, 8 tarballs, Homebrew tap 1.20.4->1.20.5); verified shipped dynamic completion on real install (COMPLETE=zsh git-cli -- worktree go returns live candidates) | Sprint 2 pilot released + install-verified. |
| 3.1 | done | Inventory + add per-CLI dynamic value providers | pending; Inventoried all workspace CLIs; added ArgValueCandidates to agent-memory (scopes) + secrets (store entries). Recorded stay-static set (plan-issue/api-*/agent-docs/etc.) and deferred external-dep CLIs (docker-tools daemon, forge-cli API cost, git-lock raw-args, git-scope git-refs, git-summary no-candidates) | Providers added only where local/fast/fail-soft + valuable. |
| 3.2 | done | Migrate CLI-by-CLI with per-CLI audit + release validation | pending; Migrated agent-memory + secrets to completion_engine=dynamic (derive #[arg(add=..)] + CompleteEnv); per-CLI audits pass (freshness dynamic_engine_skipped=6, flag-parity=3, asset); release validation via v1.20.6. Remaining candidates (docker-tools/forge-cli/git-lock/git-scope) deferred as documented follow-ups; git-summary stays static | Opt-in per CLI; clear local-candidate wins migrated, external-dep/marginal ones deferred with rationale. |

## Session Log

- 2026-07-01: Graduated the deferred follow-up #999 into this L2 plan bundle.
  Confirmed ground truth via parallel investigation: no prior bundle/run-state;
  the completion framework (three CI audits + shared adapter + zsh test +
  coverage matrix + contract template + development standard) all assume static
  `generate()`; git-cli's `build_command_model()` (completion.rs) and hand-rolled
  `run_from()` dispatch (cli.rs) are the pilot's touch points; workspace pins are
  `clap "4"` (-> 4.6.1) and `clap_complete =4.6.5` with no unstable features yet,
  and `shlex 2.0.1` is already in tree so `unstable-dynamic`'s `shlex ^1` needs a
  `deny.toml` skip. Assembled and validated the bundle; delivering Sprint 1 as
  the first PR (Sprints 2-3 gated on release, tracked here).
- 2026-07-01: Delivered Sprint 1 as `feat(completion)` commit `d557152`.
  Empirically verified that enabling `unstable-ext`/`unstable-dynamic` does not
  change `clap_complete::generate()` output (0 freshness drift across 46 CLIs),
  so no static asset regen was needed; the new deps surfaced a `windows-sys
  0.60` duplicate (via `is_executable`) skip-listed alongside `shlex 1.3.0`, and
  THIRD_PARTY artifacts were regenerated. Scope decision: only the freshness
  audit gained dynamic handling this sprint (it has a `--root` test harness, so
  a synthetic dynamic fixture gives a real test-first RED->GREEN); flag-parity
  (1.3), the shared adapter (1.5), and the zsh completion test (1.6) are
  reassigned to Sprint 2 so they are exercised against git-cli's real dynamic
  asset instead of speculative synthetic fixtures. Every current CLI stays
  `static`, so all audit/adapter paths are unchanged and nothing half-breaks.
- 2026-07-01: Released Sprint 1 as nils-cli `v1.20.4` (PR #1001, tag `v1.20.4`,
  8 release tarballs, Homebrew tap updated). Delivered Sprint 2 (git-cli pilot)
  as `feat(git-cli)` commit `27b3973`, PR #1002: `CompleteEnv` +
  `ArgValueCandidates` so `worktree go`/`remove` complete live worktree
  names/branches (superseding the static `gxwcd` workaround), plus the deferred
  framework dynamic-handling (flag-parity dynamic skip 1.3, shared adapter
  dynamic loader 1.5, zsh completion-test dynamic branch 1.6). Marked git-cli
  `completion_engine=dynamic`. Test-first RED->GREEN on the dynamic-registration
  integration tests; validated end-to-end (`COMPLETE=zsh git-cli -- ...` returns
  live candidates; bash e2e incl. the `gxw` alias). A 3-lens adversarial review
  caught a pre-existing padding-intolerant row selector in the zsh completion
  test (it silently exercised only 1 row); fixed so it now covers all 46
  required rows and the new dynamic assertion fires. Sprint 2's own release
  (2.4) and Sprint 3 remain pending.
- 2026-07-01: Released Sprint 2 as nils-cli `v1.20.5` (PR #1003, tag `v1.20.5`,
  8 tarballs, Homebrew tap 1.20.4->1.20.5); verified the shipped dynamic
  completion on the real install (`COMPLETE=zsh git-cli -- git-cli worktree go`
  returns live worktree candidates). Delivered Sprint 3: migrated `agent-memory`
  (scope args -> live agent/persona/global scopes) and `secrets` (name args ->
  live store entry names) to `completion_engine=dynamic` via the derive
  `#[arg(add = ArgValueCandidates::new(..))]` form + `CompleteEnv`. Scope
  decision: migrated the clear local/fast/fail-soft wins; kept `git-summary`
  static (no runtime candidates) and deferred `docker-tools` (needs a live
  daemon in the completion path), `forge-cli` (per-keystroke API cost), and
  `git-lock`/`git-scope` (raw-arg / git-ref plumbing) as documented follow-ups.
  Two 3-lens adversarial reviews (Rust+security, shell+audits+docs): security
  confirmed the secrets completer emits entry *names* only (never values); one
  finding fixed — the zsh completion test's tab-collapsing `read` silently
  skipped the dynamic assertion for no-alias dynamic CLIs, now split
  empty-preservingly so it covers agent-memory + secrets (negative-control
  verified). Sprint 3 release (v1.20.6) + closeout pending.

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `plan-tooling validate --file docs/plans/2026-07-01-clap-complete-dynamic-completion/clap-complete-dynamic-completion-plan.md --format text --explain` | pass | Plan Format v1 clean; 0 errors. | local |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` | pass | Docs placement, hygiene, markdown lint, plan-bundle all pass. | local |
| `plan-issue --repo sympoies/nils-cli --format json --dry-run record attach --issue 999 --profile tracking ...` | pass | Dry-run rendered source/plan/state comments; repo-relative paths only; issue 999. | local |
| `plan-issue --repo sympoies/nils-cli --format json record attach --issue 999 --profile tracking ...` | pass | Live attach posted source/plan/state and rendered the tracking dashboard. | <https://github.com/sympoies/nils-cli/issues/999> |
| `forge-cli issue edit 999 --add-label workflow::plan --add-label workflow::tracking --add-label state::ready --remove-label workflow::follow-up --remove-label state::needs-triage` | pass | Reconciled #999 to the tracking taxonomy. | <https://github.com/sympoies/nils-cli/issues/999> |
| `bash scripts/ci/tests/completion-freshness-audit.test.sh` (vs unmodified audit) | pass | Test-first RED: synthetic `completion_engine=dynamic` fixture falsely flagged stale (exit 1). | local |
| `bash scripts/ci/tests/completion-freshness-audit.test.sh` (vs modified audit) | pass | GREEN: dynamic skip + asset-existence + static-staleness assertions pass (exit 0). | local |
| `bash scripts/ci/completion-freshness-audit.sh --strict` | pass | required=46, dynamic_engine_skipped=0, 0 drift with unstable features enabled. | local |
| `bash scripts/ci/completion-flag-parity-audit.sh --strict` | pass | required=46, 0 failures. | local |
| `bash scripts/ci/completion-asset-audit.sh --strict` | pass | 48 workspace bins, 46 required, 2 excluded, 0 errors. | local |
| `bash scripts/ci/cargo-deny-audit.sh` | pass | advisories ok, bans ok (shlex@1.3.0 + windows-sys@0.60.2 skips). | local |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` | pass | Docs + workspace Rust gate green after THIRD_PARTY regen. | local |
