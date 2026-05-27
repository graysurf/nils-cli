# Plan: heuristic-inbox `new` non-skill-usage source mode

## Overview

Decouple `heuristic-inbox new` from its mandatory `--from-skill-usage`
argument so that workflow gaps diagnosed outside a single named-skill
invocation can scaffold a `verify --strict`-passing curated error-inbox
entry. The change adds two sources and a mutual-exclusivity gate:

- `--from-evidence <PATH>`: reuse the existing `ingest-evidence`
  redaction (`redact_ingest_source`) to read and redact an arbitrary
  evidence file, copy the redacted file under the new case's
  `evidence/` directory, and point the entry's `Raw record:` line at
  it.
- `--manual`: scaffold a skeleton with no captured raw evidence,
  auto-filling `Raw record: not captured (manual diagnosis, <date>)`
  so the entry still satisfies the `verify --strict`
  `missing raw evidence pointer` check while honestly recording that
  no evidence was captured.
- A clap `ArgGroup` makes exactly one of
  `--from-skill-usage | --from-evidence | --manual` required, so the
  existing skill-usage path is preserved and the two new modes cannot
  be combined.

The entry-body shape, the seven required sections, and the redaction
guarantees are unchanged. The skill-usage path stays byte-compatible
for its `Status`, `Signal`, and `Evidence` rendering.

Source: this bundle's discussion source doc (Read First, below).

## Read First

- Primary source:
  `docs/plans/heuristic-inbox-non-skill-usage-source/heuristic-inbox-non-skill-usage-source-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Source issue: sympoies/nils-cli#585
- Open questions carried into execution: none (option 3 — all three
  mutually exclusive sources — locked at the source doc).
- Implementation surface:
  `crates/agent-workflow-primitives/src/heuristic_inbox.rs`
  (`NewArgs`, `run_new`), reusing `redact_ingest_source`.
- Out of scope (tracked separately): the heuristic-inbox SKILL /
  `HEURISTIC_SYSTEM.md` doc update in `agent-runtime-kit`.

## Scope

- In scope:
  - `--from-evidence <PATH>` and `--manual` flags on `NewArgs`, plus a
    `new_source` `ArgGroup` requiring exactly one source.
  - Refactor of `run_new` into per-source resolvers
    (`resolve_skill_usage_source` / `resolve_evidence_source` /
    `resolve_manual_source`) and a shared `compose_entry`.
  - Copying redacted evidence under the case `evidence/` directory for
    `--from-evidence`; refusal when the source fails redaction
    (reusing `redact_ingest_source` violations, including the raw
    skill-usage record guard).
  - Updated `new` subcommand doc string and root `EXAMPLES` help.
  - Regenerated `completions/zsh/_heuristic-inbox` and
    `completions/bash/heuristic-inbox`.
  - Integration tests for `--from-evidence`, `--manual`, and source
    mutual-exclusivity.
- Out of scope:
  - Any change to `ingest-evidence`, `verify`, or the redaction rules.
  - Any change to the entry-body section set or the `verify_case`
    contract.
  - The `agent-runtime-kit` SKILL / `HEURISTIC_SYSTEM.md` doc update.

## Assumptions

- `redact_ingest_source` is the correct shared redaction primitive for
  arbitrary evidence; its existing violation set (raw skill-usage,
  too-large, binary, secret-pattern) is the right gate for
  `--from-evidence`.
- A `uncategorized` default `Area` (overridable via `--area`) and a
  `today_utc()` `First observed` are acceptable for the two new modes,
  which have no skill name or record timestamp to derive from.
- The `manual` `Raw record: not captured (manual diagnosis, <date>)`
  pointer is acceptable to `verify --strict` (non-empty raw-record
  line) and clearly signals the absence of evidence to a reader.

## Sprint 1: Non-skill-usage source modes

**Goal**: `heuristic-inbox new` accepts `--from-evidence` and
`--manual` as alternatives to `--from-skill-usage`, requires exactly
one, and every mode produces a `verify --strict`-passing entry, with
the skill-usage path unchanged.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-agent-workflow-primitives --test integration`
  - `heuristic-inbox new --manual --slug live-gap --area cli --severity high`
  - `heuristic-inbox new --from-evidence <file> --slug ev-gap`
  - `heuristic-inbox verify <case> --strict --format json`
