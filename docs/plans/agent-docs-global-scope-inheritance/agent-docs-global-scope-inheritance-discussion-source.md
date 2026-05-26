# Agent Docs Global Scope Inheritance Implementation Handoff

- Status: ready for plan generation
- Date: 2026-05-24
- Source: local discussion about `agent-docs` inheritance semantics, current
  `agent-docs` resolver behavior, current `agent-runtime-kit` home catalog,
  and the legacy shell-parity fixture normalization rule.
- Intended next step: create a focused implementation plan for `agent-docs`
  global-scope inheritance and the related legacy fixture cleanup. This source
  document is not itself the implementation plan.

## Purpose

`AGENT_DOCS_HOME` should be able to stay pointed at the active
`agent-runtime-kit` checkout while preventing `agent-runtime-kit` project-only
documentation requirements from leaking into unrelated repositories.

The desired model is:

- `scope = "global"` entries in the home catalog are inherited by every
  project repo.
- `scope = "project"` entries in the home catalog apply only when the active
  project is `agent-runtime-kit` itself, including its linked worktrees.
- Project-local `AGENT_DOCS.toml` files continue to define project-specific
  requirements for their own repo.

This work should also remove the legacy `$HOME/.config/agent-kit` normalization
rule from the `plan-issue-cli` shell parity fixtures, because that runtime root
is retired.

## Source Tags

- `[U1]` The user wants to keep `AGENT_DOCS_HOME` set to the
  `agent-runtime-kit` checkout, but only inherit entries from
  `$HOMEProject/graysurf/agent-runtime-kit/AGENT_DOCS.toml` when those
  entries use `scope = "global"`.
- `[U2]` The user wants
  `crates/plan-issue-cli/tests/fixtures/shell_parity/regenerate.sh` to stop
  normalizing `$HOME/.config/agent-kit` to `$AGENT_KIT_HOME`, because that is
  legacy state.
- `[F1]` `crates/agent-docs/src/model.rs` currently supports only
  `Scope::Home` and `Scope::Project`; `scope = "global"` is not accepted.
- `[F2]` `crates/agent-docs/src/config.rs` loads one home config from
  `docs_home` and one project config from `project_path`.
- `[F3]` `crates/agent-docs/src/resolver.rs` currently merges all extension
  documents whose context matches, and resolves `scope = "project"` paths
  against the active project path.
- `[F4]` `crates/agent-docs/tests/integration/resolve_builtin.rs` currently
  asserts that home-catalog `scope = "project"` entries apply to the active
  project and can be overridden by the project catalog.
- `[F5]` `crates/agent-docs/tests/integration/config.rs` currently verifies
  unsupported scope values are rejected, so adding `global` changes the schema
  contract.
- `[F6]`
  `$HOMEProject/graysurf/agent-runtime-kit/AGENT_DOCS.toml` currently
  contains a `scope = "project"` docs-placement requirement intended for the
  runtime-kit source repo, but today it leaks into nils-cli when used as the
  home catalog.
- `[F7]`
  `crates/plan-issue-cli/tests/fixtures/shell_parity/regenerate.sh` and its
  README still mention `$HOME/.config/agent-kit` / `$AGENT_KIT_HOME`.
- `[F8]` `DEVELOPMENT.md` defines the docs-only validation lane as
  `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`, and the general
  check set includes docs placement, docs hygiene, markdown lint, plan bundle
  validation, CLI output contract lint, and fixture lint.

## Confirmed Facts

- The current `agent-docs` schema has no `global` scope. Adding this behavior
  requires code and tests in `crates/agent-docs`, not only a TOML change.
  `[F1][F5]`
- `agent-docs` currently treats home-catalog project entries as requirements
  for the active `project_path`. This is why a runtime-kit document path was
  checked under the nils-cli repo. `[F2][F3][F4][F6]`
- Pointing both `--docs-home` and `--project-path` at nils-cli avoids the leak,
  but that does not preserve the desired architecture where `AGENT_DOCS_HOME`
  remains the runtime-kit checkout. `[U1][F2]`
- The shell-parity fixture normalizer still contains the retired
  `$HOME/.config/agent-kit` path, and the README documents the same legacy
  normalization rule. `[U2][F7]`

## Decisions

