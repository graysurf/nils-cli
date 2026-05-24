# Plan: agent-runtime list-skills Subcommand

## Overview

Add a read-only `agent-runtime list-skills` subcommand to `agent-runtime-cli`
that enumerates the skills an `install` would activate for a given
product / source-root / live-home triple. The subcommand reuses the existing
`LinkMap` + `InstallPlan::build` + `doctor::skill_surface` modules and adds
deterministic `text` and `json` output formats so the
`agent-runtime-kit/scripts/ci/sandbox-install-rehearsal.sh` gate can replace
its current regex parsing of `install --dry-run` output. The cross-repo
rehearsal swap is staged after a nils-cli release that ships the new
subcommand.

## Read First

- Primary source:
  `docs/plans/agent-runtime-list-skills/agent-runtime-list-skills-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Open questions carried into execution: none
- Resolved v1 decisions carried into execution:
  - JSON output keys are sorted by skill `id` for determinism, in line with
    `agent-runtime` Resolved Decision #9.
  - `--live-home` is accepted for parity with `install` but is not required
    for v1 enumeration; the v1 surface does not introspect product-loaded
    state.
  - The `agent-runtime-kit` rehearsal swap lands only after a nils-cli
    release that contains `list-skills`.

## Scope

- In scope:
  - Add a `Command::ListSkills` variant to the `agent-runtime` clap surface.
  - Implement skill enumeration by reusing `install::link_map::LinkMap` and
    `install::plan::InstallPlan::build` instead of inventing a new walker.
  - Reuse `doctor::skill_surface` warning classifiers when
    `--include-warnings` is set.
  - Emit deterministic `text` and `json` v1 output, sorted by skill `id`.
  - Add unit and integration tests covering codex and claude products,
    domain-nested directory skills, recursive skill trees, file-symlink
    SKILL.md warnings, and JSON sort stability.
  - Generate bash and zsh completions and update user-facing docs.
  - Update the cross-repo
    `agent-runtime-kit/scripts/ci/sandbox-install-rehearsal.sh` to consume
    the new subcommand once a nils-cli release ships it.
- Out of scope:
  - Live-home introspection of what the product CLI actually loaded.
  - Mutating any live-home contents from `list-skills`.
  - Cross-product enumeration (`--product all`).
  - Adding a new `--live-home <path>` validation surface beyond accepting
    the flag for parity.

## Assumptions

1. `LinkMap::load` followed by `InstallPlan::build` is a sufficient skill
   discovery path because the rehearsal already depends on the same plan
   surface.
2. The canonical skill `id` shape is `<domain>.<skill>`, consistent with
   `agent-runtime-kit/tests/sandbox/<product>/expected-skills.txt`.
3. Tests can model both products with fixture source roots under
   `tempfile` directories without needing a real product CLI.
4. The JSON v1 schema is acceptable as a versioned contract; future field
   additions are non-breaking.

## Sprint 1: List-Skills CLI Surface And Enumeration

**Goal**: Land the `agent-runtime list-skills` clap surface, the skill
enumeration logic, and the deterministic `text` / `json` formatters before
adding docs or cross-repo work.

**Demo/Validation**:

- Command(s):
  - `cargo test -p agent-runtime-cli list_skills`
  - `cargo run -p agent-runtime-cli --bin agent-runtime -- list-skills --help`
  - `cargo run -p agent-runtime-cli --bin agent-runtime -- list-skills --source-root <fixture> --product codex --format json`
- Verify: the subcommand appears in root help, JSON output sorts by `id`,
  text output is single-line-per-skill, and `--include-warnings` surfaces
  the file-symlink SKILL.md warning class.

### Task 1.1: Add `Command::ListSkills` clap surface

- **Location**:
  - `crates/agent-runtime-cli/src/lib.rs`
  - `crates/agent-runtime-cli/src/commands/mod.rs`
  - `crates/agent-runtime-cli/src/commands/list_skills.rs`
- **Description**: Add the new `Command::ListSkills` enum variant, register
  it in `Command::name`, route it through `run`, and define the args struct
  with `--source-root`, `--product`, optional `--live-home`,
  `--format text|json` (default `text`), and `--include-warnings`.
- **Dependencies**:
  - none
- **Complexity**: 3
- **Acceptance criteria**:
  - `agent-runtime --help` lists `list-skills`.
  - `agent-runtime list-skills --help` documents every flag.
  - Missing required flags exit with a clear usage error and exit code 2.
- **Validation**:
  - `cargo test -p agent-runtime-cli`
  - `cargo run -p agent-runtime-cli --bin agent-runtime -- list-skills --help`

### Task 1.2: Implement skill enumeration via LinkMap + InstallPlan

- **Location**:
  - `crates/agent-runtime-cli/src/commands/list_skills.rs`
- **Description**: Resolve the source root and product, load the link-map,
  build the install plan, and project each `PlanAction::Symlink` whose
  destination matches a skill surface into a `SkillRecord` struct carrying
  `id`, `source`, `destination`, `link_mode`, and `warnings`. Use
  `doctor::skill_surface` helpers to derive Codex discoverability when the
  product is codex.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 5
- **Acceptance criteria**:
  - Codex product enumerates skills from `plugins/<domain>/skills/<skill>`
    destinations.
  - Claude product enumerates skills from `plugins/<domain>/skills/<skill>/SKILL.md`
    destinations.
  - Skill `id` shape is `<domain>.<skill>`, matching existing
    `expected-skills.txt` pins.
  - Non-skill plan actions (managed blocks, project-local overlays) are
    excluded from the enumeration.
- **Validation**:
  - `cargo test -p agent-runtime-cli list_skills`

### Task 1.3: Add text and JSON v1 formatters

- **Location**:
  - `crates/agent-runtime-cli/src/commands/list_skills.rs`
- **Description**: Implement deterministic formatters. Text output emits one
  tab-separated line per skill (`id\tlink_mode\tdestination`) sorted by
  `id`. JSON output emits an object with `product`, `source_root`,
  `live_home`, and `skills` (array of `SkillRecord`); arrays are sorted by
  `id`; nested `warnings` are sorted by `code` then `message`.
- **Dependencies**:
  - Task 1.2
- **Complexity**: 3
- **Acceptance criteria**:
  - JSON output is byte-deterministic across repeated runs.
  - Text output is single-line-per-skill and pipeable through
    `cut -f1 | sort > observed.txt`.
  - `--include-warnings` surfaces warnings inline; without the flag,
    warnings are present in JSON but elided from text.
- **Validation**:
  - `cargo test -p agent-runtime-cli list_skills`

### Task 1.4: Integration tests on fixture source roots

- **Location**:
  - `crates/agent-runtime-cli/tests/list_skills.rs`
- **Description**: Build small fixture source roots under `tempfile::tempdir`
  that exercise: codex domain-nested directory skill, codex recursive skill
  tree, claude SKILL.md leaf, and a file-symlink SKILL.md leaf that triggers
  the codex warning class. Run the subcommand binary via
  `assert_cmd::Command` and assert exit code, JSON shape, sort order, and
  warning surfacing.
- **Dependencies**:
  - Task 1.3
- **Complexity**: 5
- **Acceptance criteria**:
  - Both products produce the expected skill ids.
  - JSON output is exactly equal across two consecutive runs.
  - `--include-warnings` surfaces the file-symlink warning code.
- **Validation**:
  - `cargo test -p agent-runtime-cli --test list_skills`

## Sprint 2: Docs, Completions, And Required-Checks Gate

**Goal**: Land docs, generate completions, and pass the full nils-cli
required-checks lane so the change is releasable.

**Demo/Validation**:

- Command(s):
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
  - `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh`
- Verify: docs gate is clean, completion assets parse, and the
  required-checks lane is green end-to-end.

### Task 2.1: Generate bash and zsh completion assets

- **Location**:
  - `crates/agent-runtime-cli/src/commands/list_skills.rs`
  - `completions/bash/agent-runtime`
  - `completions/zsh/_agent-runtime`
- **Description**: Extend the existing `agent-runtime` completion generator
  to cover the new subcommand. Regenerate tracked assets and ensure
  `bash -n` / `zsh -n` syntax checks pass.
- **Dependencies**:
  - Task 1.3
- **Complexity**: 2
- **Acceptance criteria**:
  - `bash -n completions/bash/agent-runtime` passes.
  - `zsh -n completions/zsh/_agent-runtime` passes.
- **Validation**:
  - `bash -n completions/bash/agent-runtime`
  - `zsh -n completions/zsh/_agent-runtime`

### Task 2.2: Update agent-runtime-cli docs

- **Location**:
  - `crates/agent-runtime-cli/README.md`
  - `BINARY_DEPENDENCIES.md`
- **Description**: Document the new subcommand including flag semantics,
  JSON v1 schema, and the rehearsal-replacement use case. Update the binary
  dependency table if required.
- **Dependencies**:
  - Task 1.3
- **Complexity**: 2
- **Acceptance criteria**:
  - README documents `list-skills` flags, JSON v1 schema, and text format.
  - Docs gate (`--docs-only`) passes.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`

