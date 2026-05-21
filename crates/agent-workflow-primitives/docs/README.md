# agent-workflow-primitives docs

This crate owns local-first agent workflow primitive binaries. Keep crate-specific specs, runbooks, and reports here; keep workspace-wide
completion, release, and new-crate rules in the root `docs/` tree.

## Binary overview

- `docs-impact`: Git change classification for docs impact review.
- `canary-check`: redacted local canary command records.
- `review-evidence`: review finding and validation evidence records.
- `review-specialists`: deterministic specialist finding validation, merge,
  render, bundle, and Git diff scope classification.
- `browser-session`: browser-session goal, step, and artifact records.
- `model-cross-check`: cross-model observation records without provider calls.
- `repo-retro`: repo-local implementation retrospectives from local Git,
  HEURISTIC_SYSTEM records, and explicit JSONL inputs.
- `skill-usage`: skill invocation, linked evidence, validation, outcome, and
  failure handling records.

## `repo-retro` examples

```bash
repo-retro report --repo . --days 7 --mode team --format json
repo-retro report --repo . --mode maintainer --format markdown
repo-retro report --repo . --from 2026-05-11 --to 2026-05-17 \
  --history-dir "$HOME/retro-history" --write
```

## Specs

- None yet. Add documents under `docs/specs/` and register them here.

## Runbooks

- Workspace runbook: `docs/runbooks/review-specialists-primitive.md`.

## Reports

- None yet. Add documents under `docs/reports/` and register them here.

## Links

- Back to crate README: [`../README.md`](../README.md)
