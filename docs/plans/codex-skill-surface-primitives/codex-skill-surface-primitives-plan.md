# Plan: Codex Skill Surface Primitives

## Overview

Harden the `agent-runtime-cli` install/audit/doctor contract around the
directory-symlink primitive that Codex active skills already depend on.
Existing executor behaviour is correct (PR #46 / issue #43 closeout proved
it); the gap is that the contract is implicit, the dry-run output reads as
file-only, and there is no machine-readable Codex shape diagnostic. This plan
keeps the link-map schema, executor mutation rules, and audit-drift ownership
unchanged, and ships three sequential sprints: contract docs + tests + dry-run
wording, then a new `doctor --class skill-surface` lane, then operator-facing
documentation and the live-acceptance boundary.

## Read First

- Primary source: `docs/plans/codex-skill-surface-primitives/codex-skill-surface-primitives-discussion-source.md`
- Source type: `discussion-to-implementation-doc`
- Open questions carried into execution:
  - Schema alias such as `symlinked-path`: deferred per source Decision #4. Only revisit if Sprint 1 docs/tests cannot close
    the ambiguity.
  - `audit-drift` `scan_scope: exact|parent` metadata field: deferred. Sprint 3 pins current parent-scan behaviour with a
    regression test; revisit only if a real conflict surfaces.
  - Whether the Codex diagnostic also lives under `audit-drift`: this plan ships it under `doctor --class skill-surface`
    only; cross-listing in `audit-drift` is out of scope.

## Scope

- In scope:
  - Clarify the install-map contract for non-recursive `symlinked-file` entries whose source is a directory.
  - Cover module docs, schema help text, and dry-run wording.
  - Add tests that pin non-recursive directory source to one `PlanAction::Symlink`, idempotent apply, and refusal when the
    destination is an existing directory.
  - Add a new `doctor --class skill-surface` lane that classifies each link-map entry by link mode, reports Codex
    active-skill shape, and flags `SKILL.md` file symlinks under `$CODEX_HOME/skills/**`.
  - Update operator-facing docs (`DEVELOPMENT.md` and/or runbooks) to state that `nils-cli` shape validation is not a
    substitute for live `codex debug prompt-input` acceptance.
- Out of scope:
  - Renaming `EntryKind::SymlinkedFile` / `kind: symlinked-file` or adding a schema alias.
  - Changing executor mutation rules, backup semantics, or overlay merge behaviour.
  - Reimplementing Codex Desktop skill discovery in `nils-cli`.
  - Adding `$HOME/.agents`, `AGENT_HOME`, or legacy environment-variable dependencies.
  - Changing `audit-drift::classes::extra` scan-root behaviour beyond a regression-pinning test.
  - Cross-listing the Codex diagnostic under `audit-drift`.

## Assumptions

1. The source's confirmed facts hold: the current executor already produces the issue #43 passing shape, one directory
   symlink per non-recursive entry with a directory source. No core install primitive needs to change.
2. The selected diagnostic host is `agent-runtime doctor` with a new `skill-surface` class, exposed through the existing
   class-based `DoctorFinding` printout plus structured `--format json` output used by sibling classes.
3. Domain-nested Codex active skill destinations (`$CODEX_HOME/skills/<domain>/<skill>`) are the supported shape. Flat
   `$CODEX_HOME/skills/<skill>` is not introduced in this plan.
4. Reading the link map at `<source_root>/targets/<product>/link-map.yaml` plus the existing `RuntimeRootsManifest` is
   enough input for the new diagnostic; no new manifest fields are required.
5. The crate-wide determinism contract (no `SystemTime::now`, no `HashMap`) still applies to the new code paths.

## Sprint 1: Contract Hardening, Tests, And Dry-Run Wording

**Goal**: Make the existing directory-symlink primitive an explicit, tested,
operator-visible contract without changing executor behaviour.
**Demo/Validation**:

- Command(s):
  - `cargo nextest run -p agent-runtime-cli install`
  - `cargo nextest run -p agent-runtime-cli audit_drift`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
- Verify:
  - New unit tests in `install/plan.rs` and `install/executor.rs` cover the directory-symlink path: plan shape, apply,
    idempotence, and directory-destination refusal.
  - `agent-runtime install --dry-run` printout distinguishes file symlink, directory symlink, and recursive expansion in
    `print_change` lines.
  - `link-map.yaml` schema doc-comment in `install/link_map.rs` records that `kind=symlinked-file` plus
    `recursive: false` accepts a directory source.

### Task 1.1: Document the directory-symlink contract in install modules

- **Location**:
  - `crates/agent-runtime-cli/src/install/plan.rs`
  - `crates/agent-runtime-cli/src/install/link_map.rs`
  - `crates/agent-runtime-cli/src/install.rs`