- Verify: both new modes emit `ok: true` and `verify --strict` returns
  `ok: true`; supplying two sources or none fails with a usage error.

### Task 1.1: Add `--from-evidence` / `--manual` sources and mutual-exclusivity gate

- **Location**:
  - `crates/agent-workflow-primitives/src/heuristic_inbox.rs`
    (`NewArgs`)
- **Description**: Make `from_skill_usage` an `Option<PathBuf>`, add
  `from_evidence: Option<PathBuf>` and `manual: bool`, and attach a
  `new_source` `ArgGroup` (`required(true)`) over the three so clap
  enforces exactly one. Update the `new` subcommand doc string.
- **Dependencies**: none
- **Complexity**: 1
- **Acceptance criteria**:
  - `new` with no source fails with the clap required-argument error.
  - `new` with two sources fails with the clap conflict error.
  - `--from-skill-usage` continues to parse as before.
- **Validation**:
  - `cargo test -p nils-agent-workflow-primitives --test integration cli::heuristic_inbox`

### Task 1.2: Resolve each source and compose the entry

- **Location**:
  - `crates/agent-workflow-primitives/src/heuristic_inbox.rs`
    (`run_new` and new resolver helpers)
- **Description**: Split `run_new` into per-source resolvers returning
  a `ResolvedSource` (area default, first-observed, signal,
  raw-record display, evidence summary, evidence files) and a shared
  `compose_entry`. `resolve_evidence_source` calls
  `redact_ingest_source`, errors with `evidence-not-redactable` on any
  violation, and writes the redacted copy under the case `evidence/`
  directory with the `Raw record:` line pointing at
  `evidence/<filename>`. `resolve_manual_source` emits
  `Raw record: not captured (manual diagnosis, <date>)`.
- **Dependencies**: Task 1.1
- **Complexity**: 2
- **Acceptance criteria**:
  - `--from-evidence` copies a redacted evidence file into the case and
    the entry passes `verify --strict`.
  - `--from-evidence` on a raw `skill-usage.record.json` is refused.
  - `--manual` produces a `verify --strict`-passing entry with the
    uncaptured raw-record pointer.
  - The skill-usage path renders the same `Status` / `Signal` /
    `Evidence` content as before and never leaks a raw secret.
- **Validation**:
  - `cargo test -p nils-agent-workflow-primitives --test integration cli::heuristic_inbox`

### Task 1.3: Help, completions, and tests

- **Location**:
  - `crates/agent-workflow-primitives/src/heuristic_inbox.rs`
    (root `EXAMPLES`)
  - `completions/zsh/_heuristic-inbox`,
    `completions/bash/heuristic-inbox`
  - `crates/agent-workflow-primitives/tests/integration/cli.rs`
- **Description**: Add `--from-evidence` / `--manual` examples to the
  root help `EXAMPLES`, regenerate the zsh and bash completion assets
  from the rebuilt binary, and add integration tests for the two new
  modes and for source mutual-exclusivity.
- **Dependencies**: Task 1.2
- **Complexity**: 1
- **Acceptance criteria**:
  - Completion flag-parity and asset audits pass with the new flags.
  - New integration tests assert redaction, the uncaptured pointer,
    `verify --strict` success, and the required/conflict source errors.
- **Validation**:
  - `bash scripts/ci/completion-flag-parity-audit.sh --strict`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`

## Risks

- **R-1**: `--from-evidence` could copy unredacted secrets into the
  case. Mitigation: reuse `redact_ingest_source`, which fails closed on
  any violation before the file is written; covered by the
  raw-skill-usage refusal test.
- **R-2**: A `--manual` skeleton with no real evidence could become a
  low-signal entry. Mitigation: the entry records an explicit
  `not captured (manual diagnosis, <date>)` pointer and the summary
  directs the author to attach redacted evidence later via
  `ingest-evidence`; `--manual` is never a default and requires the
  explicit flag.
