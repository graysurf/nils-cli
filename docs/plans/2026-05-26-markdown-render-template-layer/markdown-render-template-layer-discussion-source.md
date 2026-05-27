# Markdown Render Template Layer Source

- Status: open, ready for implementation planning
- Date: 2026-05-26
- Source: in-session discussion about unifying Markdown generation across
  nils-cli crates after auditing the current inline-versus-Tera split
- Source type: discussion-to-implementation-doc

## Execution

- Recommended plan: docs/plans/markdown-render-template-layer/markdown-render-template-layer-plan.md
- Recommended execution state: docs/plans/markdown-render-template-layer/markdown-render-template-layer-execution-state.md

## Purpose

Today the workspace generates human-facing Markdown two different ways. The
`agent-runtime-cli` skill renderer uses Tera with `.md.tera` templates, custom
helpers (`skill_ref`, `cli_ref`, `state_out`, `script`), determinism guards,
and golden tests. Every other Markdown-producing crate builds output inline
through `format!` and `String::push_str`, optionally calling the small set of
helpers in `nils-common::markdown` (`heading`, `code_block`,
`canonicalize_table_cell`, `format_json_pretty_sorted`,
`validate_markdown_payload`).

The inline approach is acceptable for short commit messages and short replies
but is becoming a maintenance liability for long structured artifacts such as
plan-issue lifecycle records, dispatch dashboards, repo retrospectives,
heuristic-inbox records, review-specialists reports, and API testing reports.
The same content shape is open-coded in multiple files; pipe escaping is
applied by convention rather than by construction; layout changes require
editing Rust strings; and there is no shared golden-test pattern outside
`agent-runtime-cli`.

This source document captures the decision to introduce a single
workspace-internal Markdown template layer that all human-facing Markdown
artifacts flow through, the design principle that governs how that layer is
used, and the crate-level inventory of what to migrate.

## Design Principle

**Templates carry layout only. Data is prepared in Rust as flat,
template-ready structs.**

The template file decides headings, ordering, bullet shape, table columns,
and surrounding prose. The Rust side decides what to compute, which fields
are present, how empty values are represented, and which strings need
Markdown-safe escaping before they reach the template.

### Why

1. **Single responsibility per layer.** Business logic (what to compute,
   which records to include, how to sort, what counts as empty) is one kind
   of change. Visual layout (headings, table columns, sentence ordering) is
   a different kind of change. Mixing them in `format!` chains makes both
   harder to evolve and makes diffs hard to review.
2. **Reviewable diffs.** A layout-only change becomes a template-only diff.
   A data-only change becomes a Rust-only diff. Pull request reviewers can
   tell which is happening without reading both sides.
3. **Determinism is solvable once.** `agent-runtime-cli` already proves
   the engine setup needed for byte-stable output (stable iteration order,
   no wall-clock time, no hash-randomized maps, golden fixtures). The
   determinism contract belongs to the engine and the struct preparation
   path, not to each template author.
4. **Escaping is solvable once.** Table cells, pipe characters, embedded
   newlines, and code fences each have one correct treatment. When the
   Rust side prepares a flat struct, the template can call a single
   filter (for example `| md_cell`) and never get the escaping wrong.
   Inline `format!` callers routinely forget this today.
5. **Templates stay readable.** A template that branches on nested data
   with `{% for %}`, `{% if %}`, and inline filters becomes harder to
   maintain than the Rust it replaced. Flat structs keep templates almost
   declarative. This is the rule that distinguishes a useful template
   layer from a worse-than-inline one.
6. **Golden testing scales.** With a flat struct as input and a `.md.tera`
   file as the only layout, the golden test pattern from
   `agent-runtime-cli` (`render/golden.rs`) can be lifted to every
   migrated artifact with no per-crate redesign.

### Operational rule

For any artifact migrated to the template layer:

- The Rust side exposes one `pub struct <Artifact>View` whose fields are
  exactly the values the template needs, already escaped or normalized
  where required.
- No nested computation, no `Option<Option<_>>`, no late lookups inside the
  template.
