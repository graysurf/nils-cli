# Plan: Code Review Specialist Primitives

<!-- markdownlint-disable MD013 -->

## Overview

Add a deterministic `review-specialists` CLI to
`nils-agent-workflow-primitives` for the non-judgment parts of the
`code-review-specialists` workflow. The CLI will validate and normalize
specialist findings, merge duplicate findings, render user/report/issue/PR
profiles, produce workflow bundles, and replace the current Python scope helper.
The `agent-kit` skill remains the orchestration and reviewer-judgment layer.

## Read First

- Primary source: docs/plans/code-review-specialist-primitives/code-review-specialist-primitives-discussion-source.md
- Source type: discussion-to-implementation-doc
- Open questions carried into execution:
  - Whether `scope` lands in the first released implementation or follows after
    validate/merge/render. Default: include it in this plan because it completes
    the deterministic helper migration.
  - Whether issue and PR render profiles share identical sections. Default:
    share the findings table but keep issue bodies outcome-oriented and PR
    comments review-oriented.

## Scope

- In scope:
  - New `review-specialists` binary in `crates/agent-workflow-primitives`.
  - Rust models for specialist findings, scope metadata, merged findings, report
    metadata, red-team trigger metadata, and render profiles.
  - JSONL validation, severity alias normalization, confidence checks, path/line
    validation, stable fingerprinting, dedupe, and confidence thresholding.
  - Render profiles for terminal summary, full Markdown report, GitHub issue
    body, PR comment body, and evidence-compatible JSON.
  - Bundle output for deterministic workflow artifacts.
  - Scope classification equivalent to the existing skill helper.
  - Documentation and fixtures that let `agent-kit` migrate after release.
- Out of scope:
  - Running specialist prompts or selecting findings through LLM judgment.
  - Spawning reviewer subagents.
  - Posting provider comments, opening issues, opening PRs, or making merge
    decisions.
  - Replacing `review-evidence` or `skill-usage`.
  - Editing `agent-kit` skill code in this repo plan.

## Assumptions

1. `crates/agent-workflow-primitives` is the right home because it already owns
   released workflow primitives such as `review-evidence`, `skill-usage`, and
   `docs-impact`.
2. The public binary name will be `review-specialists`.
3. The initial schema can follow the existing specialist finding contract and
   add versioned CLI output envelopes without changing the skill's reviewer
   semantics.
4. The implementation can shell out to `git` for scope detection, matching the
   current helper's behavior, without adding a new git library dependency.
5. Downstream `agent-kit` migration will happen after this primitive is merged
   and available from a local or released `nils-cli` build.

## Sprint 1: Contract and validation surface

**Goal**: Land the binary skeleton and strict finding validation before any
rendering or bundle output depends on it.

**Demo/Validation**:

- Commands:
  - `cargo run -p nils-agent-workflow-primitives --bin review-specialists -- --help`
  - `cargo test -p nils-agent-workflow-primitives review_specialists_validate`

```bash
cargo run -p nils-agent-workflow-primitives --bin review-specialists -- validate \
  --input crates/agent-workflow-primitives/tests/fixtures/review-specialists/findings.valid.jsonl \
  --format json
```

- Verify: valid JSONL normalizes to canonical severity/confidence fields, and
  malformed rows fail with line-numbered data errors.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 1.1: Add the `review-specialists` binary skeleton

- **Location**:
  - crates/agent-workflow-primitives/Cargo.toml
  - crates/agent-workflow-primitives/src/bin/review-specialists.rs
  - crates/agent-workflow-primitives/src/lib.rs
  - crates/agent-workflow-primitives/src/review_specialists.rs
  - crates/agent-workflow-primitives/tests/integration/cli.rs
- **Description**: Register a new `review-specialists` binary with root
  `-V, --version`, completion support, `validate`, `merge`, `render`, `bundle`,
  and `scope` subcommands stubbed through the shared command style used by
  existing workflow primitives.
- **Dependencies**:
  - none
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - `review-specialists --help` lists all planned subcommands.
  - `review-specialists -V` prints the crate version.
  - `review-specialists completion zsh` and `completion bash` work.
  - The workspace binary inventory includes `review-specialists`.
- **Validation**:
  - `cargo run -p nils-agent-workflow-primitives --bin review-specialists -- --help`
  - `cargo test -p nils-agent-workflow-primitives --test integration cli_lists_review_specialists`

### Task 1.2: Define finding and output schemas

- **Location**:
  - crates/agent-workflow-primitives/src/review_specialists.rs
  - crates/agent-workflow-primitives/tests/fixtures/review-specialists/findings.valid.jsonl
  - crates/agent-workflow-primitives/tests/fixtures/review-specialists/findings.invalid.jsonl
