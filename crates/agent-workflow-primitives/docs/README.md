# agent-workflow-primitives docs

This crate owns local-first agent workflow primitives:

- `docs-impact`: Git change classification for docs impact review.
- `canary-check`: redacted local canary command records.
- `review-evidence`: review finding and validation evidence records.
- `browser-session`: browser-session goal, step, and artifact records.
- `model-cross-check`: cross-model observation records without provider calls.
- `skill-usage`: skill invocation, linked evidence, validation, outcome, and
  failure handling records.

Workspace-level release, completion, and new-crate rules remain in the root
`docs/` tree.
