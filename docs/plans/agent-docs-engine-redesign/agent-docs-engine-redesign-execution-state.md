# agent-docs Engine Redesign Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: done — engine redesign shipped in nils-cli v0.30.0; all four
  sprints complete and delivered.
- Target scope: `crates/agent-docs` engine redesign in `sympoies/nils-cli`.
  Upstream of graysurf/agent-runtime-kit#181, whose Sprints 2-4 consume this
  release.
- Execution window: Sprint 1 (catalog foundation) → Sprint 2 (when +
  content validation) → Sprint 3 (command surface + docs-home + init) →
  Sprint 4 (content-emitting preflight + delivery), serial.
- Current task: none — all ledger rows done; tracking closeout pending.
- Next task: graysurf/agent-runtime-kit#181 Sprints 2-4 (kit-side adoption),
  gated on this release.
- Last updated: 2026-05-30
- Branch/commit/PR: implemented on `feat/agent-docs-engine-redesign`;
  squash-merged via PR #671 as commit `7ea5563` to `sympoies/nils-cli` main
  (CI-fix follow-up `72d253c`); released as tag `v0.30.0` (bump PR #674).
- Source document: docs/plans/agent-docs-engine-redesign/agent-docs-engine-redesign-plan.md
- Direct source-doc execution waiver: not applicable
- Tracking issue: sympoies/nils-cli#662
- Source snapshot: posted by `create-plan-tracking-issue` at issue open
- Plan snapshot: posted by `create-plan-tracking-issue` at issue open
- Initial state snapshot: posted by `create-plan-tracking-issue` at issue open

## Validation Plan

- Sprint 1: `cargo test -p agent-docs` config/model and resolver/baseline
  cases pass with catalog-driven resolution; no hardcoded builtins remain.
- Sprint 2: `when` evaluation cases (true / false / `||` / `&&`) pass;
  content-validation cases fail a zero-byte or marker-less doc.
- Sprint 3: `agent-docs --help` snapshot updated to the new surface; no doc
  listed twice; env-resolution cases cover symlink-derived docs-home; `init`
  cases plus `rumdl` on a generated stub.
- Sprint 4: `preflight --intent project-dev --format json` emits content +
  the validation contract against a fixture; `cargo test` / clippy / fmt /
  `rumdl` green; `gh pr checks` green; release published and tap bumped.
- Cross-cutting: every executed task populates its `Evidence` cell; waived
  tasks are marked `waived` with a reason. The closeout comment is preceded by
  a final `tracking run update --note "<closing summary>"` event.

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | done | Define the catalog schema and model | Engine commit 5ba826b on feat/agent-docs-engine-redesign; cargo test (15 unit + 47 integration), clippy -D warnings, fmt, completion + contract + third-party gates green. | `crates/agent-docs` `model.rs` + `config.rs`. Contexts/docs as data; default catalog inheritable. |
| 1.2 | done | Remove hardcoded builtins from resolution and baseline | Engine commit 5ba826b on feat/agent-docs-engine-redesign; cargo test (15 unit + 47 integration), clippy -D warnings, fmt, completion + contract + third-party gates green. | Depends on 1.1. Drive `resolver.rs` + `baseline.rs` from the catalog; drop the `required=false` opt-out path. |
| 2.1 | done | Implement the `when` predicate evaluator | Engine commit 5ba826b on feat/agent-docs-engine-redesign; cargo test (15 unit + 47 integration), clippy -D warnings, fmt, completion + contract + third-party gates green. | Depends on 1.2. `path-exists:<glob>` + `\ | \ | `/`&&`; replaces opt-out. |
| 2.2 | done | Add content validation | Engine commit 5ba826b on feat/agent-docs-engine-redesign; cargo test (15 unit + 47 integration), clippy -D warnings, fmt, completion + contract + third-party gates green. | Depends on 1.2. Non-empty + marker + optional freshness; placeholder fails. |
| 3.1 | done | Collapse command surface; retire old commands; dedupe | Engine commit 5ba826b on feat/agent-docs-engine-redesign; cargo test (15 unit + 47 integration), clippy -D warnings, fmt, completion + contract + third-party gates green. | Depends on 1.2. `audit`/`preflight`/`init`/`explain`/`list`/`remove`; drop `resolve`/`baseline`/`scaffold-*`/`startup`; dedupe by resolved path. |
| 3.2 | done | Symlink-derived docs-home | Engine commit 5ba826b on feat/agent-docs-engine-redesign; cargo test (15 unit + 47 integration), clippy -D warnings, fmt, completion + contract + third-party gates green. | Depends on 3.1. `dirname(readlink ~/.claude/CLAUDE.md)`; keep `--docs-home`; clear error when unresolvable. |
| 3.3 | done | `init` annotated override stub | Engine commit 5ba826b on feat/agent-docs-engine-redesign; cargo test (15 unit + 47 integration), clippy -D warnings, fmt, completion + contract + third-party gates green. | Depends on 3.1. `--print`/`--dry-run`/`--force`; lists inherited defaults; never dumps full defaults. |
| 4.1 | done | Content-emitting `preflight` + validation-contract resolution | Engine commit 5ba826b on feat/agent-docs-engine-redesign; cargo test (15 unit + 47 integration), clippy -D warnings, fmt, completion + contract + third-party gates green. | Depends on 2.2, 3.1. Documented, versioned JSON shape; cross-repo contract the kit pins. |
| 4.2 | done | Tests, `--help` snapshot, and release | Integration tests + --help snapshot updated and green (commit 5ba826b; CI-fix 72d253c for docs-hygiene + shared-helper guardrails). PR #671 squash-merged as 7ea5563; nils-cli v0.30.0 released (bump PR #674, GitHub Release + homebrew-tap formula bumped to v0.30.0). | Depends on 4.1. Host binary left at 0.29.1 (not brew-upgraded) so the kit exact-match pin stays aligned until graysurf/agent-runtime-kit#181 adopts the new surface. |

## Session Log

- 2026-05-30: Authored this bundle (discussion-source + plan +
  execution-state) as the engine slice of the agent-docs redesign. The
  authoritative full design and cross-repo decisions live in the
  agent-runtime-kit source doc and tracker graysurf/agent-runtime-kit#181;
  this nils-cli bundle covers only `crates/agent-docs`. Conclusion: make the
  catalog data-driven (remove hardcoded builtins), add real `when` predicates
  and content validation, collapse the command surface, derive the docs-home
  from the install symlink, and make `preflight` emit doc content plus the
  per-repo validation contract so the kit hooks can inject and enforce it. No
  implementation started; this state is prepared so `create-plan-tracking-issue`
  can open the tracker with a populated ledger. Authored in an isolated
  worktree off `origin/main` to avoid disturbing the shared checkout.
- 2026-05-30: Implemented all four sprints in `crates/agent-docs`
  (data-driven `AGENT_DOCS.toml` catalog, `when` predicates with a
  dependency-free glob, content validation with optional freshness, the
  collapsed `audit`/`preflight`/`init`/`explain`/`list`/`remove` surface,
  symlink-derived docs-home, and the content-emitting
  `preflight --intent X --format json` cross-repo contract). 15 unit + 47
  integration tests green; full local required-checks gate green. Opened
  PR #671; CI flagged two repo guardrails (docs-hygiene `legacy`-keyword ban
  and a shared-helper-adoption seed pointing at a removed test) — fixed in
  commit `72d253c`, re-verified locally and on CI, then squash-merged as
  `7ea5563`. Released nils-cli `v0.30.0` (bump PR #674, tag `v0.30.0`):
  GitHub Release published and homebrew-tap formula bumped. The host binary
  was deliberately left at 0.29.1 (release ran with
  `--skip-local-brew-upgrade --skip-dev-clean`) so the agent-runtime-kit
  exact-match version pin stays aligned until the kit adopts the new surface
  (graysurf/agent-runtime-kit#181).

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `cargo test -p nils-agent-docs` | pass | 15 unit + 47 integration tests green (catalog parse, resolution, `when`, content validation, command surface, docs-home, init, preflight contract, worktree fallback). | local-validation.md |
| `nils-cli-verify-required-checks.sh` (full local gate) | pass | clippy `-D warnings`, fmt, tests, completion + cli-output-contract + third-party + docs-hygiene + shared-helper audits all green. | verify-after-hygiene-fix-2.log |
| `gh pr checks 671` | pass | test, test_macos, coverage, CodeQL, and all Analyze jobs green after the CI-fix commit. | PR #671 |
| `release.yml` + tap `update-nils-cli-formula.yml` | pass | GitHub Release v0.30.0 published with assets; homebrew-tap formula bumped to v0.30.0. | release-0-30-0-tap-final.log |

## Notes

- Upstream dependency for graysurf/agent-runtime-kit#181: this engine release
  unblocks the kit's Sprints 2-4 (the kit pins the new surface via
  `required_clis` after the release ships).
- No backward compatibility required: breaking the CLI surface is acceptable
  as long as in-repo callers, fixtures, and the `--help` snapshot are updated
  in the same release.
- The `preflight --intent` JSON output shape is the cross-repo contract; it is
  defined in Sprint 4 and documented for the kit to pin.
- Authored in worktree
  `~/Project/sympoies/nils-cli-worktrees/agent-docs-engine-redesign` on branch
  `feat/agent-docs-engine-redesign`; the shared `nils-cli` main checkout was
  not disturbed.
