# plan-issue-cli → plan-issue Rename (nils-cli 1.0.0) — Implementation Handoff

- Status: ready for implementation, not started
- Date: 2026-06-01
- Source: discussion-to-implementation-doc (converged session evaluation)
- Intended next step: execute Phase 1 (nils-cli rename PR) when scheduled
- Tracking vehicle: this document + plain PRs. NOT the plan-tracking-issue
  workflow (see decision D5).

## Purpose

Capture the converged decision and the phased, cross-repo plan to rename the
`plan-issue-cli` crate to `plan-issue` in `sympoies/nils-cli` — including its
JSON output contract namespace — with no backward compatibility, shipped as the
`nils-cli` 1.0.0 milestone. This is the read-first source for the implementer so
the settled decisions are not re-litigated.

## Confirmed facts

- `plan-issue-cli` today: dir `crates/plan-issue-cli`, package
  `nils-plan-issue-cli`, lib `plan_issue_cli`, binaries `plan-issue` and
  `plan-issue-local` (both already `-cli`-free).
- Its JSON output contract is namespaced `plan-issue-cli.*`, generated in
  `crates/plan-issue-cli/src/commands/mod.rs` via
  `format!("plan-issue-cli.{}.{suffix}")`. Live schema ids include
  `plan-issue-cli.start.plan.v2`, `plan-issue-cli.status.plan.v2`,
  `plan-issue-cli.record.post.v2`, `plan-issue-cli.record.audit.v2`,
  `plan-issue-cli.close.plan.v1`, `plan-issue-cli.tracking.status.v1`, etc.
