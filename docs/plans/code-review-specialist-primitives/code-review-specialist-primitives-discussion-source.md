# Code Review Specialist Primitives Implementation Handoff

- Status: open, ready for implementation planning
- Date: 2026-05-21
- Source: converged discussion about moving deterministic parts of
  `$AGENT_HOME/skills/workflows/code-review/code-review-specialists` into
  `nils-cli`.
- Intended next step: create and execute the paired implementation plan.

## Execution

- Recommended plan:
  docs/plans/code-review-specialist-primitives/code-review-specialist-primitives-plan.md
- Recommended execution state:
  docs/plans/code-review-specialist-primitives/code-review-specialist-primitives-execution-state.md

## Purpose

`code-review-specialists` currently owns both review judgment and deterministic
helper work. The judgment layer should stay in `agent-kit`, but the helper
surface should move into `nils-cli` so agents can validate, merge, format, and
bundle specialist findings through a released, tested primitive.

The target is a deterministic `review-specialists` CLI under
`nils-agent-workflow-primitives`, not a reviewer agent and not a provider
mutation tool.

## Confirmed Facts

- `code-review-specialists` is intentionally read-only. It produces scope JSON,
  specialist findings, and a final specialist review report; it does not post PR
  comments, merge PRs, close issues, or decide review outcomes.
- The normalized finding schema already exists in
  `$AGENT_HOME/skills/workflows/code-review/code-review-specialists/references/SPECIALIST_REVIEW_CONTRACT.md`.
- The report template already exists in
  `$AGENT_HOME/skills/workflows/code-review/code-review-specialists/references/SPECIALIST_REVIEW_REPORT_TEMPLATE.md`.
- `nils-cli` already ships workflow primitives in
  `crates/agent-workflow-primitives`, including `review-evidence`,
  `skill-usage`, `docs-impact`, and related binaries.
- Existing `nils-cli` direction is that deterministic evidence capture, gate
  checks, and formatting primitives belong in CLI commands, while skills own
  judgment, workflow framing, and repo-local policy.

## Decisions

1. Add a `review-specialists` binary to `crates/agent-workflow-primitives`.
2. Keep specialist prompts, selected-specialist judgment, red-team narrative,
   provider issue creation, PR comments, merge decisions, and close decisions
   out of the CLI.
3. Implement deterministic commands for:
   - scope classification;
   - finding validation and normalization;
   - stable fingerprinting, merge, dedupe, and confidence filtering;
   - render profiles for terminal, report, issue body, PR comment, and evidence;
   - bundle output for workflow artifacts.
4. Support path link rendering as a pure formatter. The CLI may render GitHub
   source links from `--repo` and `--ref`, but it must not call GitHub APIs.
5. Treat downstream `agent-kit` skill migration as a release/adoption handoff.
   This plan may add compatibility docs and fixtures, but live `agent-kit`
   edits belong to a separate repo change after the primitive is available.

## Scope

In scope:

- `review-specialists` CLI contract inside `nils-agent-workflow-primitives`.
- Rust data models for specialist findings, merged findings, scope metadata,
  report metadata, red-team trigger metadata, and render profiles.
- JSONL input validation with severity alias normalization and confidence
  range checks.
- Stable fingerprint generation and deterministic dedupe.
- Markdown and JSON renderers for user-facing reports and issue/PR comments.
- Bundle output containing normalized findings, merged findings, report
  markdown, issue body, and scope JSON when provided.
- Tests and fixtures for malformed JSONL, duplicate findings, threshold
  filtering, link rendering, and bundle output.
- Documentation for how `code-review-specialists` should consume the primitive.

Out of scope:

- Running specialist prompts or calling LLM providers.
- Spawning subagents.
- Posting GitHub or GitLab comments.
- Opening issues or PRs.
- Making merge, close, or request-changes decisions.
- Building an accepted-risk or suppression database.
- Replacing `review-evidence`; this primitive can render evidence-compatible
  data, but retained review records stay owned by `review-evidence`.

