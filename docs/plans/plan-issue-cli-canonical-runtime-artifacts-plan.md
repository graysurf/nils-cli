# Plan: plan-issue-cli canonical runtime artifacts

## Overview

Bring `crates/plan-issue-cli` into compliance with the canonical
`plan-issue-delivery` runtime artifact contract from
`agent-kit/skills/automation/plan-issue-delivery/SKILL.md` so the
`plan-issue` claude-kit wrapper (and the Codex / OpenCode adapters
shipped in `agent-kit/references/`) can dispatch sprint subagents
end-to-end. Today the binary emits a flat `<plan-slug>-...` layout
under `$AGENT_HOME/out/plan-issue-delivery/` with no snapshot files,
no per-task dispatch records, and no `plan-branch.ref`; the wrapper
expects the nested `<repo-slug>/issue-<n>/sprint-<n>/...` layout
defined in `references/RUNTIME_LAYOUT.md`. This plan introduces a
`runtime_layout` module that derives the canonical paths and threads
the new emissions into `start-plan` and `start-sprint`. Breaking
change: the old flat artifact paths are removed in this delivery; no
backward-compat shim is shipped because the user explicitly
authorized it and the only consumers (the wrapper + adapter agents)
already expect the canonical layout.

## Scope

- In scope:
  - New `crates/plan-issue-cli/src/runtime_layout.rs` module that
    derives `RUNTIME_ROOT`, `ISSUE_ROOT`, `SPRINT_ROOT`, and every
    artifact path enumerated in `RUNTIME_LAYOUT.md` lines 30-52.
  - `start-plan` writes `MAIN_AGENT_INIT_SNAPSHOT_PATH` and
    `PLAN_BRANCH_REF_PATH` under `$ISSUE_ROOT/`.
  - `start-sprint` writes `PLAN_SNAPSHOT_PATH`,
    `SUBAGENT_INIT_SNAPSHOT_PATH`, one
    `DISPATCH_RECORD_PATH=dispatch-<TASK_ID>.json` per assigned task
    with the keys named in `RUNTIME_LAYOUT.md` L48-52, a
    `PROMPT_MANIFEST_PATH` TSV, and per-task `TASK_PROMPT_PATH` files
    relocated to `$SPRINT_ROOT/prompts/<TASK_ID>.md`.
  - Updates to `plan-issue-cli-contract-v2.md`,
    `plan-issue-state-machine-v1.md`, and `plan-issue-gate-matrix-v1.md`
    so the new artifacts are part of the public contract.
  - Test parity fixtures under
    `crates/plan-issue-cli/tests/fixtures/runtime_layout/` that pin the
    canonical layout for both `start-plan` and `start-sprint`.
  - CHANGELOG entry (or equivalent release note) flagging the
    breaking-change directory layout.
- Out of scope:
  - GitHub-side mutations beyond what `start-plan` already does. The
    plan only adds local `.ref` and snapshot artifacts; PLAN_BRANCH is
    not pushed to GitHub and worktrees are not created here.
  - Wrapper SKILL.md (`agent-kit`) edits — canonical contract is
    already correct; binary catches up.
  - Adapter agent rewrites in claude-kit (`~/.claude/agents/plan-issue-*.md`)
    or codex / opencode equivalents. They already expect the
    canonical bundle.
  - `runtime_name` / `runtime_role` / `runtime_role_fallback_reason`
    injection into `dispatch-<TASK_ID>.json`. Per canonical SKILL.md
    L81-82 + L138-144 those are the main agent / wrapper's
    responsibility at dispatch time. The binary writes the dispatch
    record without them.
  - Refactors of unrelated commands (`status-plan`, `link-pr`,
    `ready-sprint`, `accept-sprint`, `ready-plan`, `close-plan`,
    `cleanup-worktrees`). Their logic operates on the GitHub issue
    body and TSV rows, both of which stay shape-compatible.
  - `multi-sprint-guide` text changes. Its output references the
    sequence of subcommands, not the artifact paths.

## Assumptions

1. `agent-kit` lives at `$AGENT_HOME` (verified: today
   `$AGENT_HOME=$HOME/.config/agent-kit`). Canonical source files are
   reachable at `$AGENT_HOME/skills/automation/plan-issue-delivery/`
   and `$AGENT_HOME/prompts/plan-issue-delivery-{main-agent,subagent}-init.md`.
2. The Cargo package name is `nils-plan-issue-cli` and the two
   binaries are `plan-issue` (live) and `plan-issue-local` (offline).
   Both binaries share the same `execute.rs` code paths for
   `start-plan` / `start-sprint`, so the new emissions land in both
   modes uniformly.
3. The current test runner is `cargo nextest`; per `DEVELOPMENT.md`
   the canonical pre-delivery command is
   `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage`.
4. `pretty_assertions::{assert_eq, assert_ne}` is the convention for
   diff-friendly Rust test asserts and is already a dev-dependency
   pattern in this workspace.
5. The breaking change will not be self-hosted: until this delivery
   ships, `/plan-issue:plan-issue-delivery` cannot run sprint dispatch
   end-to-end, so this plan is delivered manually (sprint-by-sprint
   PRs cut by hand) rather than through plan-issue itself.

## Validation command conventions

- Named-test validations must prove the test exists and passes.
- Pattern:
  - `cargo test -p nils-plan-issue-cli -- --list | rg '^test_name:'`
  - `cargo test -p nils-plan-issue-cli test_name -- --exact`
- `test -f` on output artifacts is supplemental; behaviour gates use
  named tests.