- **Description**: Add serializable models for `SpecialistFinding`,
  `NormalizedFinding`, validation errors, severity aliases, confidence bounds,
  optional `line`, optional `category`, optional `fingerprint`, specialist name,
  and optional `test_suggestion`.
- **Dependencies**:
  - Task 1.1
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - Required fields match the existing specialist contract.
  - Severity aliases normalize to `critical`, `high`, `medium`, `low`, and
    `info`.
  - Confidence outside `0.0..=1.0` fails validation.
  - Unknown extra JSON fields are either rejected or explicitly carried through;
    the behavior is documented in tests.
- **Validation**:
  - `cargo test -p nils-agent-workflow-primitives review_specialists_schema`

### Task 1.3: Implement `validate`

- **Location**:
  - crates/agent-workflow-primitives/src/review_specialists.rs
  - crates/agent-workflow-primitives/tests/integration/review_specialists.rs
- **Description**: Implement
  `review-specialists validate --input FINDINGS_JSONL --format text|json`
  with line-numbered JSONL parsing, schema validation, optional
  `--repo <path>` path existence checks, optional path/line checks, and
  normalized output.
- **Dependencies**:
  - Task 1.2
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - Valid findings emit normalized JSON in input order.
  - Invalid JSON, missing required fields, bad severity, and bad confidence
    return exit code `65`.
  - Text output summarizes accepted and rejected rows.
  - JSON output uses a versioned `schema_version`.
  - `--repo` path checks do not require files to exist when the caller disables
    path validation.
- **Validation**:
  - `cargo test -p nils-agent-workflow-primitives review_specialists_validate`

```bash
cargo run -p nils-agent-workflow-primitives --bin review-specialists -- validate \
  --input crates/agent-workflow-primitives/tests/fixtures/review-specialists/findings.valid.jsonl \
  --format json
```

## Sprint 2: Merge, fingerprints, and bundles

**Goal**: Produce deterministic merged findings and workflow artifact bundles
that can be reused by skills, issue bodies, and retained evidence.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-agent-workflow-primitives review_specialists_merge`

```bash
cargo run -p nils-agent-workflow-primitives --bin review-specialists -- merge \
  --input crates/agent-workflow-primitives/tests/fixtures/review-specialists/findings.duplicates.jsonl \
  --summary-out target/review-specialists/review.md \
  --format json
cargo run -p nils-agent-workflow-primitives --bin review-specialists -- bundle \
  --input crates/agent-workflow-primitives/tests/fixtures/review-specialists/findings.valid.jsonl \
  --out-dir target/review-specialists/bundle
```

- Verify: duplicate findings collapse deterministically, low-confidence findings
  move to an appendix, and bundle file names are stable.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 2.1: Implement stable fingerprinting and dedupe

- **Location**:
  - crates/agent-workflow-primitives/src/review_specialists.rs
  - crates/agent-workflow-primitives/tests/fixtures/review-specialists/findings.duplicates.jsonl
  - crates/agent-workflow-primitives/tests/integration/review_specialists.rs
- **Description**: Compute fingerprints from explicit `fingerprint` when
  present, otherwise from `path`, `line`, `category`, and `summary`; dedupe by
  fingerprint; keep the highest-confidence finding as primary; and preserve
  confirming specialists in deterministic order.
- **Dependencies**:
  - Task 1.3
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - Identical input with rows in different order produces identical merged JSON.
  - Explicit fingerprints override computed fingerprints.
  - Duplicate findings retain confirming specialist metadata.
  - Ties are broken deterministically.
- **Validation**:
  - `cargo test -p nils-agent-workflow-primitives review_specialists_fingerprint`

### Task 2.2: Implement `merge`

- **Location**:
  - crates/agent-workflow-primitives/src/review_specialists.rs
  - crates/agent-workflow-primitives/tests/integration/review_specialists.rs
- **Description**: Implement
  `review-specialists merge --input FINDINGS_JSONL --display-threshold THRESHOLD --format text|json --summary-out PATH`
  by reusing validation, applying confidence thresholding, writing merged JSON
  to stdout when requested, and writing Markdown summary when requested.
- **Dependencies**:
  - Task 2.1
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - Default display threshold is `0.60`.
  - Main findings and low-confidence appendix match the existing skill contract.
  - `--summary-out` creates parent directories.
  - Invalid input fails before writing partial output.
  - Text output is concise enough to paste into a chat response.
- **Validation**:
  - `cargo test -p nils-agent-workflow-primitives review_specialists_merge`

```bash
cargo run -p nils-agent-workflow-primitives --bin review-specialists -- merge \
  --input crates/agent-workflow-primitives/tests/fixtures/review-specialists/findings.duplicates.jsonl \
  --summary-out target/review-specialists/review.md
