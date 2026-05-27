# Codex Skill Surface Primitives Implementation Handoff

- Status: ready for plan generation
- Date: 2026-05-23
- Source: issue #43 closeout in `graysurf/agent-runtime-kit`, PR #46 live
  validation, current `agent-runtime-cli` install/audit implementation, and
  the follow-up discussion about what `nils-cli` can and should own for skill
  surfaces.
- Intended next step: create a focused implementation plan for hardening
  `nils-cli` skill-surface install, inspection, and diagnostics. This source
  document is not itself the implementation plan.

## Purpose

Codex skill discovery in `agent-runtime-kit` issue #43 exposed a narrow but
important boundary: the existing `agent-runtime-cli` install primitive can
already create the directory symlinks Codex needs, but the contract is implicit
and easy to misread as file-only behavior.

This document captures the recommended `nils-cli` follow-up: make directory
skill symlink behavior explicit, testable, inspectable, and diagnosable without
turning `nils-cli` into a Codex Desktop loader or reintroducing `$HOME/.agents`
as a runtime dependency.

## Source Tags

- `[U1]` The user asked to preserve the conclusion in `nils-cli` after the
  discussion about what `nils-cli` can or should adjust for skills.
- `[A1]` `graysurf/agent-runtime-kit` issue #43 is closed and records issue
  completion, linked PRs #45 and #46, final validation, rollback proof, and
  alias retirement: <https://github.com/graysurf/agent-runtime-kit/issues/43>.
- `[A2]` PR #46 explains the bug: Codex did not discover individual
  `SKILL.md` file symlinks, while the fix exposes skills as symlinked skill
  directories under `$CODEX_HOME/skills/<domain>/<skill>/`:
  <https://github.com/graysurf/agent-runtime-kit/pull/46>.
- `[A3]` Issue #43 validation evidence records that, with `$HOME/.agents`
  absent and legacy environment variables unset, `codex debug prompt-input`
  exposed `semantic-commit`, `execute-from-tracking-issue`,
  `deliver-tracking-issue`, `discussion-to-implementation-doc`, and
  `handoff-session-prompt` from the new directory-symlink surface:
  <https://github.com/graysurf/agent-runtime-kit/issues/43#issuecomment-4521419470>.
- `[A4]` Earlier issue #43 validation evidence records the failed shape:
  domain-nested paths existed with `SKILL.md` file symlinks, but Codex did not
  expose the required skills while `$HOME/.agents` was absent:
  <https://github.com/graysurf/agent-runtime-kit/issues/43#issuecomment-4520888131>.
- `[F1]` `crates/agent-runtime-cli/src/install/plan.rs` builds one
  `PlanAction::Symlink` for a non-recursive `symlinked-file` entry and expands
  recursive entries into per-file symlink actions.
- `[F2]` `crates/agent-runtime-cli/src/install/executor.rs` applies symlink
  actions with `std::os::unix::fs::symlink`, treats an existing symlink to the
  intended source as idempotent, replaces foreign symlinks, backs up regular
  files, and refuses to overwrite directories or other non-file destinations.
- `[F3]` `crates/agent-runtime-cli/src/audit_drift/classes/extra.rs` treats
  non-recursive symlink destinations as expected live paths, but scans their
  parent directory when looking for extra live runtime surface.
- `[F4]` `DEVELOPMENT.md` defines the docs-only validation lane as
  `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`.

## Confirmed Facts

- Existing `agent-runtime-cli` behavior is sufficient to create the passing
  issue #43 shape: a non-recursive `symlinked-file` entry whose source is a
  directory becomes one destination symlink, not a recursive file expansion.
  `[F1][F2][A2][A3]`
- The failing shape was not domain nesting by itself. The failure was that each
  skill body was exposed through an individual `SKILL.md` file symlink instead
  of a symlinked skill directory. `[A2][A4]`
- The passing Codex active surface uses domain-nested directory symlinks under
  `$CODEX_HOME/skills/<domain>/<skill>`, and a live gate proved the required
  skills were visible with `$HOME/.agents` absent. `[A3]`