- The only external consumer of that contract is `graysurf/agent-runtime-kit`:
  the plan-tracking skills, the test-plan-tracking driver, and
  `tests/runtime-smoke/cases/{dispatch,pr}/run.sh` (which `grep` for
  `plan-issue-cli.record.*`). The kit invokes the binary as `plan-issue` (≈676
  call sites), not `plan-issue-cli`. `plan-tracking-testbed*` repos hold zero
  schema-namespace references (driven by the kit's driver).
- `graysurf/agent-plan-archive` has 71 files mentioning `plan-issue-cli` — mostly
  plan folder names (plans that were about the crate) and historical content;
  only 2 mention the `plan-issue-cli.*` schema. `catalog.json`
  (`plan-archive.catalog.v1`) is keyed by repo / folder / ref and does not parse
  the crate name or contract; `plan-archive query`/`search` is metadata plus
  full-text over immutable snapshots.
- `nils-cli` uses lockstep workspace versioning (currently 0.31.8); a release
  bumps all ~35 crates together.
- `scripts/ci/crate-naming-audit.sh` allowlists `plan-issue-cli` today
  (`allowed_bins_for: plan-issue plan-issue-local`); the convention is
  package `nils-<dir>` and `bin == dir`.
- Sibling renames already merged: `agent-runtime` (#727 package, #728 dir+lib),
  `memo` (#731 full incl. its `cli.memo-cli.*` → `cli.memo.*` contract), and the
  stale `agent-runtime-cli` audit entries removed (#732).

## Decisions

- D1: Rename `plan-issue-cli` → `plan-issue` in every layer — dir, package
  (`nils-plan-issue-cli` → `nils-plan-issue`), lib (`plan_issue_cli` →
  `plan_issue`), and the JSON contract namespace (`plan-issue-cli.*` →
  `plan-issue.*`).
- D2: No backward compatibility — no dual-namespace shim, no contract alias.
  Forward-correct only. Old `plan-issue-cli.*` records in archives stay as
  historical text.
- D3: Ship as the `nils-cli` 1.0.0 milestone. Lockstep means the whole workspace
  moves to 1.0.0; 1.0.0 is the "naming-convention finalized, no back-compat"
  marker.
- D4: Binaries keep `plan-issue` and `plan-issue-local`; the crate-naming-audit
  allowlist entry changes from `plan-issue-cli) echo "plan-issue plan-issue-local"`
  to `plan-issue) echo "plan-issue-local"` (now `plan-issue == dir`; only
  `plan-issue-local` still needs the allowlist), mirrored in the spec.
- D5: Execution will NOT use the plan-tracking-issue machinery
  (`create`/`execute`/`deliver-plan-tracking-issue`). That tooling runs on
  `plan-issue-cli` and emits/parses the very `plan-issue-cli.*` contract being
  changed; tracking this work with it would have the closeout read its own prior
  checkpoints across a namespace break. Track via this doc and plain PRs.
- D6: `agent-plan-archive` requires no action (immutable history; queries
  unaffected — they are metadata + full-text, not schema-validated).

## Scope

- `sympoies/nils-cli`: full crate rename + contract-namespace change + 1.0.0
  release.
- `graysurf/agent-runtime-kit`: migrate the sole contract consumer
  (`plan-issue-cli.*` → `plan-issue.*`) and the pinned-surface refresh, atomically
  with the pin bump to 1.0.0.
- `sympoies/nils-alfredworkflow`: the deferred memo consumer migration (#174)
  folds into the same 1.0.0 release wave, since `nils-memo` publishes at 1.0.0.

## Non-scope

- No back-compat shim, no `plan-issue-cli.*` aliasing.
- No behavior/command-surface change beyond names and the contract namespace.
- No retroactive edits to archived `plan-issue-cli.*` records.
- No use of the plan-tracking-issue / dispatch lifecycle to execute this work.

## Implementation boundaries — phased plan (hard ordering)

### Phase 0 — Pre-flight drain

- Confirm there are no in-flight plan-tracking issues (open trackers mid
  lifecycle). Finish or close them first; they would straddle the contract change
  and break their own closeout.

### Phase 1 — nils-cli rename PR (at current version)

- `git mv crates/plan-issue-cli crates/plan-issue`.
- Rename package `nils-plan-issue-cli` → `nils-plan-issue`; lib
  `plan_issue_cli` → `plan_issue`.
- Change the contract namespace `plan-issue-cli.*` → `plan-issue.*` everywhere:
  the `format!` generator in `src/commands/mod.rs`, every `schema_version`
  string, all asserting tests, and the contract spec
  `docs/specs/plan-issue-cli-contract-v2.md` → `plan-issue-contract-v2.md`
  (and the contract id inside it).
- Update `scripts/ci/crate-naming-audit.sh` + `crate-cli-naming-convention-v1.md`
  per D4.
- Update `release/crates-io-publish-order.txt` (`nils-plan-issue-cli` →
  `nils-plan-issue`), `wrappers/`, and any path references. Binary names are
  unchanged, so checked-in completion assets do not move.
- Regenerate `Cargo.lock` and `THIRD_PARTY_{LICENSES,NOTICES}.md`.
- Use targeted, guarded replacements with a substring-collision check (see R1)
  — `plan-issue-cli` is a substring of `plan-issue-cli-contract`, and
  `plan_issue_cli` must be checked against longer identifiers — before commit.

### Phase 2 — nils-cli 1.0.0 release

- Lockstep bump the whole workspace to 1.0.0 via the standard release flow
  (`project-bump-version-tag-release`): bump → PR → tag → `release.yml` →
  homebrew-tap formula bump → `brew upgrade` (~20-40 min; background it).
- This publishes `nils-plan-issue` (and `nils-memo`, etc.) to crates.io at
  1.0.0.

### Phase 3 — agent-runtime-kit atomic migration PR

- One PR that does all of the following together (must not be split):
  1. Bump the pinned `nils-cli` surface to 1.0.0 (`meta:nils-cli-bump`).
  2. Migrate `plan-issue-cli.*` → `plan-issue.*` in the plan-tracking skills, the
     test-plan-tracking driver, and `tests/runtime-smoke/cases/{dispatch,pr}/run.sh`.
  3. Refresh the surface docs for agent-runtime / memo / plan-issue.
  4. Bump the version-pin to 1.0.0 (EXACT-match pre-push gate).
- Atomicity is required because the EXACT-match pin and the contract-matching
  greps must move in the same commit, or pushes are blocked / smoke tests fail.

### Phase 4 — consumer cleanup

- `nils-alfredworkflow` #174 (memo consumer migration to `nils-memo`) — actionable
  once 1.0.0 publishes `nils-memo`; fold into this wave.
- `agent-plan-archive` — no action (D6).

## Acceptance criteria

- `nils-cli`: `git grep "plan-issue-cli\|plan_issue_cli"` returns zero matches
  (no frozen leftovers — there is no back-compat); `bash
  scripts/ci/crate-naming-audit.sh` → OK; `bash
  scripts/ci/nils-cli-checks-entrypoint.sh --local-fast --base origin/main` →
  pass; contract tests assert `plan-issue.*`.
- `nils-cli` 1.0.0 released; `plan-issue --version` and `nils-plan-issue` on
  crates.io reflect 1.0.0.
- `agent-runtime-kit`: runtime-smoke `dispatch` and `pr` cases pass against
  `nils-cli` 1.0.0; version-pin == 1.0.0; no `plan-issue-cli.*` references remain
  in skills/driver/tests (historical heuristic-inbox archive entries excepted).
- A round-trip plan-tracking lifecycle (create → checkpoint → close) on the
  kit's 1.0.0 driver emits and parses `plan-issue.*` end to end.

## Validation plan

- Per-PR: the `nils-cli` local-fast gate (compile + clippy + tests + markdown
  lint + third-party + crate-naming-audit) and PR CI; the kit's runtime-smoke
  cases.
- Pre-commit substring-collision sweep (R1).
- Markdown lint on this document and any renamed spec docs.

## Risks and guardrails

- R1 — Substring renames (highest of the three crates): `plan-issue-cli` is a
  substring of `plan-issue-cli-contract`, and `plan_issue_cli` could match inside
  longer identifiers (cf. the `agent_runtime_clippy` corruption caught during
  #728). Guard: targeted replacements plus a `grep -nE "plan[_-]issue[_-]cli[a-z]"`
  collision check before commit; verify `cargo fmt` introduces no broken idents.
- R2 — The Phase 3 kit PR must be atomic (pin + contract + smoke greps in one
  commit). Splitting it breaks the EXACT-match pin gate or the smoke tests.
- R3 — In-flight plan-tracking issues straddling the contract change (Phase 0
  drain mitigates).
- R4 — Self-referential tooling: do not track this work with the
  plan-tracking-issue lifecycle (D5).
- R5 — 1.0.0 is workspace-wide (lockstep); confirm the whole-workspace major bump
  is intended, not just `plan-issue`.
- R6 — Release/brew/version-pin sequencing follows the existing `/release` +
  `meta:nils-cli-bump` mechanics; do not hand-edit the agent-runtime-kit surface
  ahead of the release (the pin must match a real released tag).

## Retention intent

- `docs/discussions/` capture. Promote into a `docs/plans/` bundle only if an
  archived plan record is later wanted. Safe to delete after the 1.0.0 wave
  lands, or keep as the 1.0.0 naming-convention decision record.

## Read-first references

- `docs/specs/crate-cli-naming-convention-v1.md`, `scripts/ci/crate-naming-audit.sh`
- `crates/plan-issue-cli/docs/specs/plan-issue-cli-contract-v2.md`
- nils-cli PRs #727, #728, #731, #732 (sibling renames + audit cleanup)
- `sympoies/nils-alfredworkflow` #174 (memo consumer migration)
- agent-runtime-kit `tests/runtime-smoke/cases/{dispatch,pr}/run.sh`

## Recommended next artifact

- The Phase 1 `nils-cli` rename PR. No plan-tracking issue is created for this
  work (D5).