### Task 2.3: Run full required-checks gate

- **Location**:
  - `scripts/ci/nils-cli-checks-entrypoint.sh`
- **Description**: Run the four nils-cli CI gates plus full test suite
  locally to confirm the change is releasable.
- **Dependencies**:
  - Task 2.2
- **Complexity**: 3
- **Acceptance criteria**:
  - rumdl fmt, third-party-artifacts, completion-asset-audit, and
    Cargo.lock locked-build all pass locally.
  - `cargo test --workspace` is green.
- **Validation**:
  - `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh`

## Sprint 3: Cross-Repo Rehearsal Swap

**Goal**: Replace the regex parsing in
`agent-runtime-kit/scripts/ci/sandbox-install-rehearsal.sh` with a call to
`agent-runtime list-skills --format json`. Gated on a nils-cli release
that contains the new subcommand.

**Demo/Validation**:

- Command(s):
  - `bash agent-runtime-kit/scripts/ci/sandbox-install-rehearsal.sh`
- Verify: the rehearsal still diffs the observed list against
  `tests/sandbox/<product>/expected-skills.txt` cleanly and the regex
  branches are removed.

### Task 3.1: Replace regex parse with `list-skills --format json`

- **Location**:
  - `~/Project/graysurf/agent-runtime-kit/scripts/ci/sandbox-install-rehearsal.sh`