1. Add a first-class `global` document scope to `agent-docs`.
2. Treat `scope = "global"` as the only home-catalog scope that applies to
   unrelated project repos.
3. Resolve home-catalog `scope = "global"` document paths relative to
   `AGENT_DOCS_HOME` / `--docs-home`, not relative to the active project.
4. Keep home-catalog `scope = "project"` entries local to the docs-home repo:
   they apply when the active project is the same repo as `docs_home`, including
   linked worktree cases.
5. Reject `scope = "global"` in project-local `AGENT_DOCS.toml` files. Global
   requirements must come from the home catalog only.
6. Remove the legacy `$HOME/.config/agent-kit` normalization from the
   `plan-issue-cli` shell parity fixture workflow instead of replacing it with
   another generic runtime-root placeholder.

## Scope

In scope:

- Extend `crates/agent-docs` schema and resolver behavior for `scope =
  "global"`.
- Update `agent-docs` tests to cover cross-repo home-catalog inheritance,
  same-repo runtime-kit project entries, project-local rejection of global
  entries, and output determinism.
- Update baseline/add command behavior if their use of `Scope` would expose
  invalid `global` combinations.
- Update the runtime-kit home catalog after the CLI supports `global`, changing
  intentionally cross-repo entries to `scope = "global"` and leaving
  runtime-kit-only entries as `scope = "project"`.
- Remove `$HOME/.config/agent-kit` / `$AGENT_KIT_HOME` normalization from
  `crates/plan-issue-cli/tests/fixtures/shell_parity/regenerate.sh` and update
  the fixture README.

Out of scope:

- Changing startup built-in policy resolution for `AGENTS.md`. This source
  document only covers `AGENT_DOCS.toml` extension documents.
- Reintroducing `$HOME/.agents`, `$HOME/.config/agent-kit`, or `$AGENT_HOME` as
  docs-home indirection.
- Redesigning project-local `scope = "home"` behavior unless tests show it is
  directly entangled with the new `global` validation.
- Changing unrelated historical docs that merely record old
  `$HOME/.config/agent-kit` commands as past execution evidence.

## Implementation Boundaries

- `crates/agent-docs/src/model.rs` owns accepted scope names and serialized
  scope values.
- `crates/agent-docs/src/config.rs` owns TOML parsing and should produce an
  actionable error when a project-local catalog declares `scope = "global"`.
- `crates/agent-docs/src/resolver.rs` owns inheritance filtering, path root
  selection, deduplication, and deterministic output order.
- `crates/agent-docs/src/commands/baseline.rs` and
  `crates/agent-docs/src/commands/add.rs` should be audited because they map
  scopes to roots/sources and may expose `global` through CLI flags.
- Integration tests under `crates/agent-docs/tests/integration/` should be the
  primary acceptance harness.
- `crates/plan-issue-cli/tests/fixtures/shell_parity/` owns the fixture
  normalization rule and shell parity README.
- `$HOMEProject/graysurf/agent-runtime-kit/AGENT_DOCS.toml` is a
  coordinated downstream config update. It must not use `scope = "global"` in
  a runtime environment that still runs an older released `agent-docs` binary.

## Requirements

- `scope = "global"` is accepted in the home catalog and appears in JSON/text
  output as a distinct scope.
- A home-catalog `scope = "global"` entry resolves its path from `docs_home`.
- A home-catalog `scope = "project"` entry does not apply when `project_path`
  points at an unrelated repo.
- A home-catalog `scope = "project"` entry still applies when `project_path`
  is the same repo as `docs_home`; linked worktrees of the same repository
  should be treated as same-repo if the resolver already has enough git root
  evidence to do that safely.
- Project-local `scope = "global"` entries fail with a clear validation error
  that says global scope is allowed only in the home catalog.
- Project-local `scope = "project"` entries continue to resolve from
  `project_path` and override duplicate inherited entries where applicable.
- Existing built-in docs remain immutable and deduplicated.
- The `plan-issue-cli` shell parity fixture workflow no longer mentions or
  normalizes `$HOME/.config/agent-kit` / `$AGENT_KIT_HOME`.

## Acceptance Criteria

- With `docs_home=$HOMEProject/graysurf/agent-runtime-kit` and
  `project_path=$HOMEProject/sympoies/nils-cli`, `agent-docs resolve
  --context project-dev --format json` includes home-catalog `global` entries
  from runtime-kit and nils-cli project entries, but does not include
  runtime-kit home-catalog `project` entries.