- **Description**: Update module/`pub struct` doc comments to state that a non-recursive `symlinked-file` entry whose
  `source` resolves to a directory becomes one `PlanAction::Symlink` whose destination is a directory symlink. Tighten the
  validator error text for `kind=symlinked-file` so the schema message names file-or-directory acceptance instead of
  implying file-only.
- **Dependencies**:
  - none
- **Complexity**: 2
- **Acceptance criteria**:
  - `install/plan.rs` and `install/link_map.rs` module docs reference both file-target and directory-target use of `symlinked-file`.
  - Validator/help strings touching `kind=symlinked-file` no longer imply file-only.
  - No behavioural code change; tests still pass without modification.
- **Validation**:
  - `cargo test -p agent-runtime-cli install`

### Task 1.2: Pin non-recursive directory-source plan shape and apply behaviour with tests

- **Location**:
  - `crates/agent-runtime-cli/src/install/plan.rs`
  - `crates/agent-runtime-cli/src/install/executor.rs`
- **Description**: Add unit tests covering:
  - `InstallPlan::build` with `recursive: false` and a directory source emits exactly one `PlanAction::Symlink` whose
    destination is the link-map `destination`, not per-file expansion.
  - `executor::run` in `Mode::Apply` creates the directory symlink, treats a second `Mode::Apply` as a `NoOp`, and returns
    an `ApplyError::Io { kind: AlreadyExists }` or equivalent refusal when `dest` is an existing real directory.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 3
- **Acceptance criteria**:
  - At least one `plan.rs` test pins single-action plan shape for the directory source path.
  - At least two `executor.rs` tests pin apply + idempotence and directory-destination refusal using `tempfile::TempDir`.
  - New tests fail on a sabotage that expands the directory entry into per-file actions or that overwrites a real directory.
- **Validation**:
  - `cargo nextest run -p agent-runtime-cli install`

### Task 1.3: Update dry-run / apply printout to label file vs directory vs recursive expansion

- **Location**:
  - `crates/agent-runtime-cli/src/commands/install.rs`
- **Description**: Extend `print_change` and nearby `eprintln!` summary output so each symlink change line names the link
  mode: `file symlink`, `directory symlink`, or `recursive file symlink` when the entry was expanded. Derive the mode from
  source path metadata where possible and keep summary counter wording.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 3
- **Acceptance criteria**:
  - Each printed change line includes one of the three link-mode tokens.
  - Operator-facing summary still prints `actions=` / `changes=` counts unchanged.
  - At least one CLI smoke test or assertion covers the new wording for the directory-symlink path.
- **Validation**:
  - `cargo test -p agent-runtime-cli`

## Sprint 2: `doctor --class skill-surface` Diagnostic

**Goal**: Add a machine-readable Codex skill-surface inspection lane to
`doctor` that classifies link-map entries and emits Codex-specific shape
warnings without altering executor or audit-drift behaviour.
**Demo/Validation**:

- Command(s):
  - `cargo nextest run -p agent-runtime-cli doctor`
  - `cargo run -p agent-runtime-cli -- doctor --class skill-surface --product codex --format json --source-root <fixture>`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
- Verify:
  - JSON output lists each link-map entry as a skill-surface item with `id`, `source`, `destination`, `link_mode`,
    `expected_codex_discoverable`, and optional `warnings`.
  - A `kind=symlinked-file` entry whose destination matches `skills/**/SKILL.md` for `--product codex` emits a
    `codex.active-skill.file-symlink` warning.
  - A domain-nested directory symlink (`skills/<domain>/<skill>` whose source is a directory) is reported as
    `expected_codex_discoverable=true` with no warnings.

### Task 2.1: Add a `skill-surface` class scaffold to the doctor pipeline

- **Location**:
  - `crates/agent-runtime-cli/src/doctor/`
  - `crates/agent-runtime-cli/src/commands/doctor.rs`
- **Description**: Introduce a `skill_surface` module under `doctor/` with a `pub fn check(...) -> Vec<DoctorFinding>`
  signature mirroring sibling classes. Wire it behind the existing `--class` selector so `--class skill-surface` triggers
  it. Skip silently when the link map is missing, matching `audit_drift::classes::extra`. No Codex-specific logic in this
  task; class wiring only. Sequential ordering note: although this task has no hard task-level dependency, it lands after
  Sprint 1 in the integration order so reviewers see contract-hardening changes before the new class.
- **Dependencies**:
  - none
- **Complexity**: 3
- **Acceptance criteria**:
  - `doctor --class skill-surface` returns zero findings on a fixture with an empty link map and exits 0.
  - `doctor` without `--class skill-surface` does not invoke the new class.
  - Module compiles and is wired into the existing `DoctorFinding` print path.
