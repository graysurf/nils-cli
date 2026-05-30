# agent-docs Engine Redesign — Implementation Handoff

- Status: decisions settled; ready for plan generation.
- Date: 2026-05-30
- Source: the engine slice of the agent-docs review/redesign. The
  authoritative full design, rationale, and cross-repo decisions live in the
  agent-runtime-kit source doc
  (`graysurf/agent-runtime-kit`:
  `docs/plans/2026-05-30-agent-docs-redesign/2026-05-30-agent-docs-redesign-discussion-source.md`,
  tracker issue graysurf/agent-runtime-kit#181). This nils-cli bundle covers
  only the `crates/agent-docs` engine work that the kit consumes as an
  upstream dependency.
- Intended next step: generate the plan bundle under
  `docs/plans/agent-docs-engine-redesign/`. This is a source artifact, not an
  implementation plan.

## Execution

- Recommended plan: docs/plans/agent-docs-engine-redesign/agent-docs-engine-redesign-plan.md
- Recommended execution state: docs/plans/agent-docs-engine-redesign/agent-docs-engine-redesign-execution-state.md
- Status: decisions settled; plan generation is the next step.
- Next-task source: this document

## Purpose

Rebuild `crates/agent-docs` so policy is data the consuming repo declares and
the binary is a generic resolver and auditor, not a fixed per-context preflight
with hardcoded required docs. The redesign reframes the tool around two jobs:
`audit` (repo health, wiring, content validity) and `preflight --intent`
(resolve what a repo requires and emit it for a hook to inject). The kit then
delivers always-on policy via the harness auto-load path and enforces the
validation contract at the finish line. The engine's job is to make all of
that data-driven and to emit doc content (not just presence) so the kit hooks
can act on it.

## Confirmed Facts (current crate behaviour)

- [F1] Contexts, scopes, and the context-to-required-doc mapping are hardcoded
  Rust in `src/model.rs` and `src/resolver.rs`. Builtins: `startup` ->
  `AGENTS.md` (home and project), `skill-dev`/`project-dev` -> `DEVELOPMENT.md`,
  `task-tools` -> `core/policies/cli-tools.md`. Changing a builtin needs a
  release.
- [F2] `DocumentWhen` (`src/model.rs`) has a single value `Always`. The `when`
  field is parsed and validated (`src/config.rs`) but is functionally inert.
- [F3] The only way to drop a builtin is a project-side `required = false`
  opt-out keyed to the builtin's exact context/scope/path
  (`src/commands/baseline.rs`, `src/resolver.rs`); `startup` can never be
  opted out (shipped in PR #658).
- [F4] `resolve` and `baseline` only check existence
  (`DocumentStatus::Present|Missing`). Neither reads or returns content,
  validates non-emptiness, or checks freshness; output is a presence report
  (`src/output.rs`).
- [F5] `resolve_roots` (`src/env.rs`) requires `AGENT_DOCS_HOME` (or
  `--docs-home`); there is no symlink-derived fallback.
- [F6] The `startup` builtin resolves `AGENTS.md` for both `home` and `project`
  scope, so when `docs_home == project_path` the same file is listed twice
  (no dedupe by resolved path).

## Decisions

Engine decisions (the kit-side decisions D2/D6/D9/D12 are consumed, not
implemented here; see the kit source doc):

1. Reframe the command surface to `audit`, `preflight`, `init`, `explain`,
   `list`, `remove`; remove `resolve`, `baseline`, `scaffold-agents`,
   `scaffold-baseline`, and `add`'s overlap. (kit D1/D10)
2. Make contexts and required docs fully data-driven; remove hardcoded Rust
   builtins; load them from a catalog schema. Ship a default catalog the
   consuming repo inherits or overrides. (kit D3)
3. Implement `when` predicates: `path-exists:<glob>` composed with `||` and
   `&&`, evaluated against the resolved project root. Remove the
   `required = false` opt-out. (kit D4)
4. Validate content, not just existence: non-empty, a required marker, and an
   optional `last-reviewed` freshness check. (kit D5)
5. `preflight --intent X` emits the non-auto-loaded doc set AND each doc's
   content plus the per-repo validation contract, in a stable machine shape a
   hook consumes; it is not a trust-the-agent presence report. (kit D2/D6)
6. Derive the docs-home from the install symlink
   (`dirname(readlink ~/.claude/CLAUDE.md)`) when `--docs-home` is absent;
   keep `--docs-home`; drop the hard `AGENT_DOCS_HOME` requirement. (kit D7)
7. Retire `startup` as a built-in per-task context; dedupe resolved docs by
   resolved path. (kit D8/D10)
8. `init` emits an annotated, editable project-local override stub
   (`--print` to stdout; `--dry-run` / `--force` to write) that lists inherited
   defaults as comments and never dumps a full copy of them. (kit D11)

## Scope

- In scope: the `crates/agent-docs` engine — catalog schema and parser,
  `when` evaluator, content validation, the collapsed command surface,
  symlink-derived docs-home, content-emitting `preflight` output, per-repo
  validation-contract resolution, `init` stub generator, updated integration
  tests and `--help` snapshot, and the release that the kit consumes.
- Out of scope: all kit-side work (default catalog content, inlining the
  global cues into `AGENT_HOME.md`, the awareness-injection and finish-line
  Stop-hook gates, Codex enforcement) — that lives in
  graysurf/agent-runtime-kit#181 and consumes this release.

## Non-Scope

- A general-purpose `when` expression language beyond `path-exists`, glob, and
  boolean composition.
- Implementing the hooks that consume `preflight` output (kit-side).
- Any non-`agent-docs` nils-cli crate.

## Implementation Boundaries

- nils-cli owns the deterministic engine: schema parsing, the `when`
  evaluator, content validation, resolution, symlink-derived location, and the
  JSON / exit-code contracts. The kit owns the catalog content and the hooks.
- The released `preflight` output shape is a contract the kit depends on;
  changes after the kit consumes it require a coordinated `required_clis` bump.
- Delivery follows the nils-cli flow: PR (self-gated via `gh pr checks`) ->
  release -> Homebrew tap. The kit's `required_clis` bump and Sprints 2-4
  follow the release.

## Requirements

1. A catalog schema declares contexts and required docs as data; no hardcoded
   Rust builtins remain.
2. `when` supports `path-exists:<glob>` with `||` / `&&`.
3. Content validation (non-empty + marker; optional freshness) is available to
   `audit` and `preflight`.
4. `preflight --intent X` emits the doc set, each doc's content, and the
   per-repo validation contract in a documented machine shape.
5. Docs-home derives from the install symlink when `--docs-home` is absent.
6. The command surface is `audit`, `preflight`, `init`, `explain`, `list`,
   `remove`; resolved docs are de-duplicated by resolved path.
7. `init` emits an annotated override stub per the kit decision.
8. Integration tests and the `--help` snapshot cover the new surface.

## Acceptance Criteria

- A docs-only repo (no `Cargo.toml` / `package.json` / `src/**`) reports no
  missing code doc with no manual opt-out, via `when`.
- A zero-byte or placeholder required doc fails `audit` and `preflight`.
- With `AGENT_DOCS_HOME` unset and `--docs-home` omitted, the engine locates
  the docs-home via the symlink.
- `preflight --intent project-dev --format json` emits the resolved doc
  content and the validation contract (not just `status=present`).
- `agent-docs --help` shows only the new command set; `resolve` / `baseline` /
  `scaffold-*` are gone; no doc is listed twice.
- `cargo test -p agent-docs` and the `--help` snapshot pass; `rumdl` clean.

## Risks And Guardrails

- The `preflight` output is a cross-repo contract. Guardrail: document the JSON
  shape and version it; the kit pins via `required_clis`.
- Removing `resolve`/`baseline` is a breaking CLI change. Guardrail: no
  backward compatibility is required (per the kit design); update every
  in-repo caller, the `--help` snapshot, and any fixtures in the same release.
- Symlink-derived docs-home couples to the install convention. Guardrail:
  `--docs-home` remains the explicit override; absence of a resolvable home is
  a clear error, not a silent wrong-home selection.

## Validation Plan

- `cargo test -p agent-docs` (unit + integration), `cargo clippy`, `cargo fmt`.
- `--help` snapshot updated and asserted.
- `rumdl check` on changed Markdown.
- The nils-cli CI gates relevant to a changed crate, self-checked via
  `gh pr checks` before merge.

## Read First

- Authoritative full design (cross-repo):
  `graysurf/agent-runtime-kit`:
  `docs/plans/2026-05-30-agent-docs-redesign/2026-05-30-agent-docs-redesign-discussion-source.md`
- Consuming tracker (cross-repo): graysurf/agent-runtime-kit#181
- Engine code: `crates/agent-docs/src/` (`cli.rs`, `model.rs`, `config.rs`,
  `resolver.rs`, `commands/baseline.rs`, `env.rs`, `output.rs`).

## Retention Intent

Plan-source coordination document. Cleanup-eligible after the engine release
ships and the tracker closes and archives.
