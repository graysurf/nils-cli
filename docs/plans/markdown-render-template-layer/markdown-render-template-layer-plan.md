# Plan: Markdown Render Template Layer

## Overview

Introduce a workspace-shared Markdown template layer (`nils-markdown`) built
on Tera so that long-form human-facing Markdown artifacts are produced by
`.md.tera` templates plus flat Rust view structs instead of inline
`format!`/`push_str` chains. Sprint 1 builds the shared library, extracts
the existing helpers from `agent-runtime-cli`, and lifts the golden-test
harness without changing behavior. Sprint 2 migrates the nine Tier A
artifacts across `plan-issue-cli` and `agent-workflow-primitives`, one PR
per artifact, in the documented order. Sprint 3 adds the `md-render`
binary target so `agent-runtime-kit` skills and non-Rust agents can drive
the same engine over JSON input.

## Read First

- Primary source:
  docs/plans/markdown-render-template-layer/markdown-render-template-layer-discussion-source.md
- Source type: discussion-to-implementation-doc
- Open questions carried into execution: none

## Scope

- In scope:
  - New `crates/nils-markdown` workspace member shipping a Tera engine
    builder, the migrated helpers (`skill_ref`, `cli_ref`, `state_out`,
    `script`), a new `md_cell` filter wrapping
    `nils_common::markdown::canonicalize_table_cell`, and a golden-test
    harness.
  - Migration of `crates/agent-runtime-cli` from its in-crate render
    helpers to `nils-markdown` with no behavior change.
  - Migration of nine Tier A artifacts to `.md.tera` templates plus flat
    view structs; one PR per artifact, in the source-document order.
  - Per-artifact golden tests under the shared harness, asserting
    byte-equality against captured pre-migration output.
  - New `md-render` Cargo binary target inside `crates/nils-markdown`
    behind a `bin-cli` Cargo feature, plus completion assets,
    workspace-bins registration, and `release/crates-io-publish-order.txt`
    entry.
- Out of scope:
  - Migrating commit-message composition (`semantic-commit`,
    `git-cli/src/commit.rs` message body path).
  - Migrating Tier B artifacts (`agent-docs` scaffold templates,
    `plan-tooling` scaffold template, `api-testing-core` reports,
    `git-cli` diff context). Tier B is migrated opportunistically in
    follow-up plans once Tier A patterns are proven.
  - Migrating small Markdown emitters in `codex-cli`, `gemini-cli`,
    `git-summary`, `api-websocket`, `api-gql`, `macos-agent`.
  - Changing public CLI contracts (command names, exit codes, JSON
    schemas, help text) of any touched binary.
  - Replacing review evidence JSON schemas with template output.
  - Adding non-Tera template engines or HTML rendering.

## Assumptions

1. Workspace builds and tests run under
   `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh`.
2. `pretty_assertions` is acceptable for golden diff output (already
   used by `crates/nils-common/src/markdown.rs` and
   `crates/agent-runtime-cli/src/render/golden.rs`).
3. `include_str!` is the canonical mechanism for bundling `.md.tera`
   assets into the consumer crate (Decision 13 in the source document).
4. `nils-markdown` is publishable; the release-order list places it
   after `nils-common` and `nils-term` and before every consumer.
5. The existing four helpers in
   `crates/agent-runtime-cli/src/render/helpers/`
   (`cli_ref / script / skill_ref / state_out`) are domain-specific
   to agent-runtime-cli (they bind `ManifestSet`, `Skill`,
   `StateOutMode`, `CliToolsManifest`); `nils-markdown` exposes a
   generic `Engine::register_helper(name, F)` extension point that
   `agent-runtime-cli` consumes to re-register them in-place. The
   helper bodies stay where they are; only the engine construction
   site (`crates/agent-runtime-cli/src/render/writer.rs`) and the
   helpers' `register_all` site move from `tera::Tera` to
   `nils_markdown::Engine`.

## Sprint 1: nils-markdown foundation

