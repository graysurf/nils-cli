<!-- execute-from-tracking-issue:state:v1 -->
# agent-runtime list-skills Subcommand Execution State

## Execution State

- Status: planning-ready
- Target scope: whole issue
- Execution window: whole issue
- Current task: Task 1.1
- Next task: scaffold `Command::ListSkills` clap surface
- Last updated: 2026-05-25 Asia/Taipei
- Branch/commit/PR/release: `feat/agent-runtime-list-skills`; no PR yet
- Source document:
  docs/plans/agent-runtime-list-skills/agent-runtime-list-skills-plan.md
- Discussion source document:
  docs/plans/agent-runtime-list-skills/agent-runtime-list-skills-discussion-source.md
- Source issue: plan-only waiver
- Tracking issue: TBD (opened by `plan-issue record open --profile tracking`)
- Source snapshot: TBD
- Plan snapshot: TBD
- Initial execution state snapshot: TBD
- Direct source-doc execution waiver: not applicable

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| Task 1.1 | pending | Add `Command::ListSkills` clap surface | — | Define args + register in root help. |
| Task 1.2 | pending | Implement skill enumeration via LinkMap + InstallPlan | — | Project plan symlinks into `SkillRecord` for skill destinations only. |
| Task 1.3 | pending | Add text and JSON v1 formatters | — | Sort by `id`; warnings inline when `--include-warnings`. |
| Task 1.4 | pending | Integration tests on fixture source roots | — | `assert_cmd::Command` covering both products plus warning class. |
| Task 2.1 | pending | Generate bash and zsh completion assets | — | Regenerate via the existing completion generator. |
| Task 2.2 | pending | Update agent-runtime-cli docs | — | README + BINARY_DEPENDENCIES if needed. |
| Task 2.3 | pending | Run full required-checks gate | — | rumdl fmt, third-party-artifacts, completion-asset-audit, Cargo.lock locked-build. |
| Task 3.1 | blocked | Replace regex parse with `list-skills --format json` in agent-runtime-kit | — | Blocked on a nils-cli release that contains `list-skills`. |

## Validation

| Command | Status | Summary | Artifact |
| --- | --- | --- | --- |
| `AGENT_DOCS_HOME=~/Project/graysurf/agent-runtime-kit agent-docs resolve --context startup --strict --format checklist` | pass | Required startup preflight passed before plan-bundle scaffold. | terminal log |
| `AGENT_DOCS_HOME=~/Project/graysurf/agent-runtime-kit agent-docs resolve --context project-dev --strict --format checklist` | pass | Project-dev preflight passed before plan-bundle scaffold. | terminal log |

## Runtime Findings

- None yet.

## Blockers

- Cross-repo rehearsal swap (Task 3.1) is blocked on a released
  `agent-runtime` binary that contains the new subcommand. Sprint 1 and 2
  are not blocked.