- All Markdown-table-bound strings pass through the shared `md_cell` filter
  on the template side; the Rust side does not pre-escape pipes.
- All multi-line code blocks are passed in as already-trimmed `String`
  values and rendered by a `code_block` filter or partial.
- Empty optional sections render via `{% if field %} ... {% endif %}`; the
  Rust side does not push or skip whole sections by string concatenation.

## Confirmed Facts

- `crates/agent-runtime-cli` already depends on `tera` (workspace dep, root
  `Cargo.toml`). No other crate depends on `tera`, `askama`, `minijinja`,
  `handlebars`, or `tinytemplate`.
- `crates/agent-runtime-cli/src/render/` contains the working Tera engine
  setup (`writer.rs` 1347 lines, `manifest.rs` 729 lines, `golden.rs` 282
  lines, `support_matrix.rs` 378 lines, `time.rs` 229 lines, `cache.rs`
  158 lines, `mod.rs` 7 lines) and `helpers/` contains `cli_ref.rs`,
  `script.rs`, `skill_ref.rs`, `state_out.rs`, and `mod.rs`.
- The only `.md.tera` source in the workspace today is the determinism
  test fixture
  `crates/agent-runtime-cli/tests/fixtures/render-determinism/core/skills/sample/SKILL.md.tera`.
- `crates/nils-common/src/markdown.rs` (217 lines) exposes `heading`,
  `code_block`, `canonicalize_table_cell`,
  `validate_markdown_payload`, and `format_json_pretty_sorted`.
- `crates/api-testing-core/src/markdown.rs` is a thin wrapper over
  `nils_common::markdown`.
- `crates/plan-issue-cli` reads `nils_common::markdown` from `render.rs`,
  `execute.rs`, `github.rs`, `issue_body.rs`, `task_spec.rs`. Counts of
  `format!`/`push_str`/`writeln!` sites in the same files:
  `lifecycle_record.rs` 111, `execute.rs` 151, `render.rs` 24,
  `task_spec.rs` 28, `issue_body.rs` 21, `lifecycle_vnext/templates.rs` 3.
- `crates/agent-workflow-primitives` produces Markdown in
  `repo_retro.rs` (1958 lines), `heuristic_inbox.rs` (2611 lines),
  `review_specialists.rs` (59 inline Markdown sites).
- `crates/agent-docs/src/commands/scaffold_agents.rs` and
  `scaffold_baseline.rs` ship static templates as
  `include_str!("../templates/agents_default.md")`,
  `development_default.md`, and `cli_tools_default.md`.
- `crates/plan-tooling/src/scaffold.rs` ships
  `include_str!("../plan-template.md")` and replaces a single title line.
- `crates/forge-cli/src/ops/pr_create.rs` does not synthesize PR bodies;
  it accepts them as opaque input and persists them via the backend.
- `crates/semantic-commit/src/commit.rs` composes commit messages, which
  are short and follow Semantic Commit shape rather than long-form
  Markdown.
- `crates/git-cli/src/commit.rs` builds a long diff/context Markdown
  artifact for review consumption (32 `format!`/`push_str` sites).
- `crates/api-testing-core/src/rest/report.rs`,
  `src/report.rs`, `src/suite/summary.rs`,
  `src/cli_history.rs` build per-protocol Markdown reports.
- Smaller Markdown emitters: `crates/git-summary/src/summary.rs`,
  `crates/codex-cli/src/prompt_segment/render.rs`,
  `crates/gemini-cli/src/prompt_segment/render.rs`,
  `crates/macos-agent/src/preflight.rs`,
  `crates/api-websocket/src/commands/history.rs`,
  `crates/api-gql/src/commands/call.rs`.

## Decisions

1. Adopt one shared workspace-internal Markdown template engine layer
   built on top of the existing Tera setup proved in
   `agent-runtime-cli`.
2. Apply the design principle above to every artifact migrated onto the
   template layer. Templates carry layout only; the Rust side ships flat
   view structs.
