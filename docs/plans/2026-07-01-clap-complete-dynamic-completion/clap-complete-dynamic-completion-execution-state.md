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
| 1.3 | deferred | Teach completion-flag-parity-audit the dynamic mode | Sprint 2 (pending) | Reassigned to Sprint 2: flag-parity has no --root/test harness, so its dynamic assertion is tested against git-cli's real dynamic asset, not a synthetic fixture. Inert until a CLI is dynamic. |
| 1.4 | done | Teach freshness + asset audits the dynamic mode | d557152 freshness audit dynamic skip + test RED->GREEN | asset-audit needs no change (content-agnostic: dynamic stub satisfies present/format checks); flag-parity dynamic assert reassigned to 1.3 |
| 1.5 | deferred | Extend shared runtime adapter for CompleteEnv + alias rewrite | Sprint 2 (pending) | Reassigned to Sprint 2: shared adapter dynamic helpers land with git-cli's real CompleteEnv asset. |
| 1.6 | deferred | Cover dynamic registration shape in zsh completion test | Sprint 2 (pending) | Reassigned to Sprint 2: zsh completion-test dynamic branch exercised against git-cli's real dynamic asset. |
| 1.7 | done | Document dynamic mode in completion development standard | d557152 completion development standard: dynamic engine subsection + tuple key | Reconcile with single-completion-path policy; name `completion_engine`. |
| 2.1 | pending | Wire CompleteEnv into git-cli main dispatch | pending | Gated on released Sprint 1. |
| 2.2 | pending | Attach live worktree candidates to worktree go/remove | pending | `ArgValueCandidates` over `git worktree list --porcelain`. |
| 2.3 | pending | Emit CompleteEnv registration + update assets and aliases | pending | Regenerate committed assets; gx*/gxw wiring; keep `gxwcd` ergonomics. |
| 2.4 | pending | Full CI, release patch, real-install verification | pending | test / test_macos / coverage; zsh + bash real install. |
| 3.1 | pending | Inventory + add per-CLI dynamic value providers | pending | Gated on released Sprint 2; branches/remotes/tags/worktrees/paths. |
| 3.2 | pending | Migrate CLI-by-CLI with per-CLI audit + release validation | pending | Opt-in per CLI; static stays for CLIs w/o runtime candidates. |

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
