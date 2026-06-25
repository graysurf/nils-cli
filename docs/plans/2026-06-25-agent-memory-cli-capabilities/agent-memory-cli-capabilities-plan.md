# Plan: agent-memory CLI Capabilities

## Overview

Implement four proposed `agent-memory` subcommands in the `nils-agent-memory`
crate, per the frozen contract at `graysurf/agent-memory` →
`docs/cli-contract-proposed.md`. The CLI today is resolve + scaffold + a shallow
layout `doctor`; this adds the deterministic, structural operations the daily
memory workflow currently does by hand or in a duplicated skill bash script.
Existing commands, the store layout, and the JSON / exit-code conventions stay
stable. Delivered incrementally: `check` (MVP) first, then `add`, then
`list --json` / `--type` and `search`.

## Read First

- Primary source: `docs/plans/2026-06-25-agent-memory-cli-capabilities/agent-memory-cli-capabilities-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Frozen external contract: `graysurf/agent-memory` → `docs/cli-contract-proposed.md`
- Repo anchors:
  - `crates/agent-memory/src/cli.rs` (Command enum + arg structs)
  - `crates/agent-memory/src/lib.rs` (command dispatch; current `doctor`)
  - `crates/agent-memory/src/main.rs`
  - `crates/agent-memory/tests/integration/`
  - `crates/agent-memory/tests/integration.rs`
- External workflow anchors:
  - `graysurf/agent-memory` note format: frontmatter (`name`, `description`,
    `metadata.{node_type,type,originSessionId}`) + `MEMORY.md` index line.
  - `graysurf/agent-memory` `.agents/skills/review-global-memory/scripts/review-global-memory.sh`
    (the structural-check logic to collapse onto the new command).
- Key decisions carried into execution:
  - L2 chosen by the operator for all four proposals: a committed, multi-step,
    multi-PR CLI effort with state worth tracking.
  - The CLI owns the deterministic / structural slice ONLY — no staleness
    verification, no what-to-store judgment, no markdown formatting.
  - Frontmatter strictness: `name`, `description`, `metadata.type` are required
    (and `type` must be in the enum); `metadata.node_type` and
    `metadata.originSessionId` are warn-level so valid hand-authored notes are
    not rejected.
  - Ship `check` first; strongest demand signal and it removes the bash
    duplication.
- Open questions carried into execution:
  - Whether `check` is a new subcommand or `doctor --strict`; the contract
    recommends a separate `check` (keeps `doctor` fast/side-effect-free).
    Confirm in Task 1.1.
  - Whether `add` requires `--origin-session-id` or omits `originSessionId` when
    the caller has none (it is warn-level, so omission stays valid).

## Scope

- In scope:
  - `agent-memory check [SCOPE] [--all] [--json] [--strict]`: index/file parity,
    dangling `[[links]]`, broken index markdown links, frontmatter schema.
  - `agent-memory add [SCOPE] --name --type --description [--title] [--hook]
    [--body-file|--body -]`: atomic note write + index-line append.
  - `agent-memory list --json` and `--type <t>` filter.
  - `agent-memory search <term> [SCOPE] [--all]`.
  - Tests, `--help` / spec / README / completion updates, and PR delivery.
  - Collapsing `review-global-memory.sh` onto `agent-memory check` (delivered in
    the `graysurf/agent-memory` repo).
- Out of scope:
  - Fact-staleness verification, "should this be stored" judgment, prose
    formatting (a formatter was evaluated and declined).
  - Changing existing command behavior or the memory-store layout / contract.
  - Full `MEMORY.md` regeneration (curated hook text must not be clobbered;
    `check` reports drift, it does not rewrite the index).

## Assumptions

1. The `nils-agent-memory` crate is the sole implementation home; the store
   layout in `graysurf/agent-memory` does not change.
2. The existing integration harness can build a temp memory store on disk for
   fixtures (clean, drifted, malformed-frontmatter cases).
3. The frozen contract doc is authoritative for command shape and strictness.
4. The work ships through the normal nils-cli PR + release flow before any
   downstream runtime-kit pin changes.
5. The `review-global-memory.sh` collapse is a cross-repo follow-up in
   `graysurf/agent-memory`, tracked here but delivered by its own commit there
   once `check` is released.

## Sprint 1: `agent-memory check` (MVP)

**Goal**: A read-only structural integrity command over any scope that replaces
the skill's hand-rolled bash checks.
**Demo/Validation**:

- Command(s): `agent-memory check global --json`; `agent-memory check --all`.
- Verify: clean store exits 0; an unindexed note or dangling `[[link]]` exits 1
  with a structured finding; a bad flag exits 64.

### Task 1.1: Define the `check` command surface

- **Location**:
  - `crates/agent-memory/src/cli.rs`
  - `crates/agent-memory/src/lib.rs`
- **Description**: Add a `Check` variant (scope arg + `--all`, `--json`,
  `--strict`) to the `Command` enum and wire dispatch. Confirm `check` as a
  separate subcommand vs `doctor --strict` per the contract recommendation.
- **Dependencies**:
  - none
- **Complexity**: 2
- **Acceptance criteria**:
  - `agent-memory check --help` lists scope, `--all`, `--json`, `--strict`.
  - Scope resolution reuses the existing scope parser (`root`/`global`/`<id>`/
    `agents/<id>`/`personas/<id>`).
  - Exit codes follow the CLI convention: 0 ok, 1 issues, 64 usage error.
- **Validation**:
  - `cargo test -p nils-agent-memory cli`

### Task 1.2: Implement the structural checks

- **Location**:
  - `crates/agent-memory/src/lib.rs`
  - `crates/agent-memory/tests/integration/`
- **Description**: Implement index/file parity (every note has an index entry;
  every index link resolves), dangling `[[links]]`, and broken index markdown
  links, for a single scope and `--all`.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 3
- **Acceptance criteria**:
  - A note with no index entry, an index link to a missing file, and a dangling
    `[[link]]` are each reported with `{scope, kind, file, detail}`.
  - A clean store reports zero issues.
  - `--all` sweeps every scope and aggregates findings.
- **Validation**:
  - `cargo test -p nils-agent-memory check`

### Task 1.3: Implement frontmatter schema validation

- **Location**:
  - `crates/agent-memory/src/lib.rs`
  - `crates/agent-memory/tests/integration/`
- **Description**: Validate each note's YAML frontmatter: required `name`,
  `description`, `metadata.type` (in `{user,feedback,project,reference}`);
  warn-level `metadata.node_type` and `metadata.originSessionId`.
- **Dependencies**:
  - Task 1.2
- **Complexity**: 3
- **Acceptance criteria**:
  - A missing required field or out-of-enum `type` is an error (exit 1).
  - A note missing only `node_type` / `originSessionId` is a warning, not an
    error, unless `--strict` is set.
  - Hand-authored notes carrying only `type` validate at warn level.
- **Validation**:
  - `cargo test -p nils-agent-memory frontmatter`

### Task 1.4: JSON output, exit codes, and report

- **Location**:
  - `crates/agent-memory/src/lib.rs`
  - `crates/agent-memory/tests/integration/`
- **Description**: Human-readable grouped report by default; `--json` emits one
  record per finding; `--strict` promotes warnings to failures.
- **Dependencies**:
  - Task 1.2
  - Task 1.3
- **Complexity**: 2
- **Acceptance criteria**:
  - `--json` output is stable and machine-parseable.
  - Exit codes: 0 clean, 1 any error-level finding (or any finding under
    `--strict`), 64 usage error.
  - Snapshot/fixture tests cover clean, drifted, and malformed stores.
- **Validation**:
  - `cargo test -p nils-agent-memory check`
  - `cargo test -p nils-agent-memory exit_codes`

### Task 1.5: Collapse review-global-memory.sh onto the command

- **Location**:
  - `graysurf/agent-memory` `.agents/skills/review-global-memory/scripts/review-global-memory.sh`
- **Description**: After `check` ships in a released nils-cli, reduce the bash
  script to call `agent-memory check global`, keeping only the skill-specific
  retired-path hint sweep (a heuristic, not a structural invariant).
- **Dependencies**:
  - Task 1.4
- **Complexity**: 1
- **Acceptance criteria**:
  - The duplicated parity / link / frontmatter logic is removed from the bash
    script.
  - The skill's structural-check step still passes on the live `global/` store.
  - Delivered by its own commit/PR in `graysurf/agent-memory` (cross-repo).
- **Validation**:
  - `bash .agents/skills/review-global-memory/scripts/review-global-memory.sh`
    (in the agent-memory repo)

## Sprint 2: `agent-memory add`

**Goal**: A single guarded writer so a note and its index line never drift.
**Demo/Validation**:

- Command(s): `agent-memory add global --name foo --type feedback
  --description "..." --hook "..."`.
- Verify: the new `foo.md` has correct frontmatter and `MEMORY.md` gains one
  matching index line; a duplicate slug is refused.

### Task 2.1: Define `add` and write the note file

- **Location**:
  - `crates/agent-memory/src/cli.rs`
  - `crates/agent-memory/src/lib.rs`
- **Description**: Add the `Add` variant and write `<scope>/<slug>.md` with
  frontmatter (`name`, `description`, `type`, `node_type: memory`, and
  `originSessionId` when supplied). Body from `--body-file` or stdin.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 3
- **Acceptance criteria**:
  - Generated frontmatter matches the documented on-disk format.
  - `type` is validated against the enum; an existing slug is refused.
  - `originSessionId` is written only when provided.
- **Validation**:
  - `cargo test -p nils-agent-memory add`

### Task 2.2: Atomic index-line append

- **Location**:
  - `crates/agent-memory/src/lib.rs`
  - `crates/agent-memory/tests/integration/`
- **Description**: Append `- [Title](slug.md) — hook` to the scope's `MEMORY.md`
  in the same operation; on any failure, leave neither the file nor the index
  half-written.
- **Dependencies**:
  - Task 2.1
- **Complexity**: 2
- **Acceptance criteria**:
  - After `add`, `agent-memory check <scope>` reports zero parity issues.
  - A write failure does not leave a note without an index entry (or vice
    versa).
- **Validation**:
  - `cargo test -p nils-agent-memory add`
  - `cargo test -p nils-agent-memory check`

## Sprint 3: `list --json` / `search`, docs, and delivery

**Goal**: Round out structured listing and search, then document and ship.
**Demo/Validation**:

- Command(s): `agent-memory list global --json --type feedback`;
  `agent-memory search worktree --all`.
- Verify: JSON listing carries `path/name/description/type/mtime`; search
  returns file + matching line.

### Task 3.1: `list --json` and `--type`

- **Location**:
  - `crates/agent-memory/src/cli.rs`
  - `crates/agent-memory/src/lib.rs`
  - `crates/agent-memory/tests/integration/`
- **Description**: Add `--json` (emit `path/name/description/type/mtime` per note)
  and `--type <t>` frontmatter filter to `list`, without breaking the existing
  plain output.
- **Dependencies**:
  - Task 1.3
- **Complexity**: 2
- **Acceptance criteria**:
  - Default `list` output is unchanged.
  - `--json` is stable and parseable; `--type` filters by frontmatter type.
- **Validation**:
  - `cargo test -p nils-agent-memory list`

### Task 3.2: `agent-memory search`

- **Location**:
  - `crates/agent-memory/src/cli.rs`
  - `crates/agent-memory/src/lib.rs`
  - `crates/agent-memory/tests/integration/`
- **Description**: Search note bodies + descriptions across a scope (or `--all`),
  returning file + matching line; `--json` for structured hits.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 2
- **Acceptance criteria**:
  - A term present only in a body and one only in a description are both found.
  - `--all` searches every scope; no matches exits 1, matches exit 0.
- **Validation**:
  - `cargo test -p nils-agent-memory search`

### Task 3.3: Docs, help text, and completion

- **Location**:
  - `crates/agent-memory/src/completion.rs`
  - `crates/agent-memory/README.md` (if present) and crate docs
  - `graysurf/agent-memory` `README.md` command table (cross-repo)
- **Description**: Document the new commands, refresh completion, and update the
  `agent-memory` README command table in the memory repo.
- **Dependencies**:
  - Task 3.1
  - Task 3.2
- **Complexity**: 1
- **Acceptance criteria**:
  - `--help` and completion list the new commands.
  - No local machine paths, tokens, or private data appear in docs.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`

