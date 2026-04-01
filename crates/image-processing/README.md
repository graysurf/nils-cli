# image-processing

## Overview

`image-processing` provides a focused conversion and validation flow:

- `svg-validate --in <svg> --out <svg>`
- `convert --in <path> --to png|webp|jpg --out <file>`

`generate` is removed.

## Usage

```text
Usage:
  image-processing <subcommand> [flags]

Subcommands:
  convert | svg-validate

Help:
  image-processing --help
```

## Commands

- `svg-validate`: Validate and sanitize one SVG input into one SVG output.
- `convert`: Convert `svg|png|jpg|jpeg|webp` input into `png`, `webp`, or `jpg` output.

## Common flags

- Input:
  - `svg-validate`: `--in <path>` (exactly one)
  - `convert`: `--in <path>` (exactly one)
- Output: `--out <file>`
- Output controls: `--overwrite`, `--dry-run`, `--json`, `--report`
- Render sizing for raster output: `--width`, `--height`

## `convert` contract

- Required: exactly one `--in`, `--to png|webp|jpg`, `--out <file>`.
- Supported inputs: `svg`, `png`, `jpg`, `jpeg`, `webp`.
- `--out` extension must match `--to` (`.jpeg` is accepted for `--to jpg`).
- Optional: `--width` and `--height` for raster sizing.
- `--to jpg` flattens alpha onto a white background.

## `svg-validate` contract

- Required: exactly one `--in <svg>` and `--out <svg>`.
- Forbidden: `--to`, `--width`, `--height`.
- Output is deterministic for identical input.

## Examples

```bash
mkdir -p out/plan-doc-examples
```

```bash
cargo run -p nils-image-processing -- svg-validate \
  --in crates/image-processing/tests/fixtures/llm-svg-valid.svg \
  --out out/plan-doc-examples/llm.cleaned.svg
```

```bash
cargo run -p nils-image-processing -- convert \
  --in out/plan-doc-examples/llm.cleaned.svg \
  --to png \
  --out out/plan-doc-examples/llm.png \
  --json
```

```bash
cargo run -p nils-image-processing -- convert \
  --in crates/image-processing/tests/fixtures/sample-icon.svg \
  --to jpg \
  --out out/plan-doc-examples/sample.jpg \
  --width 512 \
  --json
```

## Exit codes

- `0`: Success with no item errors.
- `1`: Runtime failure or one-or-more items failed.
- `2`: Usage/validation error.

## Dependencies

- `convert --in` and `svg-validate`: no external binary dependency (Rust backend).

## Docs

- [Docs index](docs/README.md)
- [LLM SVG workflow runbook](docs/runbooks/llm-svg-workflow.md)
