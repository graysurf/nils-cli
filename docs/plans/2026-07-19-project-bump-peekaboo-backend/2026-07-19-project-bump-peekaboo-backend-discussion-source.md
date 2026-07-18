# Peekaboo Backend Bump Skill Implementation Handoff

## Status

- Date: 2026-07-19
- Status: ready for L2 plan tracking; implementation not started
- Source: maintainer discussion about safely updating the Peekaboo backend
- Intended next step: execute the linked L2 plan test-first in a managed
  worktree and deliver one reviewed PR

## Purpose

Create a nils-cli project skill that turns a future Peekaboo backend bump into
an explicit, repeatable, reviewable workflow while preserving the existing
exact-lock and rollback security model.

## Confirmed facts

1. The current backend lock pins Peekaboo `v3.9.3` by tag, commit, release
   timestamp, asset URLs, archive/executable hashes, architectures, signing
   identities, bridge builds, notarization policy, and capability probes. [F1]
2. The current standalone CLI notarization waiver is bound to the complete
   v3.9.3 tuple; the app still requires notarization. [F1]
3. `rollback_releases` is currently empty, although the runtime supports exact
   rollback entries and rejects mutable receipt state as a trust source. [F1]
4. nils-cli keeps canonical project skills under `.agents/skills` and exposes
   that tree through its project-local Claude bridge. [F2]
5. The maintainer wants an L2 plan whose goal is to build this skill; current
   authorization covers plan creation, not implementation, release, or
   deployment. [U1]

## Decisions

- Name the skill `project-bump-peekaboo-backend`.
- Require one explicit immutable upstream tag. Do not support `latest`, a
  minimum app version, semantic ranges, or `>=` selection.
- Put reusable planning/apply behavior and its versioned machine-readable
  contract in released nils-cli. Keep the project skill as orchestration,
  policy, evidence presentation, and delivery guidance.
- Make dry-run the first operation and keep it repository-read-only. Apply must
  consume the reviewed plan inputs and fail closed on drift.
- Retain the previously trusted current release as exact rollback material when
  applying a newer release. Never delete the last viable rollback
  automatically.
- Treat every signing identity, team id, bundle id, architecture, asset name,
  bridge build, notarization result, and capability probe as candidate evidence
  to verify, not a value to inherit silently.
- Never copy the v3.9.3 notarization waiver to a future tag. Any future waiver
  requires separate approval bound to the complete candidate tuple.

## Scope

- A deterministic nils-cli planner/apply primitive for the Peekaboo lock.
- A project-local skill with a thin script, tests, and concise operator
  references.
- Exact upstream release/commit and artifact verification.
- Targeted updates to the lock, rollback allowlist, exact version/build
  assertions, fixtures, tests, and durable docs.
- Cross-platform fixture tests and a macOS no-apply trust/capability gate.
- Normal managed-worktree, test-first, validation, review, and PR delivery.

## Non-scope

- Installing or activating a candidate in a user's live runtime.
- Sparkle or app self-update integration.
- Floating or minimum-version resolution.
- Changing TCC, Keychain, certificates, trust stores, credentials, or app
  permissions.
- nils-cli release, deploy, private launcher changes, or public/private
  infrastructure coupling.
- Automatically granting a notarization waiver.

## Implementation boundaries

- The released nils-cli primitive owns deterministic plan/apply behavior,
  versioned JSON output, typed errors, exact artifact binding, and repository
  mutation.
- The project skill owns operator sequencing, mandatory review points, safe
  evidence summaries, repository validation, and PR workflow routing.
- The existing `macos-agent` install/verify/doctor/rollback runtime remains the
  authority for managed backend state; this skill does not become a second
  runtime updater.
- macOS tools may establish architecture, signing, Gatekeeper/notarization, and
  capability evidence. Fixture-only Linux checks do not prove those properties.

## Requirements

1. Require an exact tag and resolve it to an immutable upstream commit and
   release timestamp.
2. Verify license source, CLI/app asset names and URLs, archive/executable
   hashes, safe archive structure, architecture, bundle id, signing identity,
   team id, notarization policy/result, bridge build, and required capability
   probes.
3. Produce a versioned public-safe dry-run plan before any repository edit.
4. Bind apply to the reviewed tag, commit, artifact hashes, and trust tuple;
   reject stale or changed inputs without writing.
5. Move the old current release into `rollback_releases` as an exact trusted
   entry and keep at least one viable rollback.
6. Update hard-coded version/build expectations and waiver messages through an
   explicit reference inventory rather than a blind global replacement.
7. Make repeated apply of the same plan idempotent.
8. Keep provider-visible output free of credentials, private host inventory,
   and machine-local absolute paths.
9. Prepare a normal reviewed PR only; future release, install, activation, and
   deploy remain separately authorized operations.

## Acceptance criteria

- Meaningful failing behavioral tests exist before production edits for
  exact-tag input, dry-run non-mutation, rollback retention, stale-plan
  rejection, idempotency, and no-apply-on-failure.
- Cross-platform fixtures prove deterministic release/archive planning without
  live provider or macOS mutation.
- A macOS no-apply gate verifies architecture, signing,
  Gatekeeper/notarization posture, bridge build, and capability execution.
- Dry-run produces no repository diff; failed validation also produces no
  repository diff.
- Apply changes only the exact lock, rollback, compatibility assertion,
  fixture, test, and documentation surfaces named by the plan.
- A future tag cannot inherit the v3.9.3 waiver without separately reviewed
  exact-tuple approval.
- The prior trusted current release is available to the existing rollback
  command after a successful future bump.
- The project skill is available from the canonical `.agents/skills` tree and
  existing Claude bridge, with focused shell tests.
- The required nils-cli local-fast gate and independent pre-merge review pass.

## Validation plan

- Run test-first evidence classification and meaningful red tests before
  production behavior changes.
- Run focused Rust unit/integration and CLI JSON/help tests.
- Run skill-owned script tests and shell syntax checks.
- Run a macOS no-apply acceptance command against a known explicit tag.
- Run `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` before PR
  delivery and rely on required GitHub macOS/full-suite/coverage checks before
  merge.
- Require testing, maintainability, and supply-chain-focused review before the
  tracking issue review checkpoint.

## Risks and guardrails

- Upstream release asset/signing changes must stop automatic apply until
  reviewed.
- Provider release metadata is not sufficient trust evidence; downloaded bytes
  and executable/app metadata must match the reviewed plan.
- A completed notarization rejection and timeout/signal are different states;
  only an exact approved waiver may accept the former.
- No validation step may modify TCC, trust stores, credentials, or the live
  managed backend installation.

## Read-first references

- [F1] `crates/macos-agent/peekaboo-lock.json`,
  `crates/macos-agent/src/lock.rs`, `crates/macos-agent/src/backend/mod.rs`, and
  `crates/macos-agent/README.md` at the plan baseline.
- [F2] `.agents/skills/`, `.claude/skills`, and `AGENT_DOCS.toml` at the plan
  baseline.
- [U1] Maintainer request in the originating conversation to open an L2 plan
  whose goal is this project skill.

## Retention intent

This is a transient L2 source document. Retain it with the plan while work is
active, then migrate the completed bundle through the governed archive workflow
rather than promoting it to canonical product documentation.

## Execution

- Recommended plan: docs/plans/2026-07-19-project-bump-peekaboo-backend/2026-07-19-project-bump-peekaboo-backend-plan.md
- Recommended execution state: docs/plans/2026-07-19-project-bump-peekaboo-backend/2026-07-19-project-bump-peekaboo-backend-execution-state.md