### Task 3.4: Validate and deliver the nils-cli PR(s)

- **Location**:
  - full repo
- **Description**: Run changed-scope validation, deliver the PR(s) (grouped per
  sprint or as the operator prefers), and link them to the tracker.
- **Dependencies**:
  - Task 2.2
  - Task 3.3
- **Complexity**: 2
- **Acceptance criteria**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` passes.
  - Provider PR checks pass before merge; the tracker records PR evidence.
- **Validation**:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`

### Task 3.5: Release and runtime-surface follow-up

- **Location**:
  - nils-cli release workflow
  - downstream runtime-kit pin / surface only if needed
- **Description**: If the operator needs the new commands immediately (e.g. to
  collapse the bash script in Task 1.5), release nils-cli and update downstream
  pins via the existing release + sync flow; otherwise record the deferral.
- **Dependencies**:
  - Task 3.4
- **Complexity**: 2
- **Acceptance criteria**:
  - A released nils-cli contains the new commands, or the tracker records why
    release is deferred.
  - Task 1.5 (bash collapse) is unblocked once `check` is released.
- **Validation**:
  - nils-cli release validation, when release is requested.

## Testing Strategy

- Unit: scope resolution, frontmatter parsing (required vs warn fields), index
  line parsing.
- Integration: temp-store fixtures for clean / unindexed-note / dangling-link /
  broken-index-link / malformed-frontmatter; `add` round-trips clean through
  `check`; `list --json` and `search` shape assertions.
- E2E/manual: run `agent-memory check --all` against the live
  `graysurf/agent-memory` store; run the collapsed `review-global-memory.sh`.

## Risks & gotchas

- **Risk**: `check` is too strict and rejects valid hand-authored notes.
  **Guardrail**: `node_type` / `originSessionId` are warn-level; only `--strict`
  promotes them; fixtures include a `type`-only note.
- **Risk**: `add` half-writes (note without index line, or vice versa).
  **Guardrail**: atomic append with a post-write `check` assertion in tests.
- **Risk**: scope creep into staleness / formatting.
  **Guardrail**: the design boundary is explicit and out-of-scope items are
  enumerated; new ideas become separate follow-ups.
- **Risk**: cross-repo coupling (Task 1.5 lives in agent-memory and depends on a
  released nils-cli).
  **Guardrail**: Task 1.5 is gated on Task 3.5 (release) and delivered by its
  own PR in the memory repo.

## Rollback plan

- The new subcommands are additive; reverting their PR removes them without
  touching existing commands or the store.
- Until `check` is released, `review-global-memory.sh` keeps its current
  in-script logic (Task 1.5 is not started before Task 3.5), so nothing in the
  memory repo depends on an unreleased binary.