- **Validation**:
  - `cargo test -p agent-runtime-cli doctor`

### Task 2.2: Classify link-map entries by link mode and Codex-discoverability

- **Location**:
  - `crates/agent-runtime-cli/src/doctor/skill_surface.rs` (new)
- **Description**: Implement classification by reading `<source_root>/targets/<product>/link-map.yaml`. For each entry,
  produce `{ id, source, destination, link_mode, expected_codex_discoverable }`, where `link_mode` is
  `file|directory|recursive-file`. For `--product codex`, `expected_codex_discoverable` is `true` only when the entry is a
  `symlinked-file` non-recursive entry whose source is a directory and whose destination begins with
  `skills/<domain>/<skill>`. All other `--product codex` skills-prefixed entries are not Codex-discoverable. Other
  destinations are reported as `not-applicable`.
- **Dependencies**:
  - Task 2.1
- **Complexity**: 5
- **Acceptance criteria**:
  - Pure-function classifier is unit-tested with directory-source non-recursive under `skills/<domain>/<skill>`,
    `SKILL.md` file leaf under `skills/<domain>/<skill>`, recursive entry under `skills/...`, and non-skills destination.
  - JSON output of `--format json` matches the documented field set with deterministic ordering by link-map declaration
    order, matching `InstallPlan`.
- **Validation**:
  - `cargo nextest run -p agent-runtime-cli doctor::skill_surface`

### Task 2.3: Emit `codex.active-skill.file-symlink` warning for known-bad shape

- **Location**:
  - `crates/agent-runtime-cli/src/doctor/skill_surface.rs`
  - `crates/agent-runtime-cli/src/commands/doctor.rs`
- **Description**: For `--product codex`, any item whose destination matches `skills/**/SKILL.md` at any depth emits a
  `DoctorFinding` with class `skill-surface`, severity `warn`, and a stable warning code
  `codex.active-skill.file-symlink`. Include the entry id, destination, and a one-line remediation pointing at
  "use a directory-symlink leaf at `skills/<domain>/<skill>`". Non-Codex products do not emit this warning.
- **Dependencies**:
  - Task 2.2
- **Complexity**: 3
- **Acceptance criteria**:
  - Fixture link map with a `SKILL.md`-leaf Codex entry produces exactly one warning with the documented code.
  - Fixture link map matching the issue #43 passing shape produces zero warnings.
  - `--format json` includes the warning code in the structured payload, not only in the human-readable line.
- **Validation**:
  - `cargo nextest run -p agent-runtime-cli doctor`

## Sprint 3: Audit-Drift Regression Pin, Operator Docs, And Live-Acceptance Boundary

**Goal**: Lock in current `audit-drift` parent-scan behaviour against the
domain-nested Codex shape, and make the live-acceptance boundary visible in
operator docs and command output so a passing `doctor` is not mistaken for
fresh Codex Desktop acceptance.
**Demo/Validation**:

- Command(s):
  - `cargo nextest run -p agent-runtime-cli audit_drift`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
- Verify:
  - New `audit_drift::classes::extra` test pins zero extra-surface findings against the issue #43 domain-nested
    directory-symlink fixture.
  - `DEVELOPMENT.md` or a targeted runbook names `doctor --class skill-surface` as shape-only validation and points to
    `codex debug prompt-input` for live acceptance.
  - `doctor --class skill-surface` operator-facing output, both human and JSON, includes a one-line acceptance-boundary
    footer or field.

### Task 3.1: Pin `audit-drift extra` behaviour for domain-nested directory symlinks

- **Location**:
  - `crates/agent-runtime-cli/src/audit_drift/classes/extra.rs`
  - `crates/agent-runtime-cli/src/audit_drift/` (tests)
- **Description**: Add a regression test that constructs a fixture link map containing a non-recursive `symlinked-file`
  entry whose destination is `skills/<domain>/<skill>` and whose source is a directory. Materialise a matching live home and
  assert that `extra::check` produces zero `extra` findings. The test pins current `scan_roots` parent-scan behaviour with
  the issue #43 passing shape. No code change to `extra.rs` itself.
- **Dependencies**:
  - none
- **Complexity**: 4
- **Acceptance criteria**:
  - Test fails if `scan_roots` widens extra detection beyond the entry's own parent for non-recursive entries in a way that
    would surface unrelated Codex-owned skills.
  - Test passes today against unchanged `extra.rs`.
- **Validation**:
  - `cargo nextest run -p agent-runtime-cli audit_drift`

### Task 3.2: Document the live-acceptance boundary in operator docs

- **Location**:
  - `DEVELOPMENT.md`
  - or a targeted runbook under `docs/runbooks/` if `DEVELOPMENT.md` is the wrong host
