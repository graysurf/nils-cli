# nils-markdown

Shared Markdown template layer for the nils-cli workspace.

`nils-markdown` is the workspace-wide Tera template engine layer used to
emit long-form human-facing Markdown artifacts. The library exposes:

- A deterministic Tera `Engine` builder.
- An `md_cell` Tera filter wrapping
  `nils_common::markdown::canonicalize_table_cell` for pipe-safe table
  cells.
- A re-export bridge to `nils_common::markdown::*` helpers (`heading`,
  `code_block`, `validate_markdown_payload`,
  `format_json_pretty_sorted`) so consumer crates have one import
  surface.
- A generic `Engine::register_helper(name, F)` extension point so a
  consumer crate can attach its own domain-specific Tera helpers
  without `nils-markdown` knowing the consumer's domain.
- A `golden::assert_render(fixture, engine, template, view)` test
  harness that asserts byte equality against a captured fixture using
  `pretty_assertions`.

## Design principle

Templates carry layout only. Data must be prepared in Rust as a flat
`serde::Serialize` view struct and passed to the engine. Templates do
not reach into the consumer's domain model; consumers do not embed
Markdown in `format!` chains. Helpers compose, never duplicate, the
`nils_common::markdown` rules.

## Binary target

The crate also ships an opt-in `md-render` binary (Cargo feature
`bin-cli`). The binary reads a template path and a JSON view file, runs
the same engine, and writes the rendered Markdown to `stdout`. It
supports both the text envelope (default; rendered Markdown body
written to stdout) and the JSON envelope (`--format json`; the
`cli.md-render.render.v1` envelope wraps the body alongside the
template name).

Usage:

```text
md-render --template <PATH> --data <PATH> [--format text|json]
md-render completion <bash|zsh>
```

The `--template` flag accepts any `.md.tera` or `.tera` file and the
template is registered under its file stem. The `--data` flag accepts
a JSON object that is passed to `Engine::render_value` as the
template's view.

## Rollout

This crate is introduced by
`docs/plans/markdown-render-template-layer/`. Tier A migration order
and consumer wiring are tracked in the plan and in tracking issue
`#541`.