- `nils-cli` currently names this install map kind `symlinked-file`, even
  though the non-recursive path can represent a directory symlink. This is
  behaviorally correct but semantically under-documented. `[F1]`
- `audit-drift` currently scans the parent of non-recursive symlink
  destinations. This is useful for owned plugin directories, but flat
  `$CODEX_HOME/skills/<skill>` entries would cause the broad `skills/` root to
  include unrelated Codex-owned or user-owned skills as extra surface. `[F3]`
- `nils-cli` can validate installed file shape and install-map intent. It
  cannot, by itself, prove what a fresh Codex Desktop session will expose to the
  model. Live product acceptance remains a downstream runtime-kit gate. `[A3]`

## Decisions

1. Do not make an urgent core install behavior change for issue #43. The
   current executor already supports the needed directory-symlink primitive.
2. Treat Codex active skills as directory surfaces containing `SKILL.md`, not as
   standalone `SKILL.md` file symlinks.
3. Keep domain-nested active skill destinations valid for Codex, because PR #46
   proved that shape works when the leaf skill itself is a directory symlink.
4. Harden the `nils-cli` contract with docs, tests, dry-run wording, and
   diagnostics so future plans do not infer file-only behavior from the
   `symlinked-file` name.
5. Add machine-readable skill-surface inspection before changing the link-map
   schema. The immediate gap is visibility and diagnostics, not a missing
   primitive.
6. Keep live Codex Desktop discovery proof outside `nils-cli`. `nils-cli`
   should report whether the installed shape matches the known contract; the
   product/runtime owner should run the live prompt/session gate.

## Scope

In scope for the recommended follow-up:

- Clarify the install contract for non-recursive directory sources in code
  comments, schema/help text, and dry-run output.
- Add tests proving directory source plus `recursive: false` produces and
  applies one directory symlink, remains idempotent, and refuses unsafe
  directory replacement at the destination.
- Add a machine-readable skill surface listing, either as a dedicated
  `agent-runtime skills list --product codex --format json` command or as an
  install-plan inspection mode.
- Add Codex-specific diagnostics that flag known-bad active skill shapes, such
  as `$CODEX_HOME/skills/**/SKILL.md` file symlinks used as the active
  discovery surface.
- Keep audit-drift changes narrow and explicit if scan-root behavior needs to
  avoid unrelated Codex-owned top-level skill directories.

Out of scope:

- Reimplementing Codex Desktop skill discovery in `nils-cli`.
- Depending on `$HOME/.agents`, `AGENT_HOME`, or ambient legacy environment
  variables for new skill surfaces.
- Moving runtime-kit skill sources, manifests, or product acceptance gates into
  `nils-cli`.
- Renaming the link-map kind immediately if compatibility-preserving docs,
  tests, and diagnostics can close the current ambiguity.
- Making CI depend on live Codex Desktop sessions.

## Implementation Boundaries

- `crates/agent-runtime-cli/src/install/plan.rs` owns expansion semantics:
  recursive entries expand into file actions; non-recursive entries stay as one
  symlink action.
- `crates/agent-runtime-cli/src/install/executor.rs` owns filesystem mutation,
  idempotence, backup, and refusal behavior.
- `crates/agent-runtime-cli/src/audit_drift/classes/extra.rs` owns extra live
  runtime-surface classification and must not silently widen or shrink ownership
  scope without tests.
- Any new skill-surface command should read existing link-map and manifest data
  instead of scraping live directories with ad hoc shell parsing.
- Product-specific checks should be explicit, for example `--product codex`,
  because Claude and Codex do not share the same active skill discovery model.

## Requirements

- Non-recursive directory symlink behavior is documented as an intentional
  contract, not an incidental side effect.
- Dry-run or inspection output distinguishes at least:
  - file symlink
  - directory symlink
  - recursive file expansion
- A Codex skill-surface inspection path can report each active skill with:
  - canonical skill id or link-map entry id
  - source path
  - destination path
  - link mode
  - whether the destination shape is expected to be Codex-discoverable
  - warnings for known-bad shapes