3. Reuse `tera` as the engine. Do not introduce `askama`, `minijinja`,
   `handlebars`, or `tinytemplate`.
4. Extract the engine builder, determinism configuration, and the
   existing helpers (`skill_ref`, `cli_ref`, `state_out`, `script`) out
   of `agent-runtime-cli` into a workspace-shared location, then have
   `agent-runtime-cli` re-export so its golden tests continue to pass
   unchanged.
5. Add a workspace-shared `md_cell` Tera filter that wraps
   `nils_common::markdown::canonicalize_table_cell`. Apply it from every
   migrated template; do not pre-escape pipes in Rust.
6. Keep `nils_common::markdown` as the helper home (`heading`,
   `code_block`, `canonicalize_table_cell`,
   `validate_markdown_payload`, `format_json_pretty_sorted`). The
   template engine layer depends on it; it does not depend on the
   template engine.
7. Adopt the `.md.tera` extension for all migrated templates. Store
   templates inside the owning crate (for example
   `crates/plan-issue-cli/templates/`), not in a central template
   directory, so each crate owns the layout it ships.
8. Lift the golden-test pattern from
   `crates/agent-runtime-cli/src/render/golden.rs` into the shared layer
   so every migrated artifact has the same byte-stable test harness.
9. Do not migrate short or unstructured Markdown artifacts. Commit
   messages, prompt-segment renderers, two-line summaries, and PR titles
   stay inline.
10. Do not introduce a runtime dependency on the template layer from
    `forge-cli`. `forge-cli` continues to receive PR/MR bodies as opaque
    strings; bodies are constructed upstream by the caller, which may or
    may not use the template layer.
11. Ship the template layer as one crate with both library and binary
    surfaces in the same delivery.

    - **Library surface (Sprint 1 + Sprint 2).** `nils-markdown` owns
      the Tera engine builder, the four existing helpers, the
      `md_cell` filter, and the golden-test harness.
      `agent-runtime-cli` and all Tier A consumers depend on it
      through normal Cargo dependency.
    - **Binary surface (Sprint 3).** The same `nils-markdown` crate
      adds a `md-render` Cargo binary target. Interface:
      `md-render --template <path.md.tera> --data <data.json>
      [--strict-determinism]`. The binary loads the template, parses
      the data file as `serde_json::Value`, calls
      `Engine::render_value`, and writes stdout. No new view-struct
      types are exposed; the JSON input is treated as an opaque tree.

    The original two-phase rationale (avoid pinning a public JSON
    contract before view structs stabilize) is preserved by Decision
    13's `include_str!` rule and by treating the binary's JSON input
    as opaque — the binary does not advertise per-template schemas.
    Shipping the binary in the same plan removes the need for a
    second tracking issue and unblocks immediate use from
    `agent-runtime-kit` skills and non-Rust agents.

12. Do not place the template engine inside `nils-common`. Keeping the
    template layer in its own crate preserves Decision 6 (the helper
    crate does not depend on the engine) and avoids forcing every
    `nils-common` consumer to compile Tera.

13. Bundle every consumer's `.md.tera` template into its owning crate
    with `include_str!` at compile time. Do not read templates from
    disk at runtime. The bundled-binary approach is what
    `crates/agent-docs/src/commands/scaffold_*.rs` and
    `crates/plan-tooling/src/scaffold.rs` already do for static
    Markdown templates; it keeps installed binaries offline-safe and
    keeps crate publishing self-contained. `nils-test-support` test
    fixtures may continue to read `.md.tera` from disk because they
    are test-only.

14. Make `nils-markdown` publishable (no `publish = false`). At least
    one of its first-wave consumers (`nils-agent-workflow-primitives`)
    is publishable, so the dependency must be on crates.io. The new
    crate is added to `release/crates-io-publish-order.txt`
    immediately after `nils-term` and `nils-common` and before every
    consumer that depends on it. `scripts/publish-crates.sh --dry-run`
    over the touched dependency chain must succeed before the first
    publish.

