# Phase 1.5 — agent-runtime Render Engine and Minimal Drift Audit Discussion Source

- Status: open, ready for implementation planning
- Date: 2026-05-20
- Source: cross-repo discussion captured in
  `../agent-runtime-kit/docs/source/inventory-target-architecture.md`
  (sections "Migration Phases" → Phase 1.5; "CLI Boundary"; Resolved
  Decisions #1, #6, #9; "Build And Render Output"; "Drift Detection").
- Scope: Rust implementation, inside `sympoies/nils-cli`, of the
  `agent-runtime render` body and a minimal `agent-runtime audit-drift`
  body, plus the cross-process determinism harness and the v0.1.0
  release that unblocks agent-runtime-kit Phase 2.

## Execution

- Recommended plan:
  docs/plans/02-nils-cli-render-and-drift-audit/02-nils-cli-render-and-drift-audit-plan.md
- Recommended execution state:
  docs/plans/02-nils-cli-render-and-drift-audit/02-nils-cli-render-and-drift-audit-execution-state.md

## Purpose

Phase 1 (Plan 01 in agent-runtime-kit) lands the manifest schemas and
the placeholder `agent-runtime-cli` crate with subcommand stubs that
return `not implemented`. Phase 2 (reporting POC) then expects
`agent-runtime render` and `agent-runtime audit-drift` to do real
work. The gap between those two is Phase 1.5 and it lives entirely
inside `sympoies/nils-cli`, because the CLI surface is owned here
([Resolved Decision #6](https://github.com/graysurf/agent-runtime-kit/blob/main/docs/source/inventory-target-architecture.md#resolved-decisions)).
This bundle plans that gap: turn the stubs into a working render
engine plus a minimum-viable drift audit, prove determinism with a
cross-process test, and cut a `0.1.0` release that the tap can ship.

## Current Judgment

- The template engine is Tera ([Decision #1](https://github.com/graysurf/agent-runtime-kit/blob/main/docs/source/inventory-target-architecture.md#resolved-decisions));
  helper signatures (`script`, `skill_ref`, `state_out`, `cli_ref`) are
  fixed by the source doc.
- Determinism is non-negotiable ([Decision #9](https://github.com/graysurf/agent-runtime-kit/blob/main/docs/source/inventory-target-architecture.md#resolved-decisions)):
  no `HashMap`, no wall-clock, no randomness; clippy enforces.
- The full unsafe-scoring matrix and `intentional-difference` /
  `extra` classes are deferred to Plan 04. Phase 1.5 only needs the
  four blocking-or-near-blocking classes named in the source doc:
  source-manifest validity, rendered-target vs source diff,
  `$AGENT_HOME` leak, and docs-home correctness per product.
- The Bump Ceremony from [Decision #7](https://github.com/graysurf/agent-runtime-kit/blob/main/docs/source/inventory-target-architecture.md#resolved-decisions)
  is explicitly OUT OF SCOPE here; it lands with Plan 04's doctor
  work.
- The `--update-golden` knob is most natural as a flag on `render`
  (matches `cargo insta`'s `--accept` style), but the alternative of a
  separate subcommand is open until execution.

## Findings

| Priority | ID | Issue | Evidence | Fix Location | Acceptance |
| --- | --- | --- | --- | --- | --- |
| high | P1 | `agent-runtime render` is a stub; Phase 2 cannot proceed without it | `crates/agent-runtime-cli/src/commands/render.rs` (post Plan 01 stub) | `crates/agent-runtime-cli/src/render/` | render reads all five manifest classes, registers the four Tera helpers, writes to `build/<product>/`, and an `.render-cache.json` reproduces byte-identical output |
| high | P2 | Determinism contract has no cross-process enforcement | none yet | `crates/agent-runtime-cli/tests/integration/render_determinism.rs` | rendering twice in two processes with the cache deleted in between produces byte-identical `build/` trees |
| high | P3 | `agent-runtime audit-drift` is a stub; reporting POC needs the four blocking classes | `crates/agent-runtime-cli/src/commands/audit_drift.rs` (post Plan 01 stub) | `crates/agent-runtime-cli/src/audit_drift/` | audit-drift covers source-manifest validity, rendered-target diff, `$AGENT_HOME` leak (blocks at exit 2), docs-home per product (Codex `$CODEX_HOME`, Claude `$HOME/.claude`) |
| medium | P4 | `0.1.0` of `agent-runtime` must ship through the tap; agent-runtime-kit floors say `>=0.1.0` once shipped | `release/crates-io-publish-order.txt`; `sympoies/homebrew-tap/Formula/nils-cli.rb` | nils-cli release workflow + `homebrew-tap/Formula/nils-cli.rb` + `../agent-runtime-kit/manifests/skills.yaml` | tagged release publishes a binary the tap picks up; agent-runtime-kit manifests bump `required_clis['agent-runtime']` floors to `">=0.1.0"` |

## Ownership Boundary

- Runtime: `crates/agent-runtime-cli/` (new render + audit-drift
  bodies), `crates/nils-common/` (path resolution helpers reused by
  the Tera helpers, plus clippy lint gate).
- Test/harness: `crates/agent-runtime-cli/tests/integration/` and
  `crates/agent-runtime-cli/tests/drift/`.
- Release: `release/crates-io-publish-order.txt`,
  `.github/workflows/release.yml`, and cross-repo
  `sympoies/homebrew-tap/Formula/nils-cli.rb`.
- Cross-repo content: agent-runtime-kit's `manifests/*.yaml` (floors
  only — no schema changes).

## Backlog / Next Fixes

1. Land Sprint 1 render core so Plan 03 in agent-runtime-kit (reporting
   POC) can begin in parallel with Sprint 2.
2. Land determinism lints + cross-process test before any second
   helper ships.
3. Land minimal audit-drift body for the four blocking classes.
4. Cut `agent-runtime-cli` v0.1.0 and update the tap; bump
   agent-runtime-kit's `required_clis` floors.

## Retention Intent

- This source doc is the implementation handoff record for Phase 1.5
  and should stay until v0.1.0 ships and agent-runtime-kit confirms
  Phase 2 unblocked. After that, delete.
- The determinism integration test and the audit-drift fixture set
  stay as durable coverage.

## Validation Gate

- `cargo test -p agent-runtime-cli render`
- `cargo test -p agent-runtime-cli render_determinism`
- `cargo test -p agent-runtime-cli audit_drift`
- `cargo clippy -p agent-runtime-cli -p nils-common --all-targets -- -D warnings`
- `bash scripts/ci/nils-cli-checks-entrypoint.sh`
- `plan-tooling validate --file docs/plans/02-nils-cli-render-and-drift-audit/02-nils-cli-render-and-drift-audit-plan.md --strict`

## Do Not Do

- Do not implement unsafe scoring, `extra`, or `intentional-difference`
  drift classes here — they belong to Plan 04.
- Do not touch the Bump Ceremony (Decision #7); doctor work lands in
  Plan 04.
- Do not read `~/.codex`, `~/.claude`, or any runtime state from
  `render` — render reads only `core/`, `targets/`, `manifests/`.
- Do not introduce `std::collections::HashMap` at Tera context entry
  points; `IndexMap` or `BTreeMap` only.
- Do not call `SystemTime::now()` or `chrono::Utc::now()` in any
  helper module — the only sanctioned time value is `git log -1
  --format=%cI HEAD` at render start.

## Open Questions

- Ship `--update-golden` as a flag on `render` or as a separate
  subcommand? Default: flag on `render`, matching the source doc.
- Should the determinism clippy lints be workspace-wide or scoped to
  the affected crates? Default: scoped to `agent-runtime-cli` and
  `nils-common` to keep blast radius small; revisit if other crates
  start participating in render context.
