# image-processing LLM SVG workflow

## Purpose

Use a provider-agnostic pipeline to turn user intent into policy-compliant SVG, then render with
`image-processing convert --in`. There is no built-in `generate` subcommand; the LLM step happens
out-of-process so any provider (or hand-authored SVG) can feed the same validate-and-render flow.

## Contract

- `convert --in <path>` is the canonical SVG-to-raster flow.
- `svg-validate` must gate LLM output before raster export.
- Validation policy is defined in the
  [LLM SVG output contract](../../assets/llm-svg-output-contract.md); the system prompt for the LLM
  step lives in [`assets/llm-svg-system-prompt.md`](../../assets/llm-svg-system-prompt.md).

## Prompt assets

The pipeline script reads two assets from `crates/image-processing/assets/`:

- [`llm-svg-system-prompt.md`](../../assets/llm-svg-system-prompt.md): rules that constrain the LLM
  to a single, deterministic `<svg>` document.
- [`llm-svg-output-contract.md`](../../assets/llm-svg-output-contract.md): the allowed tag set,
  forbidden tags/attributes, `href` policy, and the failure/repair contract enforced by
  `svg-validate`.

These are runtime assets; treat them as the source of truth for what the LLM must produce and what
`svg-validate` will accept.

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

Given `--out-svg out/plan-llm/traffic-car.svg`, the pipeline emits:

- `out/plan-llm/traffic-car.prompt.md`
- `out/plan-llm/traffic-car.raw.txt` (when `SVG_LLM_CMD` is used)
- `out/plan-llm/traffic-car.candidate.svg`
- `out/plan-llm/traffic-car.validate.json`
- `out/plan-llm/traffic-car.repair.prompt.md` (only on validation failure)

## Repair loop

If validation fails, re-run the LLM with the emitted repair prompt:

```bash
cat out/plan-llm/traffic-car.repair.prompt.md
```

Feed that prompt to your LLM provider, write the new candidate SVG, and run `svg-validate` again.

## Adopting the workflow

Any intent-to-icon flow that does not currently gate output through `svg-validate` should adopt the
three-step pipeline:

1. intent -> SVG (LLM or hand-authored),
2. `svg-validate` (block on diagnostics),
3. `convert --in` (render to png/webp/jpg).