## Sprint gate policy

- Rule: Sprint `N` is accepted only when
  `$AGENT_HOME/out/plan-issue-cli/sprint-N/acceptance.md` exists and
  records `Result: PASS`.
- Rule: Sprint `N+1` cannot start until Sprint `N` acceptance
  artifact is present.
- Rule: acceptance artifacts are produced at the end of each sprint
  and are never deferred.
- Rule: failures must include the failing command, exit code, and
  key stderr.

## Sprint 1: Contract freeze and runtime-layout module

**Goal**: Lock the canonical artifact contract in crate-local docs
and land a pure path-derivation module, with no behavioural change in
`start-plan` / `start-sprint` yet. This sprint is preparation only;
subsequent sprints cut over each command.
**Demo/Validation**:

- Command(s):
  - `plan-tooling validate --file docs/plans/plan-issue-cli-canonical-runtime-artifacts-plan.md`
  - `cargo test -p nils-plan-issue-cli runtime_layout -- --list | rg '^test_name:'`
  - `cargo test -p nils-plan-issue-cli runtime_layout`
- Verify:
  - Updated specs cite `RUNTIME_LAYOUT.md` artifact paths verbatim.
  - `runtime_layout` module compiles, exposes the required path
    helpers, and is round-trip tested.
  - No production code path under `run_start_plan` /
    `run_start_sprint` calls into the new module yet (cutover is
    Sprint 2/3).

**PR grouping intent**: `group`
**Execution Profile**: `parallel-x2`

### Task 1.1: Update plan-issue-cli contract spec for canonical artifacts

- **Location**:
  - `crates/plan-issue-cli/docs/specs/plan-issue-cli-contract-v2.md`
- **Description**: Append a new top-level section "Canonical Runtime
  Artifacts (v2)" that declares ownership of every artifact in
  `RUNTIME_LAYOUT.md` lines 30-52: the layout root, `ISSUE_ROOT`,
  `SPRINT_ROOT`, plan-scoped artifacts (`MAIN_AGENT_INIT_SNAPSHOT_PATH`,
  `PLAN_SNAPSHOT_PATH`, `PLAN_BRANCH_REF_PATH`), sprint-scoped
  artifacts (`SUBAGENT_INIT_SNAPSHOT_PATH`, `TASK_PROMPT_PATH`,
  `PROMPT_MANIFEST_PATH`, `TASK_SPEC_PATH`), per-task artifacts
  (`DISPATCH_RECORD_PATH` + the JSON key set), and worktree path
  rules. Note that runtime adapter fields (`runtime_name`,
  `runtime_role`, `runtime_role_fallback_reason`) are deliberately
  left absent because they are written by the active runtime adapter
  at dispatch time per canonical `SKILL.md` L81-82 + L138-144. State
  the breaking change: the old flat
  `$AGENT_HOME/out/plan-issue-delivery/<plan-slug>-...` paths are
  retired in this delivery.
- **Dependencies**:
  - none
- **Complexity**: 4
- **Acceptance criteria**:
  - Spec section names every required path constant.
  - Spec lists the eleven required JSON keys (`task_id`,
    `task_prompt_path`, `subagent_init_snapshot_path`,
    `plan_snapshot_path`, `worktree`, `branch`, `execution_mode`,
    `pr_group`, `base_branch`, `workflow_role`) plus the three
    optional adapter keys.
  - Spec calls out the breaking-change cutover and links to this
    plan file.
- **Validation**:
  - `rg -n 'MAIN_AGENT_INIT_SNAPSHOT_PATH|PLAN_BRANCH_REF_PATH|PLAN_SNAPSHOT_PATH'
    crates/plan-issue-cli/docs/specs/plan-issue-cli-contract-v2.md`
  - `rg -n 'SUBAGENT_INIT_SNAPSHOT_PATH|DISPATCH_RECORD_PATH|PROMPT_MANIFEST_PATH'
    crates/plan-issue-cli/docs/specs/plan-issue-cli-contract-v2.md`
  - `rg -n 'breaking change|removed flat layout' crates/plan-issue-cli/docs/specs/plan-issue-cli-contract-v2.md`
  - `rg -n 'workflow_role' crates/plan-issue-cli/docs/specs/plan-issue-cli-contract-v2.md`

### Task 1.2: Update state-machine and gate-matrix specs for new artifacts

- **Location**:
  - `crates/plan-issue-cli/docs/specs/plan-issue-state-machine-v1.md`
  - `crates/plan-issue-cli/docs/specs/plan-issue-gate-matrix-v1.md`
- **Description**: Add an "Artifact emission" subsection to the
  `start-plan` and `start-sprint` transitions in the state-machine
  spec; list the artifacts each transition is responsible for and
  link to the contract spec section from Task 1.1. In the
  gate-matrix, add a row per command stating that artifact emission
  failure (e.g. cannot create `$ISSUE_ROOT`, cannot copy snapshot
  source) is a hard fail with exit code 1, mirroring the canonical
  `SKILL.md` L175-176 hard-fail.
- **Dependencies**:
  - `Task 1.1`
- **Complexity**: 3
- **Acceptance criteria**:
  - State-machine spec names the artifacts each transition emits.
  - Gate-matrix has a hard-fail row for artifact emission failure
    on both `start-plan` and `start-sprint`.
  - Cross-links between the three spec files are bidirectional.
