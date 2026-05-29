# Changelog

All notable changes to `nils-plan-issue-cli` are documented here. The
format follows Keep a Changelog and the project follows semantic
versioning.

## [Unreleased]

### Added (Plan-Issue vNext)

- `plan-issue record template --kind <role> --shape markdown|json` —
  non-mutating preview of every lifecycle role's visible body and JSON
  payload skeleton, driven by the new vNext registry.
- `plan-issue record audit --expect-visible` — opt-in visible
  completeness lint over the latest comment body per role. Returns a
  `visible` block with stable role-specific failure codes
  (`state-missing-task-ledger`, `validation-missing-overall`, …) alongside
  the existing hidden-payload audit result.
- New `plan-issue tracking` surface covering:
  - `tracking status` — reconcile provider issue evidence with optional
    local run state and return the FSM state + recommended next action.
  - `tracking run init` / `tracking run update` — typed local run-state
    persistence (`plan-issue.execution-run.v1`) plus append-only
    `events.jsonl` journal (`plan-issue.execution-event.v1`) under the
    issue-scoped runtime root.
  - `tracking checkpoint` — default dry-run rendering of role-allowed
    lifecycle comments synthesized from the run state. `--live` opts in
    to provider mutation; until the live adapter ships the controller
    returns a `tracking-checkpoint-live-not-implemented` blocker.
  - `tracking close-ready` — strict, non-mutating close-readiness probe
    with role-specific blocked codes and visible-completeness gating.
- `lifecycle_vnext` module tree (`registry`, `templates`, `visible_lint`,
  `payloads`, `render`) plus `tracking` module tree (`run_state`,
  `events`, `fsm`, `reconcile`, `checkpoint`, `close_ready`) so the
  vNext controller has a clean boundary outside the catch-all executor.
- `record_compat_baseline` integration tests locking the released
  `record` subcommand surface, envelope, error shape, and tracking
  schema constants before runtime-kit migrates.

### Changed

