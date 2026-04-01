# image-processing LLM SVG workflow

## Purpose

Use a provider-agnostic pipeline to turn user intent into policy-compliant SVG, then render with `image-processing convert --in`.

## Contract

- `generate` is removed.
- `convert --in <path>` is the canonical SVG-to-raster flow.
- `svg-validate` must gate LLM output before raster export.

## Quick start

```bash
mkdir -p out/plan-llm
```

```bash
SVG_LLM_CMD='cat crates/image-processing/tests/fixtures/llm-svg-valid.svg' \
  crates/image-processing/scripts/llm_svg_pipeline.sh \
  --intent "traffic car icon" \
  --out-svg out/plan-llm/traffic-car.svg \
  --dry-run
```

```bash
cargo run -p nils-image-processing -- svg-validate \
  --in out/plan-llm/traffic-car.svg \
  --out out/plan-llm/traffic-car.cleaned.svg
```

```bash
cargo run -p nils-image-processing -- convert \
  --in out/plan-llm/traffic-car.cleaned.svg \
  --to png \
  --out out/plan-llm/traffic-car.png \
  --json
```

## Pipeline artifacts

Given `--out-svg out/plan-llm/sun.svg`, the pipeline emits:

- `out/plan-llm/sun.prompt.md`
- `out/plan-llm/sun.raw.txt` (when `SVG_LLM_CMD` is used)
- `out/plan-llm/sun.candidate.svg`
- `out/plan-llm/sun.validate.json`
- `out/plan-llm/sun.repair.prompt.md` (only on validation failure)

## Repair loop

If validation fails, re-run LLM with repair prompt:

```bash
cat out/plan-llm/sun.repair.prompt.md
```

Feed that prompt to your LLM provider, write the new candidate SVG, and run `svg-validate` again.

## Migration note

Any previous `generate --preset ...` usage must migrate to:

1. intent -> SVG (LLM or hand-authored),
2. `svg-validate`,
3. `convert --in`.
