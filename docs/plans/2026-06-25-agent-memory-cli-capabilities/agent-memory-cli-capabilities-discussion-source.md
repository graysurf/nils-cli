# agent-memory CLI Capabilities Implementation Handoff

This document is the converged implementation-readiness source for adding four
structural / scaffolding subcommands to the `nils-agent-memory` crate. It
promotes the frozen contract authored in `graysurf/agent-memory` →
`docs/cli-contract-proposed.md` into a nils-cli implementation handoff; it does
not restate the whole contract, it captures intent, decisions, and acceptance.

## Execution

- Recommended plan: docs/plans/2026-06-25-agent-memory-cli-capabilities/agent-memory-cli-capabilities-plan.md
- Recommended execution state: docs/plans/2026-06-25-agent-memory-cli-capabilities/agent-memory-cli-capabilities-execution-state.md
- Tier: L2 plan tracking, chosen by the operator for all four proposals (a
  committed, multi-step, multi-PR CLI effort with state worth tracking).
- Implementation home: `crates/agent-memory/` in `sympoies/nils-cli`.
- Authoritative contract: `graysurf/agent-memory` → `docs/cli-contract-proposed.md`.
- Delivery order: `check` (MVP) → `add` → `list --json` / `search` → docs /
  release. One PR per sprint is acceptable.

## Problem

The live `agent-memory` CLI (1.14.0) is resolve + scaffold + a shallow layout
`doctor` that only checks that `root` / `global/` / `agents/` / `personas/`
exist — it does not validate content. Two gaps follow:

- The daily read/write loop bypasses the CLI entirely: Claude reads via
  `autoMemoryDirectory` and writes notes with the editor by hand (frontmatter
  plus the `MEMORY.md` index line maintained manually); the persona launcher
  globs `personas/*` directly. Index/file parity drift is born at write time
  with nothing guarding it.
- The deterministic structural checks already exist but live OUTSIDE the CLI,
  reimplemented in `graysurf/agent-memory`
  `.agents/skills/review-global-memory/scripts/review-global-memory.sh`
  (~100 lines of bash that even wraps `agent-memory doctor` at the end).
- Concrete failure: a cross-machine sync on 2026-06-25 left two machines each
  adding a new first `MEMORY.md` index bullet — an index-parity drift a `check`
  command would have surfaced before it became a rebase conflict.

## Decisions

- The CLI owns the deterministic / structural slice ONLY. It must NOT take on
  fact-staleness verification, "should this be stored" judgment, or markdown
  formatting (a formatter was evaluated for the store and declined; note bodies
  are intentionally long single lines).
- Frontmatter strictness: `name`, `description`, and `metadata.type` are required
  (and `type` must be one of `user | feedback | project | reference`);
  `metadata.node_type` and `metadata.originSessionId` are warn-level. All 30
  current `global/` notes carry the warn-level fields because they were written
  by the auto-memory mechanism, but a note written by hand per the harness
  instructions may omit `originSessionId` — so do not reject it.
- `check` is a new subcommand, not `doctor --strict`: it keeps `doctor` fast and
  side-effect-free and reads clearer. (Folding into `doctor --strict` is an
  acceptable fallback if review prefers it.)
- `add`'s reach is honest: the harness tells Claude to write notes directly, so
  `add` primarily serves Codex, manual use, and being the single canonical
  writer that keeps frontmatter + index in sync.
- The `review-global-memory.sh` collapse is delivered in the `graysurf/agent-memory`
  repo, gated on a released nils-cli that contains `check`.

## Scope

In scope:

- `agent-memory check [SCOPE] [--all] [--json] [--strict]` — index/file parity,
  dangling `[[links]]`, broken index markdown links, frontmatter schema.
- `agent-memory add [SCOPE] --name --type --description [--title] [--hook]
  [--body-file | --body -]` — atomic note write + index-line append.
- `agent-memory list --json` and `--type <t>`.
- `agent-memory search <term> [SCOPE] [--all]`.
- Tests, help / completion / spec / README updates, PR delivery, optional
  release.
- Collapsing `review-global-memory.sh` onto `agent-memory check` (cross-repo).

Out of scope:

- Staleness verification, what-to-store judgment, prose formatting.
- Changing existing command behavior or the memory-store layout / contract.
- Full `MEMORY.md` regeneration — `check` reports parity drift, it does not
  rewrite curated index hook text.

## Requirements

- New subcommands are additive and do not change existing output or exit-code
  behavior.
- Scope resolution reuses the existing parser
  (`root`/`global`/`<id>`/`agents/<id>`/`personas/<id>`), plus `--all`.
- Exit codes follow the established convention: 0 success, 1 runtime / findings,
  64 usage error.
- `--json` output is stable and machine-parseable for skills / CI.
- `check` findings carry `{scope, kind, file, detail}`.
- `add` is atomic: never leaves a note without its index line or vice versa.

## Acceptance Criteria

- `agent-memory check` reports index/file parity gaps, dangling `[[links]]`,
  broken index links, and frontmatter violations, with required-vs-warn
  severity, across a single scope and `--all`.
- A clean store exits 0; an error-level finding (or any finding under
  `--strict`) exits 1; a usage error exits 64.
- `agent-memory add` produces a note whose frontmatter matches the documented
  format and a matching `MEMORY.md` index line, such that `check` is clean
  afterward; a duplicate slug is refused.
- `list --json` carries `path/name/description/type/mtime`; `--type` filters by
  frontmatter type; default `list` output is unchanged.
- `search` finds terms in both bodies and descriptions across scopes.
- `review-global-memory.sh` is reduced to call `agent-memory check global`,
  keeping only the retired-path heuristic sweep, and its structural step still
  passes on the live store.

## Validation Plan

- `plan-tooling validate --file <plan> --format text --explain`
- `bash scripts/ci/plan-bundle-validate.sh --strict --file <plan>`
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
- `cargo test -p nils-agent-memory` (cli / check / frontmatter / add / list /
  search / exit_codes targets)
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
- Manual: `agent-memory check --all` against the live `graysurf/agent-memory`
  store; run the collapsed `review-global-memory.sh`.

## Risks And Guardrails

- **Risk**: `check` rejects valid hand-authored notes. **Guardrail**: warn-level
  `node_type` / `originSessionId`; only `--strict` promotes them; a `type`-only
  fixture is included.
- **Risk**: `add` half-writes. **Guardrail**: atomic append plus a post-write
  `check` assertion in tests.
- **Risk**: scope creep into staleness / formatting. **Guardrail**: explicit
  design boundary; new ideas become separate follow-ups.
- **Risk**: cross-repo coupling for the bash collapse. **Guardrail**: Task 1.5
  is gated on the nils-cli release and delivered by its own PR in the memory
  repo.

## Read-First References

- `graysurf/agent-memory` → `docs/cli-contract-proposed.md` (frozen contract).
- `graysurf/agent-memory` → `DEVELOPMENT.md` (note file format) and `AGENTS.md`
  (memory content boundaries).
- `graysurf/agent-memory` →
  `.agents/skills/review-global-memory/scripts/review-global-memory.sh`.
- `crates/agent-memory/src/{cli.rs,lib.rs,main.rs,completion.rs}` and
  `crates/agent-memory/tests/integration/`.