## Requirements

- The CLI must be deterministic for identical inputs.
- The CLI must reject malformed finding rows with actionable line-numbered
  errors.
- Severity aliases must normalize to the canonical set:
  `critical`, `high`, `medium`, `low`, `info`.
- Confidence must be a number from `0.0` to `1.0`.
- Missing required fields must be reported before merge/render output is
  created.
- Fingerprints must be stable across runs and independent of input row order.
- Rendered Markdown must distinguish specialist review findings from provider
  decisions.
- Render profiles must not imply live provider mutations.
- The CLI must expose `-V, --version` at the root and support completion
  generation following workspace conventions.

## Acceptance Criteria

- `review-specialists validate --input findings.jsonl --format json` emits
  normalized findings or a structured data error.
- `review-specialists merge --input findings.jsonl --summary-out review.md`
  deduplicates by fingerprint and confidence, filters low-confidence findings,
  and writes a Markdown summary.
- `review-specialists render --profile issue-body --input merged.json --out issue.md`
  produces a concise issue-ready body without posting it.
- `review-specialists bundle --out-dir <dir> ...` writes a stable artifact set.
- `review-specialists scope --base <ref> --format json` replaces the
  deterministic scope helper currently embedded in the skill.
- Existing `code-review-specialists` report examples can be reproduced from
  fixtures with equivalent content.
- Docs-only and full workspace validation gates remain green.

## Validation Plan

- `cargo test -p nils-agent-workflow-primitives review_specialists`
- `cargo test -p nils-agent-workflow-primitives --test integration review_specialists`
- `cargo run -p nils-agent-workflow-primitives --bin review-specialists -- --help`
- `cargo run -p nils-agent-workflow-primitives --bin review-specialists -- validate --input <fixture> --format json`
- `cargo run -p nils-agent-workflow-primitives --bin review-specialists -- merge --input <fixture> --summary-out <tmp>/review.md`
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
- `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage`

## Risks And Guardrails

- Keep the CLI as a primitive. If implementation starts to embed reviewer
  judgment, move that logic back to the skill.
- Do not hard-code local `$HOME`, `$AGENT_HOME`, or `/Users/terry` paths in
  repo docs or tests.
- Do not make rendered issue bodies so verbose that they replace the source
  report. Issue/PR profiles should summarize and link to artifacts.
- Do not let bundle output hide validation failures. Invalid findings should
  fail before writing partial merged artifacts unless an explicit
  `--allow-invalid` mode is later designed.
- Avoid broad downstream `agent-kit` edits in this repo plan. Publish a stable
  CLI contract first, then migrate the skill in its own repo.

## Open Questions

- Should the first released implementation include `scope`, or should `scope`
  follow after `validate`/`merge`/`render` are stable? Recommended default:
  include `scope` in this plan because it already exists as deterministic helper
  behavior and completes the Python-helper replacement.
- Should issue and PR render profiles produce identical sections? Recommended
  default: share the findings table, but keep issue bodies outcome-oriented and
  PR comments review-oriented.

## Read-First References

- `$AGENT_HOME/skills/workflows/code-review/code-review-specialists/SKILL.md`
- `$AGENT_HOME/skills/workflows/code-review/code-review-specialists/references/SPECIALIST_REVIEW_CONTRACT.md`
- `$AGENT_HOME/skills/workflows/code-review/code-review-specialists/references/SPECIALIST_REVIEW_REPORT_TEMPLATE.md`
- `crates/agent-workflow-primitives/README.md`
- `crates/agent-workflow-primitives/src/review_evidence.rs`
- `crates/agent-workflow-primitives/src/skill_usage.rs`

## Retention Intent

This source doc is execution coordination. It can be deleted with the sibling
plan bundle after the implementation is complete and any durable contract docs
have been promoted into crate docs or runbooks.
