# Markdown Template Development Standard

This runbook is the durable policy carried out of the
`markdown-render-template-layer` rollout. It documents how to author
new `.md.tera` templates inside the workspace, when to use them, and
what guarantees they must preserve.

## When to author a template

Author a Tera template when a Rust function emits long-form Markdown
that:

- Goes to a provider surface (PR comments, issue bodies, dashboards,
  reports) or to a long-lived disk artifact a human will read.
- Has a fixed section schema (headings, tables, optional lists) that
  is stable across many invocations.
- Composes structured data into Markdown by stitching `format!` /
  `push_str` chains in Rust.

Keep small one-line `format!` calls (file paths, error messages,
key=value diagnostics, JSON envelope construction) in Rust. The
template layer is for human-readable Markdown bodies, not for
arbitrary string formatting.

## Design principle

> Templates carry layout only. Data is prepared as flat
> `serde::Serialize` view structs in Rust and handed to the engine.

Concretely:

- The template owns headings, blank lines, table structure, and the
  ordering of optional sections.
- The Rust caller owns conditional logic (Option fields, empty
  collections, branch selection) and produces the view struct with
  the exact fields the template consumes.
- Where conditional emission has tricky whitespace boundaries, the
  caller pre-renders the conditional fragment into a single string
  field (e.g. `tracker_block`, `findings_block`, `residual_block`)
  and the template just interpolates it.
- The template never reaches into the consumer's domain model
  (`StateData`, `RepoRetroReport`, `MergeResult`, etc.). Consumers
  build a flat view struct first, then call `Engine::render`.

## Required helpers

- `nils_markdown::Engine::builder()` constructs a deterministic Tera
  engine (autoescape off, no `now()` call). Always start engines
  from this builder; never construct `tera::Tera` directly.
- `nils_markdown::Engine::register_template(name, body)` registers a
  template under a name. Bundle template bodies with
  `include_str!("../templates/.../foo.md.tera")` per Decision 13 of
  the discussion source so the asset travels with the binary.
- `nils_markdown::Engine::render<T: Serialize>(name, &view)` and
  `Engine::render_value(name, &json_value)` are the two render
  entry points. The `md-render` binary uses `render_value` because
  it consumes JSON files; in-crate consumers use `render` because
  they hold typed view structs.
- The `md_cell` Tera filter wraps
  `nils_common::markdown::canonicalize_table_cell` for pipe-safe
  table cells. Always use `{{ value | md_cell }}` inside Markdown
  table cells, never raw `{{ value }}`.

## Golden-test pattern

Each template migration ships byte-equality golden fixtures. The
recipe used across PRs #542–#552:

1. Capture the pre-migration `format!` output by running the
   migration test against the pre-change code. Either bless the
   fixture from a `git stash` of the migration, or manually trace
   the original `format!` layout to construct the expected
   bytes. Save into `tests/golden/<emitter>/<scenario>.md`.
2. Run the same test against the post-migration code. The fixture
   bytes must match exactly. Where possible, capture both
   "empty / no data" and "populated" scenarios so the conditional
   sections are exercised.
3. Plumb a `BLESS_<CRATE>_GOLDEN=1` env var through the test that
   overwrites the fixture from the live engine output. This makes
   re-blessing trivial when an intentional output change ships.
4. Use `pretty_assertions::assert_eq!` for the assertion so diffs
   are readable on mismatch.

Use the harness behind the `test-support` Cargo feature when the
consumer's tests are integration tests rather than unit tests:
`nils_markdown::golden::assert_render(fixture, &mut engine, name,
&view)` does the read + assert + bless cycle.

## Workspace feature unification

Some consumer crates (`forge-cli`, `git-cli`,
`plan-issue` after PR #548) enable
`serde_json/preserve_order`. Workspace feature unification turns
that on for ALL crates in workspace test runs (`cargo nextest run
--workspace`). Per-crate tests (`cargo test -p <crate>`) do NOT
unify features by default. If a template's view contains
`serde_json::Value` payloads, hex-encoded JSON, or other
key-order-sensitive output, pin `preserve_order` in the consuming
crate's `serde_json` dependency so both invocations produce
byte-identical bytes (see PR #548's fix).

## Whitespace pitfalls

Tera's whitespace control (`{%-` strips preceding, `-%}` strips
trailing) makes conditional sections tricky:

- For a section that may be empty, render the inner body
  (`{{ block }}`) and place the surrounding heading + blank line
  outside the conditional. Use the trailing-newline-in-block
  pattern: the block ends with `\n` when non-empty (so the next
  heading is offset by one blank line) and is `""` when empty (so
  the template's static `\n\n` between heading and next heading
  becomes the natural blank line).
- For sections that always render content (an "empty bucket" line
  plus a populated table case), pre-render the block in Rust and
  pass a single `*_block` field.
- For an entire conditional section (with its own heading), wrap
  the whole `## Heading + body` in `{%- if show_block %} ...
  {%- endif %}`. Strip whitespace on the `{%-` side so the
  preceding section's trailing newline lands cleanly.

If a fixture diffs by a stray blank line, the fix is almost always
moving the `\n` into the Rust-side pre-rendered block, not chasing
`{%-` toggles inside the template.

## Reference plan + tracking issue

- Plan: archived at `agent-plan-archive:plans/github.com/sympoies/nils-cli/2026-05-26-markdown-render-template-layer/`
- Tracking issue: `sympoies/nils-cli#541`
- Implementation PRs: `#542` (Sprint 1), `#543`–`#552` (Sprint 2
  Tier-A), Sprint 3 (this runbook + the `md-render` binary).
