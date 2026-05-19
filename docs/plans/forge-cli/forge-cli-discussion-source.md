# forge-cli v1 — Discussion → Implementation Source

## Purpose

Captures the converged decisions handed off to dispatch planning for
`forge-cli` v1. Pairs with the authoritative contract at
`docs/specs/forge-cli-spec-v1.md` and the machine-readable op catalog at
`docs/specs/forge-cli-ops-v1.yaml`. The plan at
`docs/plans/forge-cli/forge-cli-plan.md` consumes this doc as its
primary source.

## Locked decisions (carry into execution)

1. **Backend strategy**: subprocess wrap `gh` and `glab`. No direct
   REST client. Auth, SSO, and enterprise hosts come from the user's
   existing `gh auth` / `glab auth` state.
2. **v1 scope**: PR/MR lifecycle + Issue lifecycle + CI wait. Releases,
   labels, raw `gh api` / `glab api` passthrough, repo creation,
   branch protection, and issue macros are explicitly v2 or later.
3. **Exit codes**: only the six sysexits constants from
   `nils_common::cli_contract::exit` (`SUCCESS`, `RUNTIME`, `USAGE`,
   `DATA`, `UNAVAILABLE`, `SOFTWARE`). Numeric exit literals are
   forbidden anywhere in the binary. Policy violations all map to
   `DATA 65`; the discriminator lives in `data.error.kind`.
4. **Provider detection precedence**: `--provider` flag > `git remote
   get-url <--remote>` host parse > `gh/glab auth status` host match.
   Unknown host → `USAGE 64` with `error.kind = "provider_unsupported"`.
   No silent fallback to a third provider in v1.
5. **Macro `pr deliver`** = `auth.status` → `repo.view` → `pr.create` →
   `pr.wait-checks` → `pr.ready` → `pr.merge`. Macro failure does NOT
   remap the inner exit code; callers branch on `data.steps[]` to
   identify which step failed.
6. **`glab ci status` text parser scope**: pin parser to the currently
   installed `glab` minor. Out-of-range versions trigger
   `UNAVAILABLE 69` with `error.kind = "glab_version_unsupported"` and
   a "please upgrade/downgrade glab" hint. No best-effort parse across
   versions.
7. **Wrapper + Homebrew tap formula** ship in this v1 (Sprint 8).
8. **Release vehicle**: cut a `nils-cli` minor bump once Sprint 8
   passes the acceptance gate, via
   `nils-cli-bump-version-tag-release` + tap formula bump.
9. **Contract baseline**: `forge-cli` adopts `cli-output-contract-v1`
   from day one. No `--json` boolean alias. `--format text|json` only.
   Snake_case envelope throughout. Schema literals follow
   `cli.forge-cli.<op>.v1`.
10. **Sprint cadence**: each sprint = one GitHub PR, cut from `main`
    via `create-feature-pr` → `close-feature-pr`. Sprint 0 (this PR)
    lands the spec + plan + this discussion source. Subsequent sprints
    rebase onto `main` after Sprint 0 merges.
11. **agent-kit migration deferral**: the migration from agent-kit
    skills' direct `gh` / `glab` invocations to `forge-cli` happens
    after v1 ships and is accepted by the user; it is NOT part of
    this plan.

## Acceptance gate (v1 done definition)

Every row in spec §"Migration plan: agent-kit skills → forge-cli" is
reachable through `forge-cli`. Parity test passes (envelope
byte-identical between backends except `data.provider` and URL host).
Exit-code matrix test covers all six sysexits paths. `--dry-run` works
for every op. `scripts/ci/cli-output-contract-lint.sh` passes.

## Read-first companions

- Contract: `docs/specs/forge-cli-spec-v1.md`
- Op catalog: `docs/specs/forge-cli-ops-v1.yaml`
- Workspace envelope contract: `docs/specs/cli-output-contract-v1.md`
- Crate layout precedent: `crates/git-cli/`
- Workspace rules: `AGENTS.md`, `DEVELOPMENT.md`,
  `docs/runbooks/new-cli-crate-development-standard.md`,
  `docs/runbooks/cli-completion-development-standard.md`

## Execution

- Recommended plan: docs/plans/forge-cli/forge-cli-plan.md
- Recommended execution state: docs/plans/forge-cli/forge-cli-execution-state.md
- Plan shape: dispatch-ready, 8 sprints (Sprint 0 lands docs; Sprints
  1–8 implement atoms → macro → tests → release).
- PR cadence: one PR per sprint, cut from `main`.
- Per-task complexity required.
- Execution state cadence: after each sprint PR merges, record the
  sprint outcome plus the acceptance-gate row(s) it cleared. Final
  state: all 8 sprints `completed`, every acceptance-gate row in spec
  §"Migration plan: agent-kit skills → forge-cli" marked reachable.

## Open questions deliberately NOT answered here

- Whether agent-kit skill migration lands in `nils-cli` workspace or
  in `agent-kit` (the latter, per current ownership). Tracked as a
  follow-up after v1 ships.
- Whether `gh api` / `glab api` passthrough returns in v2. Spec
  documents the deferral and the re-evaluation criterion (a real
  workflow that needs a non-CRUD call).