- In that cross-repo case, inherited global document paths point at
  `$HOMEProject/graysurf/agent-runtime-kit/...`, not
  `$HOMEProject/sympoies/nils-cli/...`.
- With both `docs_home` and `project_path` pointing at runtime-kit, runtime-kit
  home-catalog `project` entries are included.
- A fixture where a project `AGENT_DOCS.toml` declares `scope = "global"` fails
  before resolution with an actionable config validation error.
- Existing `agent-docs` output remains deterministic across repeated resolves.
- `cargo test -p agent-docs` passes.
- `bash -n crates/plan-issue-cli/tests/fixtures/shell_parity/regenerate.sh`
  passes after removing the legacy normalizer.
- `rg "\.config/agent-kit|AGENT_KIT_HOME"
  crates/plan-issue-cli/tests/fixtures/shell_parity` returns no matches after
  the fixture cleanup.
- Full non-doc validation passes before delivery:
  `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh
  --with-coverage`.

## Validation Plan

For the source document PR:

- `agent-docs resolve --docs-home $HOMEProject/sympoies/nils-cli
  --project-path $HOMEProject/sympoies/nils-cli --context startup
  --strict --format checklist`
- `agent-docs resolve --docs-home $HOMEProject/sympoies/nils-cli
  --project-path $HOMEProject/sympoies/nils-cli --context project-dev
  --strict --format checklist`
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`

For the implementation:

- Add failing integration tests for the cross-repo inheritance contract before
  changing resolver behavior.
- Run `cargo test -p agent-docs`.
- Run `bash -n
  crates/plan-issue-cli/tests/fixtures/shell_parity/regenerate.sh`.
- Run the fixture search acceptance check for retired path placeholders.
- Run
  `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh
  --with-coverage` before delivery.

## Risks And Guardrails

- Backward compatibility risk: changing `agent-runtime-kit/AGENT_DOCS.toml` to
  `scope = "global"` before releasing or installing a compatible `agent-docs`
  binary will make older binaries reject the catalog.
- Resolver ambiguity risk: `scope` currently controls path root selection,
  output labeling, and deduplication. The implementation must keep these
  responsibilities explicit for `global` rather than treating it as a cosmetic
  alias.
- Worktree risk: exact path equality is too narrow if runtime-kit work happens
  in linked worktrees. Prefer an existing git-root/common-dir comparison if it
  is already available and deterministic.
- Overreach risk: do not use this change to redesign all home/project startup
  policy. Keep the change scoped to `AGENT_DOCS.toml` extension documents.
- Fixture cleanup risk: remove the legacy path normalization only from the
  shell parity fixture workflow and README. Do not rewrite historical plan
  evidence unless a separate docs-retention decision asks for it.

## Execution

Recommended plan:
docs/plans/agent-docs-global-scope-inheritance/agent-docs-global-scope-inheritance-plan.md

Recommended execution state:
docs/plans/agent-docs-global-scope-inheritance/agent-docs-global-scope-inheritance-execution-state.md

- Status: not started
- Next-task source: this discussion-source document.

## Retention Intent

Keep this document as the read-first source for the implementation plan and
execution state. After delivery, retain it as plan provenance unless the final
plan promotes the stable `agent-docs` global-scope contract into a crate docs
spec or runbook.

## Resolved Decisions

- `agent-docs add --target home --scope global ...` should be supported as a
  first-class authoring path. `agent-docs add --target project --scope global`
  must fail with an actionable error because project-local catalogs cannot
  declare global requirements.
- Same-repo detection for home-catalog `scope = "project"` should use Git
  repository identity when both roots are Git repositories. Compare canonical
  Git common-dir or equivalent repository identity so linked worktrees of the
  same repository count as same-repo. Fall back to canonical path equality only
  when Git identity is unavailable.
- Global documents should participate in `baseline --target home` for the first
  implementation. Do not add a separate `baseline --target global` target yet.
  If home baseline output becomes confusing, add an explicit filter later, such
  as `agent-docs baseline --target home --scope global`.

## Recommended Next Artifact

Create
`docs/plans/agent-docs-global-scope-inheritance/agent-docs-global-scope-inheritance-plan.md`
and link this source document under the plan's `Read First` section.