```

### Task 2.3: Implement `bundle`

- **Location**:
  - crates/agent-workflow-primitives/src/review_specialists.rs
  - crates/agent-workflow-primitives/tests/integration/review_specialists.rs
- **Description**: Implement
  `review-specialists bundle --input <findings.jsonl> --out-dir <dir>` to write
  `findings.normalized.jsonl`, `findings.merged.json`,
  `specialist-review.md`, and optional `issue-body.md` when a render profile is
  requested.
- **Dependencies**:
  - Task 2.2
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - Bundle output is stable across repeated runs.
  - Existing files are overwritten only by documented bundle file names.
  - Bundle JSON contains enough metadata to link from `skill-usage`.
  - Invalid findings fail without creating a partial bundle.
- **Validation**:
  - `cargo test -p nils-agent-workflow-primitives review_specialists_bundle`

```bash
cargo run -p nils-agent-workflow-primitives --bin review-specialists -- bundle \
  --input crates/agent-workflow-primitives/tests/fixtures/review-specialists/findings.valid.jsonl \
  --out-dir target/review-specialists/bundle
```

## Sprint 3: Render profiles and link formatting

**Goal**: Make review results directly usable in chat responses, local reports,
GitHub issues, PR comments, and evidence records without live provider
mutation.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-agent-workflow-primitives review_specialists_render`

```bash
cargo run -p nils-agent-workflow-primitives --bin review-specialists -- render \
  --profile terminal \
  --input target/review-specialists/bundle/findings.merged.json
cargo run -p nils-agent-workflow-primitives --bin review-specialists -- render \
  --profile issue-body \
  --input target/review-specialists/bundle/findings.merged.json \
  --repo sympoies/nils-cli \
  --ref HEAD \
  --out target/review-specialists/issue.md
```

- Verify: each profile clearly states that review output is not a merge or
  close decision.

**PR grouping intent**: `group`
**Execution Profile**: `parallel-x2`

### Task 3.1: Implement terminal and report profiles

- **Location**:
  - crates/agent-workflow-primitives/src/review_specialists.rs
  - crates/agent-workflow-primitives/tests/fixtures/review-specialists/expected-report.md
  - crates/agent-workflow-primitives/tests/integration/review_specialists.rs
- **Description**: Implement `render --profile terminal` and
  `render --profile report` from merged JSON. Terminal output should be compact;
  report output should follow the existing specialist report sections.
- **Dependencies**:
  - Task 2.2
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - Terminal profile lists findings by severity and path.
  - Report profile includes Scope, Specialist Dispatch, Findings, Red Team,
    Evidence Reviewed, Residual Risk, and Recommended Next Step sections.
  - Markdown snapshot fixture is stable.
- **Validation**:
  - `cargo test -p nils-agent-workflow-primitives review_specialists_render_report`

### Task 3.2: Implement issue, PR comment, and evidence profiles

- **Location**:
  - crates/agent-workflow-primitives/src/review_specialists.rs
  - crates/agent-workflow-primitives/tests/fixtures/review-specialists/expected-issue-body.md
  - crates/agent-workflow-primitives/tests/fixtures/review-specialists/expected-pr-comment.md
  - crates/agent-workflow-primitives/tests/integration/review_specialists.rs
- **Description**: Implement `render --profile issue-body`,
  `render --profile pr-comment`, and `render --profile evidence`. Issue output
  should be follow-up oriented, PR comment output should be review oriented, and
  evidence output should be compact JSON suitable for linking from
  `review-evidence` or `skill-usage`.
- **Dependencies**:
  - Task 3.1
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - Issue body includes current behavior, desired outcome, findings, checked
    evidence, decision, and next action sections.
  - PR comment body includes findings first, ordered by severity.
  - Evidence profile emits JSON with schema version, counts, artifacts, and
    findings summary.
  - No profile posts to a provider.
- **Validation**:
  - `cargo test -p nils-agent-workflow-primitives review_specialists_render_provider_profiles`

### Task 3.3: Implement source link rendering

- **Location**:
  - crates/agent-workflow-primitives/src/review_specialists.rs
  - crates/agent-workflow-primitives/tests/integration/review_specialists.rs
- **Description**: Add optional `--repo <owner/repo>`, `--ref <sha-or-branch>`,
  and `--link-base <url>` support for Markdown path links. The renderer should
  support GitHub source links without making network calls.
- **Dependencies**:
  - Task 3.2
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - `path:line` renders as a GitHub blob link when `--repo` and `--ref` are
    provided.
  - Local-only Markdown remains available when no link options are provided.
  - Paths outside the repo are rejected or rendered as plain text with a warning.
- **Validation**:
  - `cargo test -p nils-agent-workflow-primitives review_specialists_links`

## Sprint 4: Scope detection and adoption handoff

**Goal**: Replace the remaining deterministic Python helper surface and publish
enough docs for the `agent-kit` skill to migrate cleanly after release.

