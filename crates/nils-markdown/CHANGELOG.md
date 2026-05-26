# Changelog

All notable changes to `nils-markdown` are documented here.

## 0.23.0

- Ship the `md-render` binary behind the `bin-cli` Cargo feature. The
  binary loads a `.md.tera` template and a JSON view file, registers
  the template under its file stem, and renders through
  `Engine::render_value`. Supports the text envelope (default) and
  the JSON envelope (`--format json`, `cli.md-render.render.v1`
  schema). Shell completions are exported via
  `md-render completion <bash|zsh>` and committed under
  `completions/{bash,zsh}/`.
- Workspace-wide Tier-A migration of nine Markdown emitters from
  `format!`/`push_str` chains to `.md.tera` templates + flat view
  structs (PRs #542–#552). Each migration adds byte-equality golden
  fixtures asserting the rendered bytes are unchanged from the
  pre-migration `format!` output.
- Initial library surface from Sprint 1 (PR #542): deterministic
  `Engine::builder()`, `md_cell` Tera filter wrapping
  `nils_common::markdown::canonicalize_table_cell`, generic
  `Engine::register_helper` extension point, and the
  `golden::assert_render` byte-equality harness behind the
  `test-support` Cargo feature.