**Goal**: Land `nils-markdown` with the engine builder, the `md_cell`
filter, a re-export bridge to `nils_common::markdown::*`, a generic
`register_helper` extension point, and a new byte-equality
`assert_render` harness. Migrate `agent-runtime-cli` to build its
Tera engine through `nils_markdown::Engine` and to register its
existing domain-specific helpers
(`cli_ref / script / skill_ref / state_out`) via the new extension
point. The four agent-runtime-cli helpers stay in
`agent-runtime-cli` because they depend on its manifest domain
(`ManifestSet`, `Skill`, `StateOutMode`, `CliToolsManifest`); moving
them into `nils-markdown` would force the lowest layer to depend on
agent-runtime-cli domain types. Existing `agent-runtime-cli` render
output and golden fixtures stay byte-identical.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-markdown`
  - `cargo test -p agent-runtime-cli`
  - `cargo build -p nils-markdown --features bin-cli` (compile-only at
    this sprint; the binary main lands in Sprint 3 but the feature gate
    is wired up so the manifest is final)
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
- Verify: `agent-runtime-cli` golden tests pass without modification;
  `nils-markdown` is on the workspace member list and in
  `release/crates-io-publish-order.txt` between `nils-term` and
  `agent-runtime-cli`.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 1.1: Scaffold `nils-markdown` crate

- **Location**:
  - crates/nils-markdown/Cargo.toml (new)
  - crates/nils-markdown/src/lib.rs (new)
  - crates/nils-markdown/README.md (new)
  - Cargo.toml (workspace `members`)
  - release/crates-io-publish-order.txt
- **Description**: Add a publishable workspace member named
  `nils-markdown`. Cargo manifest pins workspace-managed `tera`,
  `serde` (with `derive`), `serde_json`, `indexmap`, `thiserror`,
  `nils-common` (path dep). Dev-deps add `pretty_assertions`,
  `tempfile`, `nils-test-support`. A `bin-cli` Cargo feature is
  defined and pulls workspace `clap` and `anyhow` as optional deps
  (`dep:clap`, `dep:anyhow`); the `[[bin]]` manifest target itself is
  not declared in Sprint 1 (the `md-render` binary, its source tree,
  and the workspace-bins registration land together in Sprint 3 Task
  3.1 to avoid registering a no-op binary in cargo metadata). Update
  `release/crates-io-publish-order.txt` to list `nils-markdown`
  immediately after `nils-term` and `nils-common`.
- **Dependencies**:
  - none
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - `cargo build -p nils-markdown` succeeds.
  - `cargo build -p nils-markdown --features bin-cli` succeeds.
  - `bash scripts/workspace-bins.sh` does not list `md-render`
    (the `[[bin]]` manifest target lands in Sprint 3 Task 3.1; this
    keeps cargo metadata's bin list free of no-op binaries).
  - `bash scripts/publish-crates.sh --dry-run --crates "nils-common nils-term nils-markdown"`
    succeeds.
- **Validation**:
  - `cargo build -p nils-markdown`
  - `cargo build -p nils-markdown --features bin-cli`
  - `bash scripts/publish-crates.sh --dry-run --crates "nils-common nils-term nils-markdown"`

### Task 1.2: Engine builder and `RenderError`

- **Location**:
  - crates/nils-markdown/src/engine.rs (new)
  - crates/nils-markdown/src/error.rs (new)
  - crates/nils-markdown/src/lib.rs
- **Description**: Implement `Engine::builder()` returning a Tera
  instance configured for determinism (no auto-escape, fixed
  iteration order, no `now()` function registered). Expose
  `Engine::register_template(name, body)` for `include_str!` bundles
  and `Engine::render_value(name, &serde_json::Value)` plus
  `Engine::render<T: Serialize>(name, &T)`. Define `RenderError` with
  `thiserror` covering template-parse, missing-template,
  serialization, and Tera-render variants.
- **Dependencies**:
  - Task 1.1
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - `Engine` rejects registration of templates containing dynamic
    `now()` calls (matches the determinism rule already enforced by
    `agent-runtime-cli`).
  - `RenderError` is `Send + Sync + 'static` and round-trips through
    `anyhow::Error`.
  - Unit tests cover engine build, template registration, value
    rendering, struct rendering, and each error variant.
- **Validation**:
  - `cargo test -p nils-markdown engine`
  - `cargo test -p nils-markdown error`

### Task 1.3: `md_cell` filter and helper bridge

- **Location**:
  - crates/nils-markdown/src/filters.rs (new)
  - crates/nils-markdown/src/lib.rs
- **Description**: Register a `md_cell` Tera filter that wraps
  `nils_common::markdown::canonicalize_table_cell`. Re-export the
  existing `nils_common::markdown` helpers (`heading`, `code_block`,
  `validate_markdown_payload`, `format_json_pretty_sorted`) at the
  `nils_markdown::helpers` path so consumers have one import surface.