**Demo/Validation**:

- Commands:
  - `cargo run -p nils-agent-workflow-primitives --bin review-specialists -- scope --base main --format json`
  - `cargo test -p nils-agent-workflow-primitives review_specialists_scope`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
- Verify: scope output includes changed files, diff lines, stack signals,
  suggested specialists, forced specialists, red-team trigger metadata, and
  small-diff skip reasons.

**PR grouping intent**: `group`
**Execution Profile**: `parallel-x2`

### Task 4.1: Implement `scope`

- **Location**:
  - crates/agent-workflow-primitives/src/review_specialists.rs
  - crates/agent-workflow-primitives/tests/integration/review_specialists.rs
- **Description**: Port deterministic scope detection from the current
  `code-review-specialists` helper into Rust. The command should run in a git
  repo, accept `--base <ref>`, count changed files and diff lines, classify file
  categories, infer test framework signals, suggest specialists, and explain
  small-diff skip decisions.
- **Dependencies**:
  - Task 1.1
- **Complexity**:
  - 6
- **Acceptance criteria**:
  - Missing base refs produce a clear runtime error.
  - Empty diffs report zero changed files and `small_diff_skip: true`.
  - Diffs touching tests suggest `testing`.
  - Diffs touching auth/security-sensitive paths suggest `security`.
  - Diffs over the red-team threshold mark `red_team_required: true`.
- **Validation**:
  - `cargo test -p nils-agent-workflow-primitives review_specialists_scope`
  - `cargo run -p nils-agent-workflow-primitives --bin review-specialists -- scope --base main --format json`

### Task 4.2: Add red-team trigger metadata

- **Location**:
  - crates/agent-workflow-primitives/src/review_specialists.rs
  - crates/agent-workflow-primitives/tests/integration/review_specialists.rs
- **Description**: Add deterministic red-team trigger calculation for both
  scope output and merged findings. The CLI should state whether red-team is
  required and why, but should not generate red-team findings.
- **Dependencies**:
  - Task 2.2
  - Task 4.1
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - `diff_lines > 200` triggers red-team metadata.
  - Any `critical` finding triggers red-team metadata.
  - Rendered reports include trigger status and activation reason.
- **Validation**:
  - `cargo test -p nils-agent-workflow-primitives review_specialists_red_team_trigger`

### Task 4.3: Publish adoption docs and fixture parity

- **Location**:
  - crates/agent-workflow-primitives/README.md
  - crates/agent-workflow-primitives/docs/README.md
  - docs/runbooks/review-specialists-primitive.md
  - completions/bash/review-specialists
  - completions/zsh/_review-specialists
  - crates/agent-workflow-primitives/tests/fixtures/review-specialists/skill-helper-parity.jsonl
- **Description**: Document the `review-specialists` command surface, generated
  artifacts, profile semantics, and downstream `agent-kit` migration path. Add a
  parity fixture based on the existing skill helper output so the later skill
  migration can compare old and new behavior.
- **Dependencies**:
  - Task 3.2
  - Task 4.1
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - README binary table lists `review-specialists`.
  - Runbook states that the CLI never posts comments or opens issues.
  - Completion assets are generated and pass completion audits.
  - Parity fixture covers scope, merge, and report rendering.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
  - `zsh -n completions/zsh/_review-specialists`
  - `bash -n completions/bash/review-specialists`

## Testing Strategy

- Unit: schema parsing, severity normalization, confidence checks,
  fingerprinting, dedupe, thresholding, red-team trigger calculation, and link
  generation.
- Integration: command-level fixtures for `validate`, `merge`, `render`,
  `bundle`, and `scope`.
- E2E/manual: run the binary against the existing `code-review-specialists`
  fixture set and compare generated Markdown with the skill helper summary.
- CI: docs-only validation while planning, then full
  `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage`
  for implementation PRs.

## Risks & gotchas

- The CLI must not absorb reviewer judgment. Keep specialist selection,
  finding authoring, and decision-making in skills.
- Provider render profiles can look like live provider actions. Keep command
  names and help text explicit that render only writes local output.
- Scope detection must avoid local-only path assumptions. Tests should build
  temporary git repos instead of relying on this checkout.
- Bundle output can become a dumping ground. Keep the file set small and stable.
- Downstream skill migration should wait until the CLI contract is merged and
  available from the expected local/released binary.

## Rollback plan

- If validation or merge behavior is wrong, keep the binary behind docs and do
  not migrate `agent-kit`; the existing Python helper remains the skill fallback.
- If one render profile is unstable, ship the core `validate` and `merge`
  commands first and defer that profile.
- If `scope` is too large for the first implementation PR, split it into a
  follow-up PR while preserving the same public plan and acceptance criteria.