15. Migrate the `crates/plan-issue-cli/src/lifecycle_vnext/templates.rs`
    file to real templates as part of the Tier A migration. After
    migration the Rust module retains the name `templates.rs` only if
    it still contains view-struct definitions and template-loading
    glue; otherwise the rename is folded into the same change set.

## Scope

In scope:

- A workspace-shared Markdown template engine layer built on Tera.
- Extraction of the existing `agent-runtime-cli` Tera helpers and
  determinism setup into the shared layer.
- A shared `md_cell` Tera filter backed by
  `nils_common::markdown::canonicalize_table_cell`.
- A shared golden-test harness modeled on
  `agent-runtime-cli/src/render/golden.rs`.
- Migration of Tier A artifacts (see Inventory) to `.md.tera` templates
  plus flat view structs.
- Optional consolidation of Tier B artifacts (static `include_str!`
  templates) onto the same engine when migration is cheap.
- Documentation of the design principle and golden-test pattern under
  `docs/runbooks/` or the relevant project policy area.

Out of scope:

- Changing the public CLI contract of any migrated crate (command names,
  exit codes, JSON schemas, help text).
- Migrating commit-message composition (`semantic-commit`,
  `git-cli/src/commit.rs` message body composition path).
- Migrating prompt-segment renderers in `codex-cli` and `gemini-cli`.
- Migrating small Markdown emitters in `git-summary`,
  `api-websocket/src/commands/history.rs`,
  `api-gql/src/commands/call.rs`, `macos-agent/src/preflight.rs`.
- Adding non-Tera engines or HTML rendering.
- Changing `forge-cli` to construct PR or MR bodies itself.
- Replacing existing review evidence JSON schemas with template output.

## Crate Inventory

### Tier A: migrate first

These crates produce long, structured Markdown whose layout already
changes independently of the underlying data, and which are read by
humans in PRs, issues, dashboards, or evidence records.

| Crate / file | Artifact | Reason to migrate |
| --- | --- | --- |
| `crates/plan-issue-cli/src/render.rs` | Plan-issue dashboards and lifecycle render output | 549 lines, drives provider-visible dashboards; layout churn is frequent |
| `crates/plan-issue-cli/src/lifecycle_record.rs` | Lifecycle comment bodies | 111 inline Markdown sites; same shape repeated across phases |
| `crates/plan-issue-cli/src/execute.rs` | Execution-state and follow-up comments | 151 inline Markdown sites |
| `crates/plan-issue-cli/src/issue_body.rs` | Plan-issue body | 21 inline Markdown sites; stable section ordering |
| `crates/plan-issue-cli/src/task_spec.rs` | Task-spec rendering inside lifecycle | 28 inline Markdown sites |
| `crates/plan-issue-cli/src/lifecycle_vnext/templates.rs` | vNext lifecycle bodies | Already named `templates.rs`; converting to real templates removes the name mismatch |
| `crates/agent-workflow-primitives/src/repo_retro.rs` | Repository retrospective report | 1958 lines, stable section layout, golden-test target |
| `crates/agent-workflow-primitives/src/heuristic_inbox.rs` | Heuristic-system entries | 2611 lines, strict section schema (Status, Signal, Evidence, …) |
| `crates/agent-workflow-primitives/src/review_specialists.rs` | Review-specialists merged report | 59 inline Markdown sites; specialist sections concatenated |

### Tier A migration order

The plan should execute Tier A in this order to keep blast radius small
and to validate the shared layer against a representative artifact
before the long ones.

1. **`crates/plan-issue-cli/src/issue_body.rs`** — smallest of the Tier A
   set (21 sites, stable section ordering). First migration; proves the
   end-to-end pipeline (view struct → `.md.tera` → `Engine` → golden
   test) on a low-risk artifact.
2. **`crates/plan-issue-cli/src/task_spec.rs`** — adds table-cell
   coverage and exercises the new `md_cell` filter against pipes and
   embedded newlines.
3. **`crates/plan-issue-cli/src/lifecycle_vnext/templates.rs`** — the
   smallest lifecycle surface; also closes the name mismatch noted in
   Decision 15.