- **Description**: Drop the `extract_skill_ids` regex helper and switch the
  rehearsal to `agent-runtime list-skills --source-root "$REPO_ROOT"
  --product "$product" --live-home "$live_home" --format json` piped
  through `jq -r '.skills[].id' | sort` to produce `observed`.
- **Dependencies**:
  - Task 2.3
- **Complexity**: 3
- **Acceptance criteria**:
  - The script no longer carries product-specific regex branches.
  - The observed file matches the existing `expected-skills.txt` pins for
    both products.
- **Validation**:
  - `bash scripts/ci/sandbox-install-rehearsal.sh`

## Testing Strategy

- Unit: `LinkMap` -> `InstallPlan` projection logic and formatter
  determinism inside `commands::list_skills`.
- Integration: fixture-driven end-to-end run of `agent-runtime list-skills`
  via `assert_cmd::Command` for both products plus warning surfacing.
- E2E/manual: `agent-runtime list-skills --source-root <kit> --product codex
  --format json | jq -r '.skills[].id' | sort` diffed against the pinned
  expected file in a local agent-runtime-kit checkout.

## Risks & gotchas

- The JSON v1 schema becomes a public contract once released; field
  removals or renames are breaking changes and must bump the schema marker
  embedded in the output.
- The codex and claude skill destination shapes are slightly different;
  the projection logic must keep them as distinct cases instead of merging
  by accident.
- The rehearsal script swap depends on a released `agent-runtime`; it
  cannot land until at least nils-cli 0.22.0 is available, so it is
  scoped into Sprint 3.
- Reusing `doctor::skill_surface` for warnings ties two surfaces together;
  if `skill_surface` adds new warning codes later, the JSON `warnings`
  array gains new entries and downstream parsers must tolerate unknown
  codes.

## Rollback plan

- Revert the nils-cli commit that adds `Command::ListSkills` and the
  related tests. The subcommand is additive and not consumed by any other
  internal module, so the revert is mechanical.
- The cross-repo rehearsal swap, if landed, is reverted by restoring the
  previous `extract_skill_ids` helper; the pinned `expected-skills.txt`
  files are unchanged.