- **Dependencies**:
  - Task 1.2
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - A template with `{{ value | md_cell }}` escapes pipes and embedded
    newlines using the exact rule
    `nils_common::markdown::canonicalize_table_cell` already enforces.
  - `nils_markdown::helpers::heading` and friends call
    `nils_common::markdown::*` directly (no re-implementation).
  - Unit tests cover pipe escape, newline collapse, and empty input.
- **Validation**:
  - `cargo test -p nils-markdown filters`
  - `cargo test -p nils-markdown helpers`

### Task 1.4: Migrate `agent-runtime-cli` engine construction to `nils-markdown`

- **Location**:
  - crates/nils-markdown/src/engine.rs
  - crates/agent-runtime-cli/Cargo.toml
  - crates/agent-runtime-cli/src/render/writer.rs
  - crates/agent-runtime-cli/src/render/helpers/mod.rs
- **Description**: Expose
  `Engine::register_helper(name: &str, f: F) where F: tera::Function + 'static`
  on `nils-markdown` so consumers can attach domain-specific Tera
  helpers without `nils-markdown` knowing the consumer's domain.
  Migrate `agent-runtime-cli/src/render/writer.rs` to construct its
  engine through `nils_markdown::Engine::builder()` and migrate
  `agent-runtime-cli/src/render/helpers/mod.rs::register_all` to call
  `Engine::register_helper` instead of `Tera::register_function`. The
  four agent-runtime-cli helper bodies
  (`cli_ref / script / skill_ref / state_out`) and their `HelperContext`
  stay in `agent-runtime-cli`; only the engine construction site and
  the registration call site change. No behavior change to rendered
  output. `agent-runtime-cli/Cargo.toml` adds a path dep on
  `nils-markdown`.
- **Dependencies**:
  - Task 1.3
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - `Engine::register_helper` is publicly exported from `nils-markdown`
    and accepts any `tera::Function + 'static`.
  - `agent-runtime-cli/src/render/writer.rs` constructs its engine via
    `nils_markdown::Engine::builder()` and no longer calls
    `Tera::default()` for the render path.
  - `agent-runtime-cli/src/render/helpers/mod.rs::register_all` takes a
    `&mut nils_markdown::Engine` (or equivalent) and registers each
    helper through `Engine::register_helper`.
  - `cargo test -p agent-runtime-cli` passes without modification to
    its golden fixtures.
  - `grep -n 'tera::Tera\|use tera::Tera' crates/agent-runtime-cli/src/render/writer.rs`
    returns nothing.
- **Validation**:
  - `cargo test -p nils-markdown engine`
  - `cargo test -p agent-runtime-cli`

### Task 1.5: New byte-equality `assert_render` harness

- **Location**:
  - crates/nils-markdown/src/golden.rs (new)
  - crates/nils-markdown/tests/golden_smoke.rs (new)
- **Description**: Add a new byte-equality assertion helper at
  `nils_markdown::golden::assert_render(fixture_path: &Path, engine: &Engine, template_name: &str, view: &impl Serialize)`.
  The helper registers the named template body if needed, renders
  against `view`, reads the fixture file from disk, and asserts
  byte-for-byte equality with `pretty_assertions`. This is a new
  utility, not a lift: `crates/agent-runtime-cli/src/render/golden.rs`
  is the `--update-golden` mode that copies rendered files from
  `build/<product>/` into `tests/golden/<product>/expected/`; it
  is a different responsibility (fixture refresh, not per-template
  byte-equality assertion) and stays in `agent-runtime-cli`
  unchanged. Sprint 2 Tier A migrations consume `assert_render`;
  agent-runtime-cli does not need to switch its existing golden
  tests in Sprint 1.
- **Dependencies**:
  - Task 1.4
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - `nils_markdown::golden::assert_render` is publicly exported and
    accepts any `serde::Serialize` view.
  - `crates/nils-markdown/tests/golden_smoke.rs` renders a fixture
    template against a view struct, reads a captured `.golden.md`
    file, and asserts byte equality (positive case) plus a negative
    case that surfaces a `pretty_assertions` diff.
  - `crates/agent-runtime-cli/src/render/golden.rs` is unchanged.