4. **`crates/plan-issue-cli/src/render.rs`** — dashboards. Provider-
   visible byte stability matters here; land golden coverage first.
5. **`crates/plan-issue-cli/src/lifecycle_record.rs`** — the long
   lifecycle comment body (111 sites).
6. **`crates/plan-issue-cli/src/execute.rs`** — the longest plan-issue
   surface (151 sites); migrated last in this crate so earlier patterns
   are already proven.
7. **`crates/agent-workflow-primitives/src/review_specialists.rs`** —
   first cross-crate consumer; validates that the layer is reusable
   outside `plan-issue-cli`.
8. **`crates/agent-workflow-primitives/src/repo_retro.rs`** — long
   retrospective report; rich struct-to-template mapping.
9. **`crates/agent-workflow-primitives/src/heuristic_inbox.rs`** —
   largest and most schema-strict artifact; migrated last so the
   layer's helpers and golden harness are battle-tested.

### Tier B: migrate opportunistically

These crates already use static templates via `include_str!` or build
structured reports through `nils-common::markdown` helpers. Migration is
straightforward but lower urgency.

| Crate / file | Today | Why opportunistic |
| --- | --- | --- |
| `crates/agent-docs/src/commands/scaffold_agents.rs` and `scaffold_baseline.rs` (`templates/agents_default.md`, `development_default.md`, `cli_tools_default.md`) | Static `include_str!` with no interpolation | Becomes a real template once any field substitution is added; until then the static file is acceptable |
| `crates/plan-tooling/src/scaffold.rs` (`plan-template.md`) | Static `include_str!` with single-line title replace | Same story as `agent-docs` scaffolds |
| `crates/api-testing-core/src/report.rs`, `src/suite/summary.rs`, `src/rest/report.rs`, `src/cli_history.rs` | Calls `nils-common::markdown` helpers, builds via `push_str` | Reports have stable shape; migration low risk; defer until Tier A patterns are proved |
| `crates/git-cli/src/commit.rs` (diff/context Markdown for review consumption) | 32 inline Markdown sites | Output shape stable; migrate if a downstream consumer needs golden testing |

### Tier C: reference architecture, do not migrate

| Crate | Reason |
| --- | --- |
| `crates/agent-runtime-cli` | Already on Tera; this crate is the source of the engine setup, helpers, and golden harness that the shared layer will extract |

### Tier D: do not migrate

| Crate / file | Reason |
| --- | --- |
| `crates/semantic-commit/src/commit.rs` | Short commit messages, Semantic Commit shape, no long-form Markdown |
| `crates/git-cli/src/commit.rs` (commit message composition path) | Short message body |
| `crates/forge-cli/src/ops/pr_create.rs` and siblings | Accepts PR/MR bodies as opaque input; does not construct them |
| `crates/codex-cli/src/prompt_segment/render.rs`, `crates/gemini-cli/src/prompt_segment/render.rs` | Small prompt segments, 4 sites each |
| `crates/git-summary/src/summary.rs` | 2 sites |
| `crates/api-websocket/src/commands/history.rs`, `crates/api-gql/src/commands/call.rs` | Small command-local output |
| `crates/macos-agent/src/preflight.rs` | Small preflight messages |

## Requirements

- A workspace-shared library crate exists that exposes the Tera engine
  builder, the existing four helpers, the new `md_cell` filter, and the
  golden-test harness; current `agent-runtime-cli` behavior is preserved.
- Every Tier A artifact compiles, renders identically to its pre-migration
  byte output for at least one representative input, and gains a golden
  test under the shared harness.
- The migration preserves every public CLI contract of every touched
  crate: command names, exit codes, JSON schemas, help shape, and
  completion assets.
- `nils-common::markdown` remains the lowest-level helper layer. It does
  not gain a dependency on the template engine.
- New `.md.tera` files live inside the owning crate directory tree.
- All migrated templates use the `md_cell` filter for Markdown-table
  cells; no migrated template pre-escapes pipes inline.
