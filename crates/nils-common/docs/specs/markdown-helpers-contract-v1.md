# markdown Helpers Contract v1

## Purpose

Define the shared markdown helper behavior in `nils-common::markdown` used by multiple CLI crates.

## APIs

### `validate_markdown_payload(markdown: &str) -> Result<(), MarkdownPayloadError>`

- Rejects literal escaped-control artifacts (`\\n`, `\\r`, `\\t`) in markdown payloads.
- Accepts real control characters (`\n`, `\r`, `\t`).
- Returns structured violations via `MarkdownPayloadError::violations()` (a slice of
  `MarkdownPayloadViolation { sequence, count }`).

### `markdown_payload_violations(markdown: &str) -> Vec<MarkdownPayloadViolation>`

- Lower-level scan that returns the per-sequence violation counts without wrapping them in an error.
- Empty result implies the payload is acceptable for table/code-block rendering.

### `canonicalize_table_cell(value: &str) -> String`

- Produces markdown-table-safe cell text for render/compare round-trips.
- Normalizes:
  - `|` -> `/`
  - `\n` / `\r` runs -> single space
- Idempotent: applying it multiple times yields the same output.

### `format_json_pretty_sorted(value: &serde_json::Value) -> Result<String, serde_json::Error>`

- Sorts JSON object keys recursively.
- Emits stable pretty JSON text for markdown code blocks and report artifacts.

### `heading(level: u8, text: &str) -> String`

- Clamps heading level to 1..=6.
- Trims heading text and emits a trailing newline.

### `code_block(lang: &str, body: &str) -> String`

- Emits fenced code blocks using backticks.
- Preserves body text and guarantees a trailing newline before fence close.

## Intended Call Sites

- Markdown table renderers before writing rows.
- Drift/contract comparisons for table-parsed values.
- Notes fields that must survive markdown table serialization without false-positive diffs.
- API test/report markdown builders that need stable heading and code-fence layout.
- JSON-in-markdown renderers that require deterministic key ordering.

## Ownership

- `nils-common` owns the canonical production implementation.
- Consuming crates should not duplicate table-cell canonicalization logic.
- Consuming crates should prefer shared heading/code-block/json-format helpers over local duplicates.
- `nils-common` does not own GitHub adapters or `gh` command orchestration; keep those crate-local.

## Examples

- `A|B` -> `A/B`
- `"line1\r\nline2"` -> `"line1 line2"`
- `"x\n\n y"` -> `"x  y"` is not guaranteed; callers should not rely on whitespace collapsing beyond line-break run normalization.