- **Validation**:
  - `rg -n 'Artifact emission|MAIN_AGENT_INIT_SNAPSHOT|DISPATCH_RECORD' crates/plan-issue-cli/docs/specs/plan-issue-state-machine-v1.md crates/plan-issue-cli/docs/specs/plan-issue-gate-matrix-v1.md`

### Task 1.3: Add `runtime_layout` module with path derivation

- **Location**:
  - `crates/plan-issue-cli/src/runtime_layout.rs` (new)
  - `crates/plan-issue-cli/src/lib.rs`
- **Description**: Add a pure module that exposes:
  - `pub fn runtime_root() -> Result<PathBuf, RuntimeLayoutError>` —
    reads `$AGENT_HOME` and returns `<AGENT_HOME>/out/plan-issue-delivery`.
    Returns an explicit error variant `agent_home_not_set` when the
    env var is missing or empty (do not panic, do not fall back).
  - `pub fn repo_slug(owner_repo: &str) -> String` — converts
    `owner/repo` to `owner__repo`.
  - `pub struct IssueRoot { ... }` and `pub struct SprintRoot { ... }`
    new-types each holding an absolute `PathBuf` and exposing typed
    accessors:
    - `IssueRoot::new(repo_slug, issue_number)`,
    - `IssueRoot::root(&self)`,
    - `IssueRoot::main_agent_init_snapshot(&self)`,
    - `IssueRoot::plan_snapshot(&self)`,
    - `IssueRoot::plan_branch_ref(&self)`,
    - `IssueRoot::worktree_root(&self)`,
    - `SprintRoot::new(&IssueRoot, sprint)`,
    - `SprintRoot::root(&self)`,
    - `SprintRoot::subagent_init_snapshot(&self)`,
    - `SprintRoot::task_prompt(&self, task_id)`,
    - `SprintRoot::prompt_manifest(&self)`,
    - `SprintRoot::task_spec(&self)`,
    - `SprintRoot::dispatch_record(&self, task_id)`.
  - `pub fn ensure_dir(path: &Path) -> io::Result<()>` (or reuse one
    that already exists in the crate; check before adding).
  - `RuntimeLayoutError` enum: `AgentHomeNotSet`, `InvalidRepoSlug`,
    `InvalidTaskId`. Implement `Display` + `std::error::Error`.
  - The struct values produce **exactly** the paths in
    `RUNTIME_LAYOUT.md` lines 30-52 (do not mutate naming).
  Module is not yet wired into `execute.rs`; that cutover is Sprint
  2 + Sprint 3. Until then, `cargo build` must keep working with the
  module being dead code (`#[allow(dead_code)]` is acceptable inside
  the module body for the duration of Sprint 1).
- **Dependencies**:
  - `Task 1.1`
- **Complexity**: 6
- **Acceptance criteria**:
  - Module compiles under `cargo build -p nils-plan-issue-cli`.
  - All path methods return absolute paths under
    `$AGENT_HOME/out/plan-issue-delivery/<repo-slug>/issue-<n>/...`.
  - `RuntimeLayoutError::AgentHomeNotSet` triggers when `AGENT_HOME`
    is unset or empty.
  - `repo_slug("graysurf/plan-issue-smoke")` returns
    `graysurf__plan-issue-smoke`.
  - `dispatch_record("S1T1")` returns
    `<sprint_root>/manifests/dispatch-S1T1.json`.
- **Validation**:
  - `cargo build -p nils-plan-issue-cli`
  - `cargo test -p nils-plan-issue-cli runtime_layout::tests -- --list | rg '^test_'`
  - Required named tests (write all four; each a separate `#[test]`):
    - `cargo test -p nils-plan-issue-cli runtime_layout::tests::test_runtime_root_requires_agent_home -- --exact`
    - `cargo test -p nils-plan-issue-cli runtime_layout::tests::test_repo_slug_uses_double_underscore -- --exact`
    - `cargo test -p nils-plan-issue-cli runtime_layout::tests::test_issue_root_path_layout -- --exact`
    - `cargo test -p nils-plan-issue-cli runtime_layout::tests::test_sprint_root_path_layout -- --exact`

### Task 1.4: Sprint 1 acceptance artifact

- **Location**:
  - `$AGENT_HOME/out/plan-issue-cli/sprint-1/acceptance.md`
- **Description**: Write the Sprint 1 acceptance file containing
  command transcripts (`cargo build`, the four named tests, the spec
  rg validations) and `Result: PASS`.
- **Dependencies**:
  - `Task 1.1`
  - `Task 1.2`
  - `Task 1.3`
- **Complexity**: 1
- **Acceptance criteria**:
  - File exists with `Result: PASS`.
  - Lists every Sprint 1 validation command and its observed exit
    code.
- **Validation**:
  - `test -f "$AGENT_HOME/out/plan-issue-cli/sprint-1/acceptance.md"`
  - `rg -n '^Result: PASS$' "$AGENT_HOME/out/plan-issue-cli/sprint-1/acceptance.md"`

## Sprint 2: `start-plan` canonical artifacts

**Goal**: Cut `run_start_plan` over to the canonical `$ISSUE_ROOT`
layout. After this sprint, `plan-issue start-plan` and
`plan-issue-local start-plan` write `MAIN_AGENT_INIT_SNAPSHOT_PATH`,
`PLAN_BRANCH_REF_PATH`, the issue body, and the plan task-spec under
the nested layout; the old flat
`<plan-slug>-plan-tasks.tsv` / `<plan-slug>-plan-issue-body.md` paths
under `$AGENT_HOME/out/plan-issue-delivery/` are removed.
**Demo/Validation**:

- Command(s):
  - `cargo test -p nils-plan-issue-cli start_plan_emits_canonical_artifacts -- --exact`
  - `cargo test -p nils-plan-issue-cli start_plan_writes_plan_branch_ref -- --exact`
  - `cargo test -p nils-plan-issue-cli start_plan_local_uses_placeholder_issue -- --exact`
- Verify:
  - For a fixture plan + repo, `start-plan` produces only the
    canonical files; the old flat paths are no longer created.
  - The output JSON's `task_spec_path` and `issue_body_path` point
    inside `$ISSUE_ROOT`.
  - `plan-branch.ref` contents equal `plan/issue-<n>` (no trailing
    newline drama; canonical contract just says "one canonical branch
    name").

**PR grouping intent**: `per-sprint`
**Execution Profile**: `serial`

### Task 2.1: Wire `runtime_layout` into `run_start_plan`

- **Location**:
  - `crates/plan-issue-cli/src/execute.rs` (`run_start_plan` at line
    128, supporting helpers nearby)
  - `crates/plan-issue-cli/src/task_spec.rs` (`default_plan_task_spec_path`
    at line 531 — repurpose to take an `IssueRoot` or replace with a
    new helper)
  - `crates/plan-issue-cli/src/render.rs`
    (`default_plan_issue_body_path` — repurpose similarly)
- **Description**: Resolve `IssueRoot` at the top of `run_start_plan`
  using `--repo` (live mode requires it; for `plan-issue-local`, the
  repo override is mandatory and already validated by the existing
  CLI surface — re-confirm). Issue number resolution: live binary
  uses the GitHub-returned number; `plan-issue-local` uses the
  existing `LOCAL_ISSUE_PLACEHOLDER` (line 185). Plumb `IssueRoot`
  through to:
  - `task_spec_out` default → `IssueRoot::root() / "plan" / "tasks.tsv"`
    (or whatever the canonical filename ends up being — see Sprint 1
    spec; the canonical SKILL.md does not mandate a specific filename
    for the plan-scope task-spec, only `TASK_SPEC_PATH` for sprint
    scope, so name it consistently and document in the contract).
  - `issue_body_out` default →
    `IssueRoot::root() / "plan" / "issue-body.md"`.
  - Always also write `MAIN_AGENT_INIT_SNAPSHOT_PATH` by copying
    `$AGENT_HOME/prompts/plan-issue-delivery-main-agent-init.md` to
    `IssueRoot::main_agent_init_snapshot()`. If the source file is
    missing, fail with a runtime error variant
    `main-agent-init-snapshot-source-missing`.
  - Always also write `PLAN_BRANCH_REF_PATH` containing the canonical
    branch name `plan/issue-<n>` (no trailing newline; UTF-8). For
    dry-run, the placeholder issue number flows through unchanged.
  - Update the returned JSON to expose new keys:
    `main_agent_init_snapshot_path`, `plan_branch_ref_path`,
    `issue_root`. Existing keys (`task_spec_path`, `issue_body_path`,
    `record_count`, `issue_number`, `issue_url`, `labels`,
    `live_mutations_performed`) keep their semantics but their path
    values now sit under `$ISSUE_ROOT`.
  - Remove the call to `task_spec::default_plan_task_spec_path` /
    `render::default_plan_issue_body_path` flat-path implementations
    and either delete those helpers or repurpose them. If renaming,
    update every call site (search the crate).
- **Dependencies**:
  - `Task 1.3`
- **Complexity**: 7
- **Acceptance criteria**:
  - `run_start_plan` returns paths only under `$ISSUE_ROOT`.
  - The two pre-existing flat-path helpers are gone or repurposed
    (no production caller writes to the flat layout).
  - Returned JSON includes `main_agent_init_snapshot_path`,
    `plan_branch_ref_path`, `issue_root`.
  - Errors when `AGENT_HOME` is unset surface
    `RuntimeLayoutError::AgentHomeNotSet` and exit code 1 (runtime
    failure, not usage).
  - Live mode records the canonical branch name on disk only; it
    does not push the plan branch to the remote. Out-of-scope for
    this delivery.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli start_plan_emits_canonical_artifacts -- --exact`
  - `cargo test -p nils-plan-issue-cli start_plan_writes_plan_branch_ref -- --exact`
  - `cargo test -p nils-plan-issue-cli start_plan_local_uses_placeholder_issue -- --exact`
  - `cargo test -p nils-plan-issue-cli start_plan_fails_on_missing_main_agent_init_source -- --exact`

### Task 2.2: Refresh existing `start-plan` tests for the new layout

- **Location**:
  - `crates/plan-issue-cli/tests`
  - `crates/plan-issue-cli/tests/fixtures`
- **Description**: Update every existing `start-plan` integration
  test or fixture that asserts on the retired flat layout. Locate
  them with `rg -l 'plan-issue-delivery' crates/plan-issue-cli/tests`.
  Switch asserts to the canonical `$ISSUE_ROOT` layout. Where
  helpful, add a small test helper (in
  `crates/plan-issue-cli/tests/support/` if that module already
  exists, otherwise inline) that constructs a temp `AGENT_HOME` and
  asserts file presence under the canonical paths using
  `pretty_assertions::assert_eq` for path strings.
- **Dependencies**:
  - `Task 2.1`
- **Complexity**: 5
- **Acceptance criteria**:
  - `cargo nextest run -p nils-plan-issue-cli` is green after the
    test refresh.
  - No remaining test asserts the retired flat plan-tasks TSV or
    plan-issue-body Markdown filenames.
- **Validation**:
  - `cargo nextest run -p nils-plan-issue-cli`
  - `! rg -n 'plan-tasks\\.tsv|plan-issue-body\\.md' crates/plan-issue-cli/tests`

### Task 2.3: Sprint 2 acceptance artifact

- **Location**:
  - `$AGENT_HOME/out/plan-issue-cli/sprint-2/acceptance.md`
- **Description**: Write Sprint 2 acceptance with command
  transcripts and `Result: PASS`.
- **Dependencies**:
  - `Task 2.1`
  - `Task 2.2`
- **Complexity**: 1
- **Acceptance criteria**:
  - File exists with `Result: PASS`.
- **Validation**:
  - `test -f "$AGENT_HOME/out/plan-issue-cli/sprint-2/acceptance.md"`
  - `rg -n '^Result: PASS$' "$AGENT_HOME/out/plan-issue-cli/sprint-2/acceptance.md"`

## Sprint 3: `start-sprint` canonical artifacts

**Goal**: Cut `run_start_sprint` over to `$SPRINT_ROOT`. The biggest
sprint by far: every dispatch artifact the canonical contract
requires is emitted here.
**Demo/Validation**:

- Command(s):
  - `cargo test -p nils-plan-issue-cli start_sprint_emits_plan_snapshot -- --exact`
  - `cargo test -p nils-plan-issue-cli start_sprint_emits_subagent_init_snapshot -- --exact`
  - `cargo test -p nils-plan-issue-cli start_sprint_emits_dispatch_record_per_task -- --exact`
  - `cargo test -p nils-plan-issue-cli start_sprint_emits_prompt_manifest -- --exact`
  - `cargo test -p nils-plan-issue-cli start_sprint_relocates_task_prompt -- --exact`
  - `cargo test -p nils-plan-issue-cli dispatch_record_omits_runtime_adapter_keys -- --exact`
- Verify:
  - For a fixture sprint, the `$SPRINT_ROOT/` tree contains
    `plan.snapshot.md`, `prompts/plan-issue-delivery-subagent-init.snapshot.md`,
    `prompts/<TASK_ID>.md` per task, `manifests/prompt-manifest.tsv`,
    `manifests/dispatch-<TASK_ID>.json` per task, and `specs/sprint-task-spec.tsv`.
  - Each `dispatch-<TASK_ID>.json` parses as JSON with the eleven
    required keys present and the three runtime-adapter keys absent.
  - The retired flat sprint paths (sprint-tasks TSV and the
    sprint-subagent-prompts directory) are no longer produced.

**PR grouping intent**: `per-sprint`
**Execution Profile**: `serial`

### Task 3.1: Add JSON model for `dispatch-<TASK_ID>.json`

- **Location**:
  - `crates/plan-issue-cli/src/dispatch_record.rs` (new)
  - `crates/plan-issue-cli/src/lib.rs`
- **Description**: Define `DispatchRecord` with the eleven required
  keys (`task_id`, `task_prompt_path`, `subagent_init_snapshot_path`,
  `plan_snapshot_path`, `worktree`, `branch`, `execution_mode`,
  `pr_group`, `base_branch`, `workflow_role`) and serde-serialize
  it into a stable, sorted JSON object. Three optional adapter
  fields (`runtime_name`, `runtime_role`,
  `runtime_role_fallback_reason`) are intentionally **not** part of
  the struct because the binary never writes them; the wrapper /
  main agent edits the JSON post-emission to add them. Provide a
  helper `write_dispatch_record(path: &Path, record: &DispatchRecord)`
  that writes pretty JSON with a trailing newline.
- **Dependencies**:
  - `Task 1.3`
- **Complexity**: 5
- **Acceptance criteria**:
  - Struct + serializer compile.
  - JSON output uses snake-case keys exactly matching
    `RUNTIME_LAYOUT.md` L48-52.
  - `workflow_role` defaults to `"implementation"` for tasks emitted
    by `start-sprint` (review / monitor are dispatched ad-hoc by the
    main agent and have no per-task record at sprint start, per
    `AGENT_ROLE_MAPPING.md`).
  - Round-trip `serde_json::to_string` then `from_str` produces an
    equal value (proof the field set is self-consistent).
- **Validation**:
  - `cargo test -p nils-plan-issue-cli dispatch_record::tests -- --list | rg '^test_'`
  - `cargo test -p nils-plan-issue-cli dispatch_record::tests::test_serializes_required_keys -- --exact`
  - `cargo test -p nils-plan-issue-cli dispatch_record::tests::test_default_workflow_role_is_implementation -- --exact`
  - `cargo test -p nils-plan-issue-cli dispatch_record::tests::test_round_trip_equals -- --exact`

### Task 3.2: Wire `runtime_layout` and dispatch records into `run_start_sprint`

- **Location**:
  - `crates/plan-issue-cli/src/execute.rs` (`run_start_sprint` at
    line 862, `default_subagent_prompts_path` at line 1515,
    `write_subagent_prompts` at line 1528)
  - `crates/plan-issue-cli/src/task_spec.rs` (`default_sprint_task_spec_path`
    at line 544 — repurpose or replace)
  - `crates/plan-issue-cli/src/render.rs`
    (`default_sprint_comment_path` — repurpose if it currently uses
    the flat layout; otherwise keep)
- **Description**: After the existing `task_spec_rows_from_issue_rows`
  / `ensure_start_sprint_runtime_truth_matches_plan` flow:
  - Resolve `IssueRoot` (same way as Sprint 2.1) and `SprintRoot::new(&issue_root, sprint)`.
  - Materialize `$SPRINT_ROOT`, `$SPRINT_ROOT/prompts`,
    `$SPRINT_ROOT/manifests`, `$SPRINT_ROOT/specs` directories.
  - Copy `$AGENT_HOME/prompts/plan-issue-delivery-subagent-init.md`
    to `SubagentInitSnapshotPath`. Fail with
    `subagent-init-snapshot-source-missing` if absent.
  - Copy the source plan to `IssueRoot::plan_snapshot()` (idempotent;
    if the file already exists from a prior sprint of the same plan,
    overwrite — the canonical contract treats it as
    immutable-per-issue but does not forbid rewriting on resume).
  - Replace `default_subagent_prompts_path` /
    `default_sprint_task_spec_path` defaults so:
    - Sprint task-spec writes to `SprintRoot::task_spec()`.
    - Per-task prompts write to `SprintRoot::task_prompt(task_id)`.
    Update `write_subagent_prompts` to accept the `SprintRoot` (or a
    `&Path` that already names the canonical prompts dir) and emit
    one `<TASK_ID>.md` per task instead of the current
    `S?T?-subagent-prompt.md` flat file.
  - For each sprint task row, build a `DispatchRecord` (`base_branch`
    is `plan/issue-<n>` — read from `IssueRoot::plan_branch_ref()`
    if present, otherwise compute) and write it via
    `write_dispatch_record`.
  - Build `PROMPT_MANIFEST_PATH` (TSV with header
    `task_id\tprompt_path\texecution_mode\tworkflow_role`, one row
    per task) and write to `SprintRoot::prompt_manifest()`.
  - Update the returned JSON to include
    `plan_snapshot_path`, `subagent_init_snapshot_path`,
    `prompt_manifest_path`, `dispatch_record_paths` (sorted vec),
    `sprint_root`. Existing `subagent_prompts_out`,
    `subagent_prompt_files`, `task_spec_path` keep their meaning but
    point under `$SPRINT_ROOT`.
- **Dependencies**:
  - `Task 2.1`
  - `Task 3.1`
- **Complexity**: 9
- **Acceptance criteria**:
  - All canonical sprint artifacts are emitted in a single call.
  - `dispatch_record_paths.len() == artifact_rows.len()`.
  - Each emitted JSON parses and contains the required keys; none
    contain `runtime_name` / `runtime_role`.
  - `prompt-manifest.tsv` has a stable header and one row per task.
  - Old flat helpers (`default_subagent_prompts_path` flat output,
    `S?T?-subagent-prompt.md` filename pattern) are gone.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli start_sprint_emits_plan_snapshot -- --exact`
  - `cargo test -p nils-plan-issue-cli start_sprint_emits_subagent_init_snapshot -- --exact`
  - `cargo test -p nils-plan-issue-cli start_sprint_emits_dispatch_record_per_task -- --exact`
  - `cargo test -p nils-plan-issue-cli start_sprint_emits_prompt_manifest -- --exact`
  - `cargo test -p nils-plan-issue-cli start_sprint_relocates_task_prompt -- --exact`
  - `cargo test -p nils-plan-issue-cli dispatch_record_omits_runtime_adapter_keys -- --exact`

### Task 3.3: Refresh existing `start-sprint` tests for the new layout

- **Location**:
  - `crates/plan-issue-cli/tests`
  - `crates/plan-issue-cli/src/execute.rs`
- **Description**: Update every test that asserts on the retired
  flat layout to the canonical paths. Locate them with
  `rg -l 'subagent-prompts' crates/plan-issue-cli/tests`. The inline
  `#[cfg(test)] mod` in `execute.rs` (around lines 2687, 2706, 2720,
  2759) already exercises `write_subagent_prompts`; those tests must
  be updated to pass a `SprintRoot` (or canonical prompts dir) and
  verify the per-task Markdown filenames. Use `pretty_assertions` for
  path-set diffs.
- **Dependencies**:
  - `Task 3.2`
- **Complexity**: 5
- **Acceptance criteria**:
  - `cargo nextest run -p nils-plan-issue-cli` is green.
  - No remaining test asserts the retired
    `subagent-prompt.md` flat filename pattern.
- **Validation**:
  - `cargo nextest run -p nils-plan-issue-cli`
  - `! rg -n 'subagent-prompt\\.md' crates/plan-issue-cli/tests crates/plan-issue-cli/src/execute.rs`

### Task 3.4: Sprint 3 acceptance artifact

- **Location**:
  - `$AGENT_HOME/out/plan-issue-cli/sprint-3/acceptance.md`
- **Description**: Write Sprint 3 acceptance with command transcripts
  and `Result: PASS`.
- **Dependencies**:
  - `Task 3.2`
  - `Task 3.3`
- **Complexity**: 1
- **Acceptance criteria**:
  - File exists with `Result: PASS`.
- **Validation**:
  - `test -f "$AGENT_HOME/out/plan-issue-cli/sprint-3/acceptance.md"`
  - `rg -n '^Result: PASS$' "$AGENT_HOME/out/plan-issue-cli/sprint-3/acceptance.md"`

## Sprint 4: Parity, cutover docs, and release gate

**Goal**: Ship a parity fixture that pins the exact canonical layout
end-to-end, retire any leftover references to the flat layout in
docs, announce the breaking change, and pass every `DEVELOPMENT.md`
gate.
**Demo/Validation**:

- Command(s):
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh`
  - `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage`
  - `cargo test -p nils-plan-issue-cli runtime_layout_parity -- --exact`
- Verify:
  - End-to-end: `start-plan` followed by `start-sprint --sprint 1`
    against a fixture plan produces exactly the canonical artifact
    tree, byte-for-byte stable for snapshot files and structurally
    stable for JSON / TSV files.
  - Every doc reference in the crate to the flat layout has been
    removed or rewritten.
  - CHANGELOG announces the breaking change with a `BREAKING:`
    prefix and a migration note for any downstream consumer that was
    parsing the flat paths.

**PR grouping intent**: `per-sprint`
**Execution Profile**: `serial`

### Task 4.1: End-to-end runtime-layout parity test

- **Location**:
  - `crates/plan-issue-cli/tests/runtime_layout_parity.rs`
  - `crates/plan-issue-cli/tests/fixtures/runtime_layout`
- **Description**: Add the new parity test file and a fixture
  directory holding a small `plan.md`, an expected
  `dispatch-S1T1.json`, and an expected `prompt-manifest.tsv`. Set
  up a temp `AGENT_HOME` (with mock `prompts/` files for the two
  snapshot sources), run `plan-issue-local start-plan` then
  `plan-issue-local start-sprint --sprint 1` against the fixture,
  walk the resulting `$AGENT_HOME/out/...` tree, and
  `pretty_assertions::assert_eq!` the file set plus the
  byte-for-byte JSON / TSV outputs against fixtures. Use a temp env
  guard so the test isolates `AGENT_HOME` from the dev environment.
- **Dependencies**:
  - `Task 3.2`
- **Complexity**: 7
- **Acceptance criteria**:
  - Test asserts the exact file set under the canonical
    `$ISSUE_ROOT` after both commands run.
  - Test pins the dispatch-record JSON shape and the prompt-manifest
    TSV header.
  - Test runs offline (no `gh` calls).
- **Validation**:
  - `cargo test -p nils-plan-issue-cli runtime_layout_parity -- --list | rg '^test_'`
  - `cargo test -p nils-plan-issue-cli runtime_layout_parity -- --exact`

### Task 4.2: Crate README + CHANGELOG breaking-change announcement

- **Location**:
  - `crates/plan-issue-cli/README.md`
  - `crates/plan-issue-cli/CHANGELOG.md`
  - `crates/plan-issue-cli/docs/specs/plan-issue-cli-contract-v2.md`
- **Description**: Update README to describe the canonical artifact
  layout (replace any leftover flat-path examples). Add a CHANGELOG
  entry under the next version with a `BREAKING:` prefix that names
  the layout flip (nested repo-slug / issue / sprint directories),
  lists the retired flat filenames (plan-tasks TSV, plan-issue-body
  Markdown, sprint subagent-prompts directory, sprint task-spec
  TSV), states the migration note (the only known consumer is the
  `plan-issue-delivery` wrapper, which already expects the canonical
  layout), and links to this plan plus the agent-kit
  `RUNTIME_LAYOUT.md`. If the repo uses the workspace-level
  `CHANGELOG.md` instead of a per-crate one, add the entry there;
  decide during 4.2 by inspecting the existing file.
  Reconcile any drift between contract-v2 and shipped behaviour.
- **Dependencies**:
  - `Task 3.2`
  - `Task 4.1`
- **Complexity**: 3
- **Acceptance criteria**:
  - README shows only the canonical layout.
  - CHANGELOG has a `BREAKING:` entry referencing this plan.
  - Contract spec from Task 1.1 matches shipped behaviour exactly.
- **Validation**:
  - `rg -n 'BREAKING' crates/plan-issue-cli/CHANGELOG.md`
  - `! rg -n 'plan-tasks\\.tsv|plan-issue-body\\.md|subagent-prompts/' crates/plan-issue-cli/README.md crates/plan-issue-cli/docs`
  - `rg -n 'plan-issue-cli-canonical-runtime-artifacts-plan' crates/plan-issue-cli/CHANGELOG.md`

### Task 4.3: Run the canonical pre-delivery gate stack

- **Location**:
  - `scripts/ci/nils-cli-checks-entrypoint.sh`
- **Description**: Run `bash scripts/ci/nils-cli-checks-entrypoint.sh`
  and `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage`.
  Capture transcripts in the Sprint 4 acceptance file. Fix any
  failure surfaced (do not skip gates).
- **Dependencies**:
  - `Task 4.1`
  - `Task 4.2`
- **Complexity**: 2
- **Acceptance criteria**:
  - Both entrypoints exit `0`.
  - Coverage report is generated under the workspace's standard
    location.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh; echo rc=$?`
  - `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage; echo rc=$?`

### Task 4.4: Sprint 4 acceptance artifact

- **Location**:
  - `$AGENT_HOME/out/plan-issue-cli/sprint-4/acceptance.md`
- **Description**: Write Sprint 4 acceptance with the gate-stack
  transcripts (Task 4.3), a confirmation that the parity fixture
  (Task 4.1) is green, and `Result: PASS`.
- **Dependencies**:
  - `Task 4.1`
  - `Task 4.2`
  - `Task 4.3`
- **Complexity**: 1
- **Acceptance criteria**:
  - File exists with `Result: PASS`.
- **Validation**:
  - `test -f "$AGENT_HOME/out/plan-issue-cli/sprint-4/acceptance.md"`
  - `rg -n '^Result: PASS$' "$AGENT_HOME/out/plan-issue-cli/sprint-4/acceptance.md"`

## Testing Strategy

- Unit: `runtime_layout` module tests (pure path math + AGENT_HOME
  resolution); `dispatch_record` serializer tests (required keys,
  default `workflow_role`, round-trip).
- Integration: `start_plan_*` and `start_sprint_*` named tests that
  exercise the new emissions against a temp `AGENT_HOME`; refreshed
  legacy tests that previously asserted on the flat layout.
- E2E parity: `runtime_layout_parity.rs` walks the artifact tree
  produced by `plan-issue-local start-plan` + `start-sprint --sprint 1`
  end-to-end against checked-in fixtures.
- Pre-delivery: `bash scripts/ci/nils-cli-checks-entrypoint.sh` and
  the `--with-coverage` variant per `DEVELOPMENT.md`.

## Risks & gotchas

- **Test isolation around `$AGENT_HOME`**: The new tests must not
  pollute the dev `$AGENT_HOME`. Use `tempfile::TempDir` + a scoped
  env guard. Tests run in parallel by default under nextest; do not
  `std::env::set_var("AGENT_HOME", ...)` from one test if another
  reads it concurrently — pass an explicit `agent_home` argument
  through the helper API or serialize the relevant tests with
  `#[serial]` (the workspace already uses `serial_test` in some
  crates; confirm).
- **`plan-issue` live mode + `AGENT_HOME` unset**: The current binary
  produces output even if `AGENT_HOME` is unset (it derives the flat
  output dir from the plan slug, which does not need `AGENT_HOME`).
  The new layout hard-requires `AGENT_HOME`; `start-plan` must fail
  loud and early if it is missing, with exit code 1 and a clear
  error referencing the env var. Document the new requirement in
  the README and contract spec.
- **Snapshot source files missing**: Both
  `$AGENT_HOME/prompts/plan-issue-delivery-main-agent-init.md` and
  `...-subagent-init.md` ship in agent-kit. If a user has an older
  agent-kit checkout, the copies will fail. Surface explicit error
  variants (`main-agent-init-snapshot-source-missing`,
  `subagent-init-snapshot-source-missing`) and recommend
  `agent-kit baseline --check`.
- **Plan-snapshot rewrite on resume**: If a sprint is re-run (resume
  case), the snapshot files already exist. The canonical contract
  treats them as immutable-per-issue. Choose: (a) overwrite without
  diffing (simpler; matches "snapshot represents current source");
  (b) diff source vs existing snapshot and refuse on drift. The plan
  picks (a) but flags this for review during 3.2 — if the wrapper
  expects (b), adjust there.
- **Cargo workspace test runner**: `cargo nextest` is the
  expected runner per `DEVELOPMENT.md`. Plain `cargo test` may fail
  on shared fixtures or harness wiring; always use nextest for the
  integration crate-level tests.
- **Flat-path call sites buried in tests**: Any leftover
  `default_plan_task_spec_path` / `default_sprint_task_spec_path` /
  `default_subagent_prompts_path` reference outside the main code
  paths (for example, an integration test that called the helper
  directly) will silently break. Tasks 2.2 and 3.3 must use
  `rg -l` to locate every call site before touching either signature.
- **Multi-sprint resume**: If sprint `N+1` runs after sprint `N` was
  emitted, sprint `N+1`'s `SUBAGENT_INIT_SNAPSHOT_PATH` lives under
  `$SPRINT_ROOT(N+1)`, not `$SPRINT_ROOT(N)`. Each sprint gets its
  own copy. Test 3.2 should cover this implicitly via the parity
  fixture in 4.1.
- **`PROMPT_MANIFEST_PATH` filename**: Canonical SKILL.md L95
  describes columns but does not pin the filename. The plan picks
  `prompt-manifest.tsv` per `RUNTIME_LAYOUT.md` line 45; if the
  wrapper or any adapter assumes a different filename, reconcile
  during 3.2 review.
- **`workflow_role` for review / monitor records**: The plan emits
  `workflow_role=implementation` only at sprint start. Review and
  monitor dispatches are ad-hoc by the main agent and have no
  per-task record at start-sprint time. If a future change wants
  start-sprint to also emit pre-allocated review / monitor records,
  it is additive and out of scope here — call this out in the
  contract spec.
- **Branch reference value**: `plan/issue-<n>` is an example in
  RUNTIME_LAYOUT.md; the canonical contract does not pin the
  pattern. Picking `plan/issue-<n>` matches the example and keeps
  the wrapper SKILL's hint accurate. If a downstream user has a
  different convention, expose `--plan-branch-name <name>` in a
  later iteration; for this delivery, hard-code the canonical
  pattern.

## Rollback plan

- Revert the squash-merge of each sprint PR in reverse order
  (Sprint 4 → 3 → 2 → 1). The artifact layout flips back to flat
  the moment Sprint 2's PR is reverted; nothing on disk under the
  user's existing `$AGENT_HOME/out/plan-issue-delivery/` needs
  cleanup because the old flat paths and the new nested paths do
  not collide (different parents).
- Acceptance artifacts under `$AGENT_HOME/out/plan-issue-cli/sprint-N/`
  are runtime-only and can be deleted at any time without affecting
  the source tree.
- No DB / migrations / external service state involved.
- If only the spec changes from Sprint 1 are unwanted but the code
  is fine, revert just the doc commits in 1.1 and 1.2; the
  `runtime_layout` module from 1.3 stands alone.