- The design principle and the golden-test pattern are documented under
  `docs/runbooks/` so future contributors can extend the layer without
  re-deriving conventions.

## Acceptance Criteria

- `cargo test -p agent-runtime-cli` passes unchanged after the engine
  and helper extraction.
- `cargo test --workspace` passes after each Tier A migration.
- For each migrated artifact, a byte-equality golden test exists under
  the shared harness and is committed alongside the migration.
- `grep -rn 'canonicalize_table_cell' crates/*/templates` returns no
  hits; the filter is applied via Tera (`md_cell`), not via Rust calls
  inside template-driven code paths.
- `grep -rn 'tera' crates/*/Cargo.toml` shows that only the shared
  template layer crate and `agent-runtime-cli` depend on Tera directly,
  or that the dependency is exposed through a single workspace re-export.
- The required docs-only and full-workspace gates remain green for every
  touched surface.

## Rust Implementation Stack

All dependencies are already present in the workspace root `Cargo.toml`
or as internal path-dependent workspace members. No new external crates
are introduced.

### New crate

- `crates/nils-markdown` — new workspace member; owns the engine
  builder, helpers, `md_cell` filter, golden harness, and (in Phase 2)
  the `md-render` binary target. Library by default; binary target
  gated behind a `bin-cli` Cargo feature so library consumers do not
  pull `clap` and the binary path until Phase 2 ships.

### Runtime dependencies (`nils-markdown`)

| Crate | Why |
| --- | --- |
| `tera = "1"` (workspace) | Engine. Same version already proven in `agent-runtime-cli`. |
| `serde = "1"` (workspace, `derive`) | View-struct trait used by every consumer to define render input. |
| `serde_json = "1"` (workspace) | Carrier type for Phase 2 CLI input and for `Engine::render_value`. |
| `indexmap = workspace` | Stable insertion-order maps inside view structs (matches the determinism contract already used by `crates/agent-runtime-cli/src/render/manifest.rs`). |
| `thiserror = "2"` (workspace) | Typed `RenderError` returned from the library API. |
| `nils-common` (path dep) | Reuse `canonicalize_table_cell`, `heading`, `code_block`, `format_json_pretty_sorted`, `validate_markdown_payload`. The `md_cell` filter wraps `canonicalize_table_cell`. |

### Phase 2 CLI dependencies (feature-gated `bin-cli`)

| Crate | Why |
| --- | --- |
| `clap = "4"` (workspace, `derive`) | CLI argument parsing for `md-render`. Same major version as every other CLI in the workspace. |
| `anyhow = "1"` (workspace) | Top-level error type at the CLI boundary, with `?` over `RenderError` and IO errors. |

### Dev / test dependencies

| Crate | Why |
| --- | --- |
| `pretty_assertions = "1"` (workspace) | Byte-diff output for golden tests, matching the style already used in `crates/nils-common/src/markdown.rs` and `crates/agent-runtime-cli/src/render/golden.rs`. |
| `tempfile = "3"` | Per-test scratch dirs for golden fixtures, matching the pattern used by `agent-runtime-cli`, `agent-docs`, and `agent-workflow-primitives` today. |
| `nils-test-support` (path dep) | Reuse the workspace's existing fixture helpers rather than vendoring new ones. |

### Explicitly rejected

- `askama`, `minijinja`, `handlebars`, `tinytemplate`, `liquid`,
  `ramhorns` — Decision 3 keeps `tera` as the only engine. Adding any
  of these would split the determinism configuration surface and
  duplicate the helper layer.
- `tracing` inside `nils-markdown` — render errors surface through
  `Result<_, RenderError>`. The library is silent; logging belongs to
  the calling binary.
- `serde_yaml_ng` inside `nils-markdown` — view structs and Phase 2
  CLI input are JSON only. YAML stays an `agent-runtime-cli` concern
  (manifest parsing), not a template-engine concern.
- `insta` — not currently a workspace dependency. Reuse
  `pretty_assertions` plus `nils-test-support` to stay consistent
  with the existing golden pattern.

### Release and publish wiring