- **Validation**:
  - `cargo test -p nils-markdown golden`
  - `cargo test -p nils-markdown --test golden_smoke`

### Task 1.6: Workspace gate

- **Location**:
  - whole workspace
- **Description**: After Tasks 1.1–1.5 land in the Sprint 1 PR, run
  the full workspace gate to prove no behavior drifted.
- **Dependencies**:
  - Task 1.1
  - Task 1.2
  - Task 1.3
  - Task 1.4
  - Task 1.5
- **Complexity**:
  - 1
- **Acceptance criteria**:
  - `cargo nextest run --workspace` passes.
  - `cargo clippy --workspace --all-targets -- -D warnings` passes.
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` passes.
- **Validation**:
  - `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh`

## Sprint 2: Tier A migration

**Goal**: Migrate all nine Tier A artifacts to `.md.tera` templates
plus flat view structs. One PR per artifact, in source-document
order. Every PR lands a byte-equality golden test against captured
pre-migration output.

**Demo/Validation**:

- Commands:
  - `cargo test -p plan-issue-cli` after each plan-issue-cli PR
  - `cargo test -p agent-workflow-primitives` after each AWP PR
  - `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh`
    after each PR
- Verify: each migrated artifact produces byte-identical output to its
  pre-migration capture, the consumer crate's `templates/` directory
  contains the `.md.tera`, and no Rust call site invokes
  `canonicalize_table_cell` directly for migrated artifacts.

**PR grouping intent**: `per-sprint`
**Execution Profile**: `serial`

### Task 2.1: Migrate `plan-issue-cli/src/issue_body.rs`

- **Location**:
  - crates/plan-issue-cli/src/issue_body.rs
  - crates/plan-issue-cli/templates/issue_body.md.tera (new)
  - crates/plan-issue-cli/Cargo.toml (add `nils-markdown` dep)
  - crates/plan-issue-cli/tests/golden/issue_body/ (new fixture dir)
- **Description**: Smallest Tier A artifact (21 inline Markdown
  sites). Extract a flat `IssueBodyView` struct, write
  `issue_body.md.tera`, render through `nils-markdown::Engine`.
  Capture pre-migration output as a golden fixture before changing
  the function body.
- **Dependencies**:
  - Task 1.6
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - The function previously returning the inline string returns the
    rendered template output byte-for-byte.
  - Golden test `tests/golden/issue_body/*.golden.md` is byte-equal
    to a representative capture.
  - No Rust code in the migrated path calls
    `canonicalize_table_cell`; pipes are escaped via `| md_cell` in
    the template.
- **Validation**:
  - `cargo test -p plan-issue-cli issue_body`
  - `cargo test -p plan-issue-cli --test golden`

### Task 2.2: Confirm `plan-issue-cli/src/task_spec.rs` TSV / Markdown boundary

- **Location**:
  - crates/plan-issue-cli/src/task_spec.rs
- **Description**: Pre-execution scoping for Sprint 2 found that
  `task_spec.rs` is a TSV emitter, not a Markdown emitter: its only
  string output is `render_tsv`, and the two
  `nils_common::markdown::canonicalize_table_cell` calls inside
  (lines around `notes.join("; ")` and `notes: ...row.notes`) make
  the resulting `TaskSpecRow.notes` field TSV-cell-safe (newline
  collapse, pipe escape). The original plan's "28 inline Markdown
  sites" count does not match the file. Task 2.2 is now an audit and
  documentation step: confirm no Markdown emission lives here, add
  a module-level doc note so future readers do not confuse the
  pre-canonicalization with Markdown escaping, and rely on
  `canonicalize_table_cell` idempotency
  ([`nils-common` markdown helpers contract v1]) so Task 2.4's
  template-side `| md_cell` filter does not double-escape the same
  notes downstream in `render::render_plan_issue_body`.
- **Dependencies**:
  - Task 2.1
- **Complexity**:
  - 1
- **Acceptance criteria**:
  - `task_spec.rs` carries a module-level doc note that calls out
    the TSV-only contract and the
    `canonicalize_table_cell` idempotency with `md_cell`.
  - No production behavior change: `render_tsv` output stays
    byte-stable for all inputs.
  - `cargo test -p nils-plan-issue-cli` stays green.
- **Validation**:
  - `cargo test -p nils-plan-issue-cli`

### Task 2.3: Migrate `plan-issue-cli/src/lifecycle_vnext/templates.rs`

- **Location**:
  - crates/plan-issue-cli/src/lifecycle_vnext/templates.rs
  - crates/plan-issue-cli/templates/lifecycle_vnext.md.tera (new)
  - crates/plan-issue-cli/tests/golden/lifecycle_vnext/ (new fixture dir)
- **Description**: Closes the name-mismatch noted in source-document
  Decision 15. The Rust module retains only view-struct definitions
  and `Engine` glue after this task.
- **Dependencies**:
  - Task 2.2
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - All vNext lifecycle comment shapes render byte-identically.
  - `crates/plan-issue-cli/src/lifecycle_vnext/templates.rs` contains
    only view structs and a `render(view: &View) -> Result<String>`
    function per shape; no inline `format!` of Markdown bodies.
- **Validation**:
  - `cargo test -p plan-issue-cli lifecycle_vnext`

### Task 2.4: Migrate `plan-issue-cli/src/render.rs`

- **Location**:
  - crates/plan-issue-cli/src/render.rs
  - crates/plan-issue-cli/templates/render/plan_issue_body.md.tera (new)
  - crates/plan-issue-cli/templates/render/sprint_comment.md.tera (new)
  - crates/plan-issue-cli/tests/golden/render/ (new fixture dir)
- **Description**: Provider-visible Markdown emitters in render.rs
  (549 lines, ~21 `format!`/`out.push` sites): `render_plan_issue_body`
  (initial plan-issue body, including the Task 2.1 task-decomposition
  table) and `render_sprint_comment` (sprint Start / Ready / Accepted
  comments). The discussion source calls these "dashboards"; the file
  itself names them `render_plan_issue_body` and
  `render_sprint_comment`. The actual dashboard (`render_dashboard`,
  `render_dashboard_from_audit`) lives in `lifecycle_record.rs` and is
  Task 2.5's scope. This task also wires `render_plan_issue_body` to
  reuse the `render_task_decomposition_block` helper introduced in
  Task 2.1 (closes the dead-code allowance there).
- **Dependencies**:
  - Task 2.3
- **Complexity**:
  - 6
- **Acceptance criteria**:
  - `render_plan_issue_body` output byte-identical to capture for at
    least three representative inputs (no rows, mixed-status rows,
    all-done rows).
  - `render_sprint_comment` output byte-identical to capture for
    Sprint Start / Ready / Accepted modes.
- **Validation**:
  - `cargo test -p plan-issue-cli render`
  - `cargo test -p plan-issue-cli --test golden_render`

### Task 2.5: Migrate the dashboard renderers in `lifecycle_record.rs`

- **Location**:
  - crates/plan-issue-cli/src/lifecycle_record.rs
  - crates/plan-issue-cli/templates/lifecycle_record/dashboard.md.tera (new)
  - crates/plan-issue-cli/tests/golden/lifecycle_record/ (new fixture dir)
- **Description**: First slice of the largest plan-issue-cli surface
  (3108 lines, ~200 inline Markdown sites). Migrates the two
  dashboard emitters that share an identical provider-visible
  shape: `render_dashboard` (used by `record open` to seed the
  issue body) and `render_dashboard_from_audit` (used by
  `record attach`, `record post`, and dashboard repair to rebuild
  the body from audit evidence). Both feed a single
  `dashboard.md.tera` through a shared `DashboardView` struct. The
  remaining renderers in `lifecycle_record.rs` (snapshot comments,
  post comments, the five kind-specific visible-content helpers)
  ship in Task 2.5b so each PR stays reviewable. Phase names follow
  the source document's lifecycle kinds (source / plan / state /
  session / validation / review / closeout) rather than the
  placeholder "open / progress / review / delivery / close" wording
  from the original plan.
- **Dependencies**:
  - Task 2.4
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - Three byte-identical golden fixtures (`dashboard_pending`,
    `dashboard_full`, `dashboard_complete`) covering the conditional
    branches (`show_review` and `tracker_block` populated).
  - `render_dashboard` and `render_dashboard_from_audit` collapse
    inline `format!`/`out.push` chains into `DashboardView` struct
    construction only.
- **Validation**:
  - `cargo test -p plan-issue-cli lifecycle_record`
  - `cargo test -p plan-issue-cli --test golden_lifecycle_record`

### Task 2.5b: Migrate the snapshot, post-comment, and kind helpers in `lifecycle_record.rs`

- **Location**:
  - crates/plan-issue-cli/src/lifecycle_record.rs
  - crates/plan-issue-cli/templates/lifecycle_record/snapshot.md.tera (new)
  - crates/plan-issue-cli/templates/lifecycle_record/post_comment.md.tera (new)
  - crates/plan-issue-cli/templates/lifecycle_record/state.md.tera (new)
  - crates/plan-issue-cli/templates/lifecycle_record/session.md.tera (new)
  - crates/plan-issue-cli/templates/lifecycle_record/validation.md.tera (new)
  - crates/plan-issue-cli/templates/lifecycle_record/review.md.tera (new)
  - crates/plan-issue-cli/templates/lifecycle_record/closeout.md.tera (new)
  - crates/plan-issue-cli/tests/golden/lifecycle_record/ (extends Task 2.5 dir)
- **Description**: Second slice of the lifecycle_record migration.
  Migrates `render_record_snapshot_comment` (source/plan kinds) to
  `snapshot.md.tera`, `render_record_post_comment_with_display`
  (state/session/validation/review/closeout) to
  `post_comment.md.tera`, and each kind-specific
  `render_*_payload_visible` helper to its own per-kind template
  (`state.md.tera`, `session.md.tera`, ...). The Rust helpers
  collapse into view-struct preparation only after this task.
- **Dependencies**:
  - Task 2.5
- **Complexity**:
  - 6
- **Acceptance criteria**:
  - Byte-identical golden fixtures for
    `render_record_snapshot_comment` (source + plan kinds) and
    `render_record_post_comment_with_display` (one per kind: state,
    session, validation, review, closeout), plus at least one
    populated-and-empty pair for the conditional sections inside
    each kind helper.
  - The `render_*_payload_visible` helpers collapse inline string-
    building into view-struct preparation only.
- **Validation**:
  - `cargo test -p plan-issue-cli lifecycle_record`
  - `cargo test -p plan-issue-cli --test golden_lifecycle_record`

### Task 2.6: Migrate `plan-issue-cli/src/execute.rs`

- **Location**:
  - crates/plan-issue-cli/src/execute.rs
  - crates/plan-issue-cli/templates/execute/plan_status_comment.md.tera (new)
  - crates/plan-issue-cli/templates/execute/subagent_prompt.md.tera (new)
  - crates/plan-issue-cli/tests/golden/execute/ (new fixture dir)
- **Description**: execute.rs has 151 `format!`/`push` sites across
  6121 lines, but most are JSON value construction, file paths,
  error/log strings, and `KEY=value` dry-run guides — not
  provider-visible Markdown. Only two sites emit Markdown to
  providers or to-disk consumers and need template migration:
  `render_plan_status_comment` (5-line plan-status snapshot used by
  the local previews) and the inline subagent task-prompt body
  inside `write_subagent_prompts` (header + metadata bullets +
  Lane Tasks section, written to disk per subagent lane). The
  placeholder templates from the original plan
  (`state_update.md.tera`, `follow_up.md.tera`) referred to
  emitters that do not exist in this file; the actual checkpoint
  state/session/validation/review comments are produced by
  `lifecycle_record::render_record_post_comment_with_display`
  (already templated in Tasks 2.5 / 2.5b) and are not re-emitted in
  `execute.rs`. Start only after Task 2.5b lands; the formal
  dependency stays "Task 2.5" because plan-tooling validation only
  accepts `Task N.M` references.
- **Dependencies**:
  - Task 2.5
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - `render_plan_status_comment` output byte-identical to capture
    for representative inputs (empty rows, mixed-status rows).
  - Subagent prompt body output byte-identical to capture for at
    least one task row.
- **Validation**:
  - `cargo test -p plan-issue-cli execute`
  - `cargo test -p plan-issue-cli --test golden_execute`

### Task 2.7: Migrate `agent-workflow-primitives/src/review_specialists.rs`

- **Location**:
  - crates/agent-workflow-primitives/src/review_specialists.rs
  - crates/agent-workflow-primitives/templates/review_specialists.md.tera (new)
  - crates/agent-workflow-primitives/Cargo.toml (add `nils-markdown` dep)
  - crates/agent-workflow-primitives/tests/golden/review_specialists/ (new fixture dir)
- **Description**: First cross-crate consumer; proves the layer is
  reusable outside `plan-issue-cli`. 59 inline sites.
- **Dependencies**:
  - Task 2.6
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - Merged specialist report output byte-identical to capture.
- **Validation**:
  - `cargo test -p agent-workflow-primitives review_specialists`

### Task 2.8: Migrate `agent-workflow-primitives/src/repo_retro.rs`

- **Location**:
  - crates/agent-workflow-primitives/src/repo_retro.rs
  - crates/agent-workflow-primitives/templates/repo_retro.md.tera (new)
  - crates/agent-workflow-primitives/tests/golden/repo_retro/ (new fixture dir)
- **Description**: 1958-line file, stable section layout. Rich
  view-to-template mapping.
- **Dependencies**:
  - Task 2.7
- **Complexity**:
  - 7
- **Acceptance criteria**:
  - Retro report output byte-identical to capture for at least two
    representative repo histories.
- **Validation**:
  - `cargo test -p agent-workflow-primitives repo_retro`

### Task 2.9: Migrate `agent-workflow-primitives/src/heuristic_inbox.rs`

- **Location**:
  - crates/agent-workflow-primitives/src/heuristic_inbox.rs
  - crates/agent-workflow-primitives/templates/heuristic_inbox/open.md.tera (new)
  - crates/agent-workflow-primitives/templates/heuristic_inbox/promoted.md.tera (new)
  - crates/agent-workflow-primitives/templates/heuristic_inbox/wontfix.md.tera (new)
  - crates/agent-workflow-primitives/tests/golden/heuristic_inbox/ (new fixture dir)
- **Description**: 2611-line file, strict section schema. Largest
  and most schema-strict artifact; migrated last so the layer's
  helpers and golden harness are battle-tested.
- **Dependencies**:
  - Task 2.8
- **Complexity**:
  - 8
- **Acceptance criteria**:
  - Heuristic-system entries render byte-identically to capture for
    at least three entry shapes (open / promoted / wontfix).
  - Section ordering matches the existing schema exactly.
- **Validation**:
  - `cargo test -p agent-workflow-primitives heuristic_inbox`

## Sprint 3: md-render binary

**Goal**: Land the `md-render` binary inside `nils-markdown` so
`agent-runtime-kit` skills and non-Rust agents can render templates
against JSON input. View-struct shape is treated as opaque
`serde_json::Value`; no per-template schema is exposed.

**Demo/Validation**:

- Commands:
  - `cargo build -p nils-markdown --features bin-cli`
  - `cargo test -p nils-markdown --features bin-cli bin`
  - `cargo run -p nils-markdown --features bin-cli --bin md-render -- --help`
  - `bash scripts/workspace-bins.sh | grep md-render`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
- Verify: `md-render --template <path.md.tera> --data <data.json>`
  reads the template and JSON, renders through `Engine::render_value`,
  and writes stdout; completion assets exist; binary is registered in
  workspace-bins and CHANGELOG.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 3.1: Implement `md-render` binary main

- **Location**:
  - crates/nils-markdown/src/bin/md-render/main.rs (new)
  - crates/nils-markdown/src/bin/md-render/cli.rs (new)
  - crates/nils-markdown/Cargo.toml
- **Description**: Implement the clap derive CLI:
  `md-render --template <PATH> --data <PATH> [--strict-determinism]
  [--format text|json]`. The binary loads template body, registers it
  under the file stem name, deserializes the data file as
  `serde_json::Value`, calls `Engine::render_value`, and writes to
  stdout. Error envelope follows `nils_common::cli_contract`.
- **Dependencies**:
  - Task 2.9
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - `md-render --help` prints the expected interface.
  - Exit codes follow `nils_common::cli_contract::exit::*`.
  - JSON output emits a `cli.md-render.render.v1` envelope when
    `--format json` is set.
  - Integration test renders a fixture template against a fixture
    JSON file and asserts byte equality.
- **Validation**:
  - `cargo test -p nils-markdown --features bin-cli bin`
  - `cargo run -p nils-markdown --features bin-cli --bin md-render -- --help`

### Task 3.2: Completion assets and workspace registration

- **Location**:
  - completions/bash/md-render (new)
  - completions/zsh/_md-render (new)
  - scripts/workspace-bins.sh (no source change; just verify
    discovery picks up the new binary)
- **Description**: Generate completions via the binary's
  `completion bash|zsh` subcommand per
  `docs/runbooks/cli-completion-development-standard.md` and commit
  the static outputs.
- **Dependencies**:
  - Task 3.1
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - `bash -n completions/bash/md-render` passes.
  - `zsh -n completions/zsh/_md-render` passes.
  - `bash scripts/workspace-bins.sh` reports `md-render`.
- **Validation**:
  - `bash -n completions/bash/md-render`
  - `zsh -n completions/zsh/_md-render`
  - `bash scripts/workspace-bins.sh`

### Task 3.3: README + CHANGELOG + agent-docs entry

- **Location**:
  - crates/nils-markdown/README.md
  - crates/nils-markdown/CHANGELOG.md (new)
  - AGENT_DOCS.toml (entry for `md-render`)
- **Description**: Document the binary in the crate README, add an
  initial CHANGELOG row aligned with the workspace version, and
  register `md-render` in `AGENT_DOCS.toml` so `agent-docs` resolves
  it for skill discovery.
- **Dependencies**:
  - Task 3.2
- **Complexity**:
  - 2
- **Acceptance criteria**:
  - `agent-docs resolve --context task-tools --strict --format checklist`
    lists `md-render` as `status=present`.
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` passes.
- **Validation**:
  - `agent-docs resolve --context task-tools --strict --format checklist`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`

### Task 3.4: Promote design principle to runbook

- **Location**:
  - docs/runbooks/markdown-template-development-standard.md (new)
- **Description**: Promote the design principle ("templates carry
  layout only; data prepared as flat structs") and the golden-test
  pattern into a permanent runbook. This is the source-document
  retention payoff.
- **Dependencies**:
  - Task 3.3
- **Complexity**:
  - 2
- **Acceptance criteria**:
  - Runbook documents the principle, the `md_cell` filter, the
    golden-test harness usage, and the view-struct preparation rule.
  - `rumdl check docs/runbooks/markdown-template-development-standard.md`
    passes.
- **Validation**:
  - `rumdl check docs/runbooks/markdown-template-development-standard.md`

## Testing Strategy

- Every Tier A migration captures pre-migration output to a golden
  fixture **before** changing the function body; the same PR adds the
  byte-equality assertion.
- Sprint 1 reuses `agent-runtime-cli` golden fixtures unchanged to
  detect any regression introduced by the helper relocation.
- `cargo nextest run --workspace` is the gate run at the end of each
  sprint.
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` runs in
  every Sprint 1 / Sprint 3 PR; full-workspace runs at sprint
  boundaries.
- Sprint 3 adds an integration test that drives the `md-render`
  binary via a `tempfile` scratch dir to prove the JSON-input path.

## Risks & gotchas

- **Byte stability of provider-visible artifacts**. Plan-issue
  dashboards, lifecycle comments, and heuristic-inbox entries land in
  provider issues read by humans and audited by `record audit`. Land
  golden tests in the same change set as each migration; never split.
- **Tera template parse errors are opaque**. Keep view structs flat,
  named, and `serde::Serialize` so Tera's error messages identify the
  missing field by name.
- **Determinism drift**. The shared engine must keep the determinism
  rules `agent-runtime-cli` already enforces (no `now()`, stable
  iteration). Sprint 1 lifts these into `nils-markdown`; do not relax
  them per consumer.
- **Half-migrated artifact**. Never ship a PR where one section of a
  long artifact is templated and another is still inline. Migrate
  per artifact, not per paragraph.
- **Helper drift between `nils-common` and `nils-markdown`**.
  Decision 6 makes `nils-common::markdown` the lowest layer.
  `nils-markdown` re-exports helpers but does not duplicate logic.
  Any new helper lands in `nils-common` first.

## Rollback plan

- Sprint 1: revert the Sprint 1 PR. `agent-runtime-cli` returns to
  in-crate helpers and its golden tests stay passing.
- Sprint 2: per-PR rollback. Each Tier A migration is a single PR;
  revert restores the inline Rust path and removes the new template
  and golden fixture from the same commit.
- Sprint 3: revert the binary-target PRs. The library surface stays
  intact and Tier A migrations continue to work.
- The `nils-markdown` crate itself is publishable from Sprint 1; if
  it ships to crates.io before a Sprint 2 PR reverts, leave the
  published version in place and continue with the next minor bump.
  Do not yank published crates.
