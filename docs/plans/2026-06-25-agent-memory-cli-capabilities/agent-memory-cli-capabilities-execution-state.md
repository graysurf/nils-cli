# agent-memory CLI Capabilities Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: tracking issue open (#940); Sprint 1 ready
- Target scope: add four structural / scaffolding subcommands (`check`, `add`,
  `list --json`/`--type`, `search`) to the `nils-agent-memory` crate in
  `sympoies/nils-cli`, per the frozen `graysurf/agent-memory`
  `docs/cli-contract-proposed.md` contract.
- Execution window: Sprint 1 (`check` MVP + collapse the skill bash) -> Sprint 2
  (`add` atomic writer) -> Sprint 3 (`list --json`/`search`, docs, delivery,
  optional release), serial.
- Current task: Sprint 1 ready.
- Next task: Sprint 1 Task 1.1 - define the `check` command surface.
- Last updated: 2026-06-25
- Branch/commit/PR: branch `feat/agent-memory-cli-capabilities`; no PR yet.
- Source document:
  `docs/plans/2026-06-25-agent-memory-cli-capabilities/agent-memory-cli-capabilities-discussion-source.md`
- Plan document:
  `docs/plans/2026-06-25-agent-memory-cli-capabilities/agent-memory-cli-capabilities-plan.md`
- Direct source-doc execution waiver: not applicable.
- Tracking issue: <https://github.com/sympoies/nils-cli/issues/940>
- Source snapshot:
  <https://github.com/sympoies/nils-cli/issues/940#issuecomment-4796671780>
- Plan snapshot:
  <https://github.com/sympoies/nils-cli/issues/940#issuecomment-4796672142>
- Initial state snapshot:
  <https://github.com/sympoies/nils-cli/issues/940#issuecomment-4796672556>

## Validation Plan

- Bundle creation: validate the plan-source bundle before opening the tracker.
- Tracker creation: dry-run `plan-issue record open`, then live-create only if
  labels, title, issue body, lifecycle comments, and repo are correct.
- Initial read-back: audit the live issue with `record audit --profile tracking
  --expect-visible`.
- Sprint 1: targeted `nils-agent-memory` tests for the `check` surface,
  structural checks, frontmatter schema, JSON output, and exit codes.
- Sprint 2: `add` write + atomic index-line append tests, with a post-write
  `check` assertion.
- Sprint 3: `list --json`/`--type` and `search` tests, docs-only validation,
  local-fast validation, and provider PR checks.

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | todo | Define the `check` command surface | pending | Scope + `--all`/`--json`/`--strict`; confirm vs `doctor --strict`. |
| 1.2 | todo | Implement the structural checks | pending | Index/file parity, dangling `[[links]]`, broken index links. |
| 1.3 | todo | Implement frontmatter schema validation | pending | Required name/description/type+enum; warn-level node_type/originSessionId. |
| 1.4 | todo | JSON output, exit codes, and report | pending | `--json` records; `--strict` promotes warnings; exit 0/1/64. |
| 1.5 | todo | Collapse review-global-memory.sh onto the command | pending | Cross-repo (graysurf/agent-memory); gated on a released nils-cli. |
| 2.1 | todo | Define `add` and write the note file | pending | Frontmatter writer; enum + duplicate-slug refusal. |
| 2.2 | todo | Atomic index-line append | pending | `check` clean after `add`; no half-writes. |
| 3.1 | todo | `list --json` and `--type` | pending | Stable JSON; default output unchanged. |
| 3.2 | todo | `agent-memory search` | pending | Body + description match across scopes. |
| 3.3 | todo | Docs, help text, and completion | pending | Update crate docs + memory-repo README; no private paths. |
| 3.4 | todo | Validate and deliver the nils-cli PR(s) | pending | local-fast + provider checks; link PRs to tracker. |
| 3.5 | todo | Release and runtime-surface follow-up | pending | Release if needed to unblock 1.5; else record deferral. |

## Session Log

- 2026-06-25: Operator evaluated the live `agent-memory` CLI surface, agreed the
  daily memory loop bypasses it and the structural checks are duplicated in the
  `review-global-memory` skill bash, authored the frozen contract
  (`graysurf/agent-memory` `docs/cli-contract-proposed.md`), and chose L2 plan
  tracking for all four proposals.

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `plan-tooling validate --file docs/plans/2026-06-25-agent-memory-cli-capabilities/agent-memory-cli-capabilities-plan.md --format text --explain` | pass | Plan valid; 0 errors. | local |
| `bash scripts/ci/plan-bundle-validate.sh --strict --file docs/plans/2026-06-25-agent-memory-cli-capabilities/agent-memory-cli-capabilities-plan.md` | pass | Strict plan-bundle validation passed. | local |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` | pass | Docs placement, hygiene, markdown lint, plan-bundle, CLI contract passed. | local |
| `plan-issue --repo sympoies/nils-cli --format json --dry-run record open --profile tracking ...` | pass | Dry-run rendered the intended issue body, labels, and source/plan/state comments; no local paths. | local |
| `plan-issue --repo sympoies/nils-cli --format json record open --profile tracking ...` | pass | Opened tracker #940 and posted source, plan, and initial state snapshots. | <https://github.com/sympoies/nils-cli/issues/940> |
| `plan-issue --format json record audit --profile tracking --expect-visible ...` | pending | Read-back audit of provider-visible records. | local/provider |