- **Description**: Add a short section stating that `agent-runtime doctor --class skill-surface` validates install-map shape
  only, that a passing run is not Codex Desktop acceptance, and that live acceptance requires `codex debug prompt-input` in
  a fresh Codex Desktop session with `$HOME/.agents` absent and legacy env vars unset. Link the discussion source as the
  rationale.
- **Dependencies**:
  - Task 2.3
- **Complexity**: 2
- **Acceptance criteria**:
  - Section exists with the three claims above and the source-doc link.
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` passes.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`

### Task 3.3: Surface the live-acceptance boundary in `doctor --class skill-surface` output

- **Location**:
  - `crates/agent-runtime-cli/src/doctor/skill_surface.rs`
  - `crates/agent-runtime-cli/src/commands/doctor.rs`
- **Description**: When `doctor --class skill-surface --product codex` runs, append a one-line operator-facing footer in
  human format and an `acceptance_boundary` string field in JSON format. The text states that shape validation passed but
  live Codex Desktop discovery requires `codex debug prompt-input`. Apply to both warn and pass exits.
- **Dependencies**:
  - Task 2.3
  - Task 3.2
- **Complexity**: 2
- **Acceptance criteria**:
  - Human-format output ends with the boundary line for `--product codex` (both warn and pass paths).
  - JSON-format output includes `acceptance_boundary` as a stable string field.
  - At least one assertion in the doctor tests checks for the boundary line / field.
- **Validation**:
  - `cargo nextest run -p agent-runtime-cli doctor`

## Testing Strategy

- Unit:
  - `install::plan` tests for non-recursive directory source → one symlink action.
  - `install::executor` tests for apply + idempotence + directory-destination refusal using `tempfile::TempDir`.
  - `doctor::skill_surface` tests for link-mode classification and the Codex `SKILL.md`-leaf warning.
  - `audit_drift::classes::extra` regression test pinning parent-scan behaviour against the domain-nested directory-symlink
    fixture.
- Integration:
  - `cargo run -p agent-runtime-cli -- install --dry-run` smoke against a fixture link map exercises the new printout
    wording.
  - `cargo run -p agent-runtime-cli -- doctor --class skill-surface --product codex --format json` against fixtures
    exercises classification, warning, and acceptance-boundary footer end-to-end.
- E2E/manual:
  - One-off manual run of `doctor --class skill-surface --product codex` against the real local install before Sprint 3
    sign-off, captured under `${CLAUDE_KIT_STATE_HOME}/out/` per CLAUDE.md.

## Risks & gotchas

- The crate-wide `#![deny(clippy::disallowed_types, clippy::disallowed_methods)]` forbids `std::collections::HashMap` and
  `SystemTime::now`. The new diagnostic must use deterministic aggregation and must not stamp wall-clock time into output.
- `print_change` wording is operator-facing. Any change must keep current `actions=` / `changes=` counter format so
  downstream log parsers continue to work. Treat the new link-mode token as additive.
- Sprint 2's classifier must not load or stat live `$CODEX_HOME` paths. It reads only the link map plus the source root.
  Stat-ing live paths would couple `doctor` to host state and break the current `--source-root` reproducibility contract.
- `audit-drift::classes::extra::scan_roots` widens to `dest.parent()` for non-recursive entries. The regression test
  freezes this against the domain-nested Codex shape only. Introducing flat `skills/<skill>` destinations in a future plan
  is the migration path that would re-open this question; flag any such proposal in review.
- Codex active-skill shape is a moving product target. The `codex.active-skill.file-symlink` warning encodes the currently
  known-bad shape (`skills/**/SKILL.md`). If Codex later accepts file leaves, the warning becomes a false positive; document
  the warning code in the operator runbook so it is searchable when the product changes.
- Plan scope explicitly excludes a schema rename / alias and any `scan_scope` field. If Sprint 2/3 surfaces a real need,
  such as flat-skill-root conflict in the wild, escalate to a follow-up plan rather than widening this one.

## Rollback plan

- Sprint 1 changes are pure docs, tests, and printout wording; revert the touched files to roll back. No data migration, no
  on-disk format change, no install behaviour change.
- Sprint 2 introduces a new `doctor` class wired behind `--class skill-surface`. Roll back by deleting the new module and
  removing the class arm from `commands::doctor`. No state on disk, no schema change.
- Sprint 3's `audit_drift` regression test is additive; delete the test file or section to revert. Doc changes in
  `DEVELOPMENT.md` revert by reverting the markdown patch. The `acceptance_boundary` footer/field is additive in `doctor`
  output; remove the print/serialize path to revert.
- No external consumers should depend on the new printout tokens or the `acceptance_boundary` JSON field within this plan's
  lifecycle. If downstream tooling latches onto them later, treat that latching as the migration cost of a future rollback.
