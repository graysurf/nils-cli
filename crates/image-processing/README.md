# image-processing

## Overview

`image-processing` is a Rust CLI that provides two focused operations on a single input/output pair:

- `svg-validate --in <svg> --out <svg>`: sanitize an SVG against the policy contract.
- `convert --in <path> --to png|webp|jpg --out <file>`: render an SVG or transcode a raster
  (`png|jpg|jpeg|webp`) into `png`, `webp`, or `jpg`.

The full sanitize policy (allowed/forbidden tags, attribute and `href` rules) lives in the
[LLM SVG output contract](assets/llm-svg-output-contract.md) and is enforced by `svg-validate`.

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

- `convert` uses the `image` crate for raster decode/encode and `usvg`/`resvg` for SVG input.
- `svg-validate` uses `roxmltree` for parsing and applies the policy contract in-process.
- No external runtime binary is required for either subcommand.
- See the workspace [`BINARY_DEPENDENCIES.md`](../../BINARY_DEPENDENCIES.md) for the canonical
  external-tool matrix (the `image-processing` runtime policy section confirms the no-binary
  contract).

## Docs

- [Docs index](docs/README.md)