- A diagnostic warns or fails when a Codex active skill is represented only by a
  `SKILL.md` file symlink under the active `$CODEX_HOME/skills` surface.
- Audit-drift must keep protecting owned runtime roots while avoiding false
  warnings from unrelated Codex system or user skills when link maps use active
  skill leaf destinations.
- Acceptance text must state that `nils-cli` shape validation is not equivalent
  to fresh Codex Desktop model-visible acceptance.

## Acceptance Criteria

- Tests cover non-recursive directory source installation and prove the plan
  produces one symlink action rather than per-file actions.
- Executor tests or integration tests prove apply/no-op behavior for directory
  symlinks and refusal behavior when the destination is an existing directory.
- Dry-run or JSON plan output identifies directory symlink actions without the
  operator needing to infer them from filesystem state.
- A Codex skill-surface listing or doctor check reports the issue #43 passing
  shape as valid:
  `$CODEX_HOME/skills/<domain>/<skill> -> <source skill directory>`.
- The same diagnostic reports the issue #43 failing shape as invalid or
  suspicious:
  `$CODEX_HOME/skills/<domain>/<skill>/SKILL.md -> <source SKILL.md>`.
- Docs mention that live `codex debug prompt-input` or an equivalent product
  gate remains required for final Codex Desktop acceptance.

## Validation Plan

- `cargo test -p agent-runtime-cli install`
- `cargo test -p agent-runtime-cli audit_drift`
- Focused integration tests for install dry-run/apply/idempotence if the unit
  surface is insufficient.
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
- For this docs-only source document:
  `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`

## Risks And Guardrails

- A broad rename from `symlinked-file` to a new kind can create unnecessary
  schema churn. Prefer compatibility-preserving clarification first unless the
  next plan proves a new kind is worth the migration.
- Changing audit-drift scan-root behavior globally can hide real extra files
  under owned plugin roots. Prefer explicit metadata or product-specific
  handling over a silent global semantic change.
- A passing `nils-cli` doctor check can still overstate readiness if it is
  described as Codex Desktop acceptance. Keep the distinction visible in output
  and docs.
- Do not flatten Codex active skill destinations merely because some examples
  use flat skill roots. The issue #43 passing evidence supports domain-nested
  directory symlinks, and flat roots interact poorly with broad extra-surface
  scans.

## Execution

- Recommended plan: docs/plans/codex-skill-surface-primitives/codex-skill-surface-primitives-plan.md
- Recommended execution state: docs/plans/codex-skill-surface-primitives/codex-skill-surface-primitives-execution-state.md

## Retention Intent

This document is a plan-source artifact for a narrow `nils-cli` hardening lane.
After implementation, promote durable operator-facing rules into the relevant
install, audit-drift, or runtime-surface docs and clean up this plan bundle when
it no longer drives execution.

## Open Questions

- Should the machine-readable surface be a new `skills list` command, an
  `install --plan-json` extension, or a `doctor --class skill-surface` output?
- Should link-map schema add a compatibility alias such as `symlinked-path`, or
  is better dry-run wording enough for the current migration stage?
- Should audit-drift support an explicit `scan_scope: exact|parent` field for
  non-recursive leaf symlinks?
- Should Codex-specific diagnostics live under `doctor`, `audit-drift`, or both?

## Read First References

- `crates/agent-runtime-cli/src/install/plan.rs`
- `crates/agent-runtime-cli/src/install/executor.rs`
- `crates/agent-runtime-cli/src/audit_drift/classes/extra.rs`
- `DEVELOPMENT.md`
- `https://github.com/graysurf/agent-runtime-kit/issues/43`
- `https://github.com/graysurf/agent-runtime-kit/pull/46`

## Recommended Next Artifact

Create
`docs/plans/codex-skill-surface-primitives/codex-skill-surface-primitives-plan.md`
from this source document. Keep the first implementation lane small: contract
hardening, tests, and diagnostics before any schema rename or broad audit-drift
ownership change.