- `crates/nils-markdown` is added to the root `Cargo.toml` workspace
  `members` list alongside the other workspace crates.
- `release/crates-io-publish-order.txt` lists `nils-markdown` after
  `nils-common` and `nils-term` and before any consumer that depends
  on it (`agent-runtime-cli`, `plan-issue-cli`,
  `agent-workflow-primitives`).
- `scripts/publish-crates.sh --dry-run --crates "nils-common nils-term nils-markdown <consumer>"`
  succeeds for each first-wave consumer before the first real publish.
- Phase 2's `md-render` binary is registered the same way every other
  CLI in the workspace is: `bash scripts/workspace-bins.sh` reports
  `md-render`; `completions/bash/md-render` and
  `completions/zsh/_md-render` exist; the binary follows
  `docs/runbooks/cli-completion-development-standard.md`. Phase 2 also
  follows `docs/runbooks/new-cli-crate-development-standard.md` for
  the human-readable and JSON output contracts (the Phase 2 binary's
  JSON contract is its rendered output passthrough, not a new schema).

### Cargo dependency direction (must hold)

```text
nils-common  <----  nils-markdown  <----  agent-runtime-cli
                                    <----  plan-issue-cli
                                    <----  agent-workflow-primitives
                                    <----  (Tier B consumers, later)
```

No reverse edges. `nils-common` must not gain a `tera` dependency.
`nils-markdown` must not gain a dependency on any consuming CLI crate.

## Implementation Boundaries

- One shared crate owns the Tera engine setup, helpers, and golden
  harness. `agent-runtime-cli` consumes it through normal Cargo
  dependency, not through copy-paste.
- Helpers move with their tests. Behavior is preserved byte for byte.
- The shared crate does not embed any project-specific templates. Each
  consuming crate owns and ships its own `.md.tera` files.
- Templates do not call provider APIs, do not read files, and do not
  reach out to the network. Anything that would require such a call is
  resolved into the view struct before rendering.
- View structs are plain `serde::Serialize` Rust types. No `Box<dyn>`
  trait objects, no late `Any` lookups inside templates.
- Empty optional sections are expressed in the template, not by
  conditional Rust string concatenation.

## Risks And Guardrails

- The migration is byte-sensitive for provider-visible artifacts
  (plan-issue dashboards, lifecycle comments, heuristic-inbox entries).
  Land golden tests in the same change set as each migration.
- Tera template debug output can be opaque. Keep view structs flat and
  named so error messages identify the missing field by name.
- The shared engine layer must keep determinism guarantees the
  `agent-runtime-cli` golden tests already rely on (stable iteration,
  no `SystemTime::now`, no hash-randomized maps). Lift these guarantees
  into the shared layer, do not relax them per consumer.
- Do not stage a half migration where one section of a long artifact is
  templated and another is still inline. Migrate per artifact, not per
  paragraph.
- Do not let the shared crate accumulate consumer-specific helpers.
  Consumer-specific layout logic stays in the consumer's view struct.

## Read First References

- `crates/agent-runtime-cli/src/render/`
- `crates/agent-runtime-cli/src/render/helpers/`
- `crates/agent-runtime-cli/src/render/golden.rs`
- `crates/agent-runtime-cli/tests/fixtures/render-determinism/core/skills/sample/SKILL.md.tera`
- `crates/nils-common/src/markdown.rs`
- `crates/plan-issue-cli/src/render.rs`
- `crates/plan-issue-cli/src/lifecycle_record.rs`
- `crates/agent-workflow-primitives/src/repo_retro.rs`
- `crates/agent-workflow-primitives/src/heuristic_inbox.rs`
- `docs/specs/crate-docs-placement-policy.md`
- `docs/runbooks/new-cli-crate-development-standard.md`

## Retention Intent

This document is a plan-source artifact under `docs/plans/`. After the
plan it feeds is delivered, retain the design-principle and golden-test
guidance by promoting them into `docs/runbooks/` (or the relevant
project policy location). The inventory, scope, and acceptance criteria
in this source document can be archived with the plan once execution is
complete.