- `tracking checkpoint --post state` now emits an **accumulative** `tasks[]`
  hidden payload: every state post carries the full per-task table from the
  canonical execution-state `## Task Ledger` (when one is recorded), so the
  provider issue is self-contained per-task history matching the visible
  ledger. Falls back to the single-current synthesized baseline when no
  execution-state file is recorded. New-format-only; no v2 reader or mixed
  old/new stream reconciliation. (graysurf/plan-tracking-testbed#16,
  sympoies/nils-cli#628)
- `state.tasks[].status` now shares the execution-state ledger vocabulary
  (`pending|in-progress|done|deferred|blocked|waived`); the `TaskRowStatus`
  payload enum gained `blocked` and `waived`.
- Documented the next state payload replacement as a new-format-only contract:
  `record audit`, `record repair-dashboard`, `tracking status`, and
  `tracking close-ready` target the active payload contract, while old
  provider issues require one-off migration/repair instead of long-term v2
  readers. (sympoies/nils-cli#628)

### Fixed

- `plan-issue record close` closeout-comment table renders the
  `Required` column as `none required` for GitLab linked-PR rows
  instead of the misleading `unknown` label that the GitLab adapter
  produced before this fix. GitLab has no first-class required-check
  concept, so `forge_cli_adapter::pr_merge_summary` now reports zero
  required checks (`required_state=Some("success"), required_count=
  Some(0)`) — the same shape the GitHub adapter returns for a branch
  without a required-check rule — and the close gate treats this as a
  clean resolve per the #502 "non-required failures never block close"
  contract. The `closeout.v1` payload wire format is unchanged; only
  the rendered Markdown cell changes. (sympoies/nils-cli#557, follow-up
  to #563)
- `plan-issue record close` closeout-comment table renders the
  `Required` column as `none required` (zero required checks defined),
  `pass (N)`, `fail (N)`, `none`, or `unknown` instead of collapsing
  every `required_state == None` cause into the single misleading
  `unknown` label. Two underlying repairs feed the new rendering: the
  GitHub adapter's `pr_required_summary` now drops the unsupported
  `conclusion` field from the `gh pr checks --required --json …` call
  list (the call was exiting 5 with `Unknown JSON field: "conclusion"`
  on current `gh`) and recognises `gh`'s `no required checks reported
  on the '<branch>' branch` stderr message as the canonical
  zero-required success rollup, mapping it to
  `(Some("success"), Some(0), [])` so the renderer can pick the new
  `none required` label. The `closeout.v1` payload wire format is
  unchanged; historical closeout records remain immutable.
  (sympoies/nils-cli#561, observation source #541)
- `plan-issue record close` no longer collapses non-required GitHub
  `statusCheckRollup` failures into `linked-pr-not-merged`. The strict
  closeout gate now consults a separate required-check rollup (via
  `gh pr checks <pr> --required`), passes when required checks succeed
  even if non-required workflows failed, and emits the distinct
  `linked-pr-checks-failed` blocker code when required checks actually
  fail. Provider adapters return `required_state`, `required_count`, and
  the list of non-required failures alongside the existing aggregate
  rollup. (sympoies/nils-cli#502)

### Added

- `plan-issue record close` accepts
  `--allow-non-required-check-failure` plus
  `--allow-non-required-check-failure-reason <text>` as an explicit,
  evidence-emitting override for the degraded-provider case where
  required-check state cannot be resolved. The override decision and
  observed non-required failures are recorded under
  `non_required_check_override` in the closeout-comment payload, and the
  comment summary advertises that the override was used.

### Documentation

- `docs/specs/issue-backed-plan-record-contract-v2.md` now states
  explicitly that `plan-issue record audit` does not validate
  provider-issue labels and does not accept a `--label` flag. Callers
  that need to verify expected labels must do so through the provider
  (e.g. `gh issue view --json labels`, `forge-cli pr view`) as a separate
  gate. Label mutation remains the responsibility of `record open`,
  `record post`, and `record close` via `--label`, `--add-label`, and
  `--remove-label`. (sympoies/nils-cli#535)

### BREAKING (Plan-Issue Lifecycle v3)

- The `plan-issue record` surface is rewritten around the v3 issue-backed
  plan record contract (see
  [`docs/specs/issue-backed-plan-record-contract-v2.md`](docs/specs/issue-backed-plan-record-contract-v2.md)
  and [`docs/specs/plan-issue-state-machine-v2.md`](docs/specs/plan-issue-state-machine-v2.md)).
  Consumers (notably `agent-runtime-kit`) must migrate after upgrading to
  the next plan-issue-cli release. There is no migration shim.
  - The retired marker-family selector is removed. There is now one canonical
    marker family
    `<!-- plan-issue-record:v2 role=<role> profile=<profile> -->`. Pre-v2
    markers are reported by audit as `unsupported_markers` and ignored as
    current lifecycle evidence.
  - Audit JSON renames `audit.markers` to `audit.evidence` and indexes by
    role (`source`, `plan`, `state`, `session`, `validation`, `review`,
    `closeout`). Each entry exposes the latest URL, created timestamp,
    profile, role, status, and the parsed structured payload. Audit also
    surfaces a stable `missing_required` array (`source-missing`,
    `plan-missing`, `state-missing`).
  - Every v2 lifecycle comment carries a hidden structured payload. Audit,
    dashboard repair, and closeout gating consume that payload as the source
    of truth; older visible `plan-issue-record-payload` fences remain
    accepted for existing records. Prose-Markdown status parsing is no longer
    used.
  - `record post` validates `--payload-file` against the selected lifecycle
    role before rendering or posting. Audit and dashboard repair now fail on
    malformed typed v2 payloads instead of treating the marker as valid
    evidence and rendering dashboard summary fields as `pending`.
  - The standalone closeout helper command and its
    `--require-complete`, `--require-session`, `--require-validation`,
    `--require-review`, `--require-closeout` flags are retired.
    Closeout-gate evaluation now runs inside `record close` and is
    strict by default. Failure modes return stable codes
    (`source-missing`, `plan-missing`, `state-missing`,
    `state-not-complete`, `state-tasks-incomplete`, `validation-missing`,
    `validation-failed`, `review-missing`, `review-rejected`,
    `review-unresolved-findings`, `linked-pr-not-merged`,
    `approval-missing`).
  - The retired record helper subcommands are removed from the CLI
    parser. Consumers must use `record open`, `record post`,
    `record repair-dashboard`, and `record close`.
  - `record close` now requires a non-empty `--approval` URL or text. The
    strict gate verifies linked PR evidence through provider state
    (`gh pr view --json state,mergeCommit,statusCheckRollup`) and
    records the resolved `merge_sha` + `checks` rollup back into the
    closeout payload.
  - The JSON envelope `schema_version` for every `record` subcommand
    bumps to `plan-issue-cli.record.<sub>.v2`. v1 readers that only read
    the older fields are still compatible because new fields are
    additive at the result top level, but consumers should pin v2 before
    reading new fields (`issue.url`, `comments.{source,plan,state}`,
    `closeout_url`, `final_dashboard`).

### Added (Plan-Issue Lifecycle v3)

- `plan-issue record open --bundle <dir>` opens a provider issue from a
  plan bundle, validates the plan via `plan-tooling`, verifies source +
  plan files are committed (`--allow-dirty` opts out), posts canonical
  v2 source / plan / initial-state comments with structured payloads,
  and repairs the dashboard with the freshly-created comment URLs.
  Supports `--dry-run` and `--fixture <dir>` deterministic modes.
- `plan-issue record post --issue <n> --kind <state|session|validation|review> --payload-file <p>`
  appends one canonical lifecycle comment with the v2 marker + hidden payload
  carrier. `--kind source|plan` is rejected (owned by `record open`);
  `--kind closeout` is rejected (owned by `record close`).
- `plan-issue record repair-dashboard --issue <n>` (or `--body-file +
  --comments-json` for local mode) recomputes the canonical dashboard
  from audit evidence and edits the issue body without requiring
  caller-supplied per-role URLs.
- `plan-issue record close --issue <n> --linked-pr <ref>... --approval <url-or-text>`
  performs strict closeout: audit → strict gate → closeout comment →
  final dashboard → issue close, with provider-verified PR merge
  evidence.
- Adapter additions: `GitHubAdapter::issue_evidence`,
  `GitHubAdapter::pr_merge_summary`, and `comment_issue` now returns the
  posted comment URL.
- Agent-runtime-kit consumer handoff: see
  [`docs/specs/issue-backed-plan-record-contract-v2.md`](docs/specs/issue-backed-plan-record-contract-v2.md)
  section "Consumer Migration" for the canonical v3 commands.

### BREAKING

- `start-plan` and `start-sprint` retire the previous flat artifact
  layout under `$AGENT_HOME/out/plan-issue-delivery/<plan-slug>-...`
  and now materialize every required artifact under the canonical
  nested layout
  `$AGENT_HOME/out/plan-issue-delivery/<repo-slug>/issue-<n>/...`
  defined by
  [`agent-kit RUNTIME_LAYOUT.md`](https://github.com/sympoies/agent-kit/blob/main/skills/automation/plan-issue-delivery/references/RUNTIME_LAYOUT.md)
  and the `plan-issue-cli-canonical-runtime-artifacts` plan (retired; see repository history).
  Retired filenames:
  - `<plan-slug>-plan-tasks.tsv`
  - `<plan-slug>-plan-issue-body.md`
  - `<plan-slug>-sprint-<N>-tasks.tsv`
  - `<plan-slug>-sprint-<N>-subagent-prompts/<anchor>-subagent-prompt.md`
  Migration: the only known consumer is the `plan-issue-delivery`
  wrapper (claude-kit / codex / opencode adapters), which already
  expects the canonical layout. Direct callers should switch to the
  new `task_spec_path`, `issue_body_path`, `sprint_root`,
  `plan_snapshot_path`, `subagent_init_snapshot_path`,
  `prompt_manifest_path`, and `dispatch_record_paths` fields in the
  command JSON output instead of computing flat paths from the plan
  slug.
- `start-plan` and `start-sprint` now hard-require `AGENT_HOME` to be
  set; missing or empty `AGENT_HOME` fails fast with exit code `1` and
  the `runtime-layout-failed` error code.
- `start-sprint` writes one `<TASK_ID>.md` prompt per dispatched task
  under `$SPRINT_ROOT/prompts/`. The retired
  `<anchor>-subagent-prompt.md` flat filename is no longer produced.

### Added

- `runtime_layout` module exposing `runtime_root`, `repo_slug`,
  `IssueRoot`, `SprintRoot`, and `RuntimeLayoutError` for canonical
  path math.
- `dispatch_record` module with the ten-key `DispatchRecord`
  serializer (`task_id`, `task_prompt_path`,
  `subagent_init_snapshot_path`, `plan_snapshot_path`, `worktree`,
  `branch`, `execution_mode`, `pr_group`, `base_branch`,
  `workflow_role`) and `write_dispatch_record` helper. Optional
  adapter fields (`runtime_name`, `runtime_role`,
  `runtime_role_fallback_reason`) are intentionally absent from the
  binary's emission and are added post-emission by the wrapper /
  main-agent.
- `start-plan` JSON gains `issue_root`,
  `main_agent_init_snapshot_path`, and `plan_branch_ref_path`.
- `start-sprint` JSON gains `sprint_root`, `plan_snapshot_path`,
  `subagent_init_snapshot_path`, `prompt_manifest_path`, and
  `dispatch_record_paths`.
- Gate matrix `G11` (canonical runtime artifact emission) wired into
  the spec.
