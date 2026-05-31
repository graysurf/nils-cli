# repo-retro Report v2 — Signal-vs-Noise Pre-Digestion — Implementation Handoff

- Status: decisions settled; graduated to an L2 tracked plan bundle.
- Date: 2026-05-31
- Source: a design review of `repo-retro report` output run against a
  doc-heavy workflow repo (`agent-runtime-kit`, last 3 days). The derived
  analysis layer ranks by raw line churn over an undifferentiated path space,
  so plan/discussion authoring and mass plan archival dominate the "insight"
  and crowd out real source movement. The run even nominated an archived
  (net-deleted) plan file for "focused review".
- Intended next step: execute the sibling plan under this bundle. This is a
  source artifact, not an implementation plan.

## Execution

- Recommended plan: docs/plans/repo-retro-report-v2-predigest/repo-retro-report-v2-predigest-plan.md
- Recommended execution state: docs/plans/repo-retro-report-v2-predigest/repo-retro-report-v2-predigest-execution-state.md
- Status: decisions settled; plan execution is the next step.
- Next-task source: this document

## Purpose

Make `repo-retro`'s derived insight trustworthy in repos whose workflow
produces large volumes of process documentation (plans, captured discussions)
and periodic mass archival. The fix is a deterministic **pre-digestion layer**
that classifies every changed path, ranks file hotspots by how often a file
was touched rather than by line count, and surfaces archival as a labelled
fact — then rewires the soft-narrative layer to read that pre-digestion
instead of raw line churn. The raw counts are already correct; only the
derived layer overreaches.

## Confirmed Facts (current crate behaviour)

- [F1] Hotspots are ranked by raw changed lines.
  `crates/agent-workflow-primitives/src/repo_retro.rs:992-999` sorts
  `top_files` by `changed_lines` (desc) and truncates to 10. A plan written
  once (one large commit) outranks a source file iterated across five commits.
- [F2] Areas collapse to the top-level path segment with no class distinction.
  `top_level_area` (`repo_retro.rs:1095-1097`) returns `path.split('/').next()`,
  so `docs/plans/**`, `docs/discussions/**`, `docs/specs/**`, and
  `docs/runbooks/**` all fold into a single `docs` bucket; `top_areas` is then
  ranked by `changed_lines` (`repo_retro.rs:1001-1017`). In a doc-heavy repo
  `docs` always wins the headline regardless of where real work happened.
- [F3] The "theme" line is mechanically derived from that ranking.
  `repo_retro.rs:1747-1752` emits "`<area>` carried the largest code/doc
  movement with N changed line(s)." off `top_areas.first()`. Observed output:
  "`docs` carried the largest code/doc movement with 8216 changed line(s)" —
  true but content-free when `docs` is process churn.
- [F4] The follow-up question nominates the line-churn hottest file with **no
  net-deletion guard**. `repo_retro.rs:1808-1813` asks "Does `<top_files[0]>`
  need focused review because it was the hottest file?" The observed nominee
  was a plan with `0` insertions / `751` deletions — i.e. archived and
  deleted, not reviewable. The follow-up is actively misleading.
- [F5] The raw fact layer is sound. `CommitSummary`, `commit_types`,
  `authors`, and `test_signals` (`repo_retro.rs:301-360, 1019-1062`) are
  honest deterministic counts. The defect is isolated to the derived
  hotspot / area / analysis layer.
- [F6] Per-file commit count and signed insert/delete are already collected.
  `FileChangeSummary` (`repo_retro.rs:324-330`) already carries `commits`,
  `insertions`, `deletions`, `changed_lines`. Commit-frequency ranking and a
  `netDeleted` flag are therefore sort-key / derived-field changes, **not new
  data collection**. Path classification is the only genuinely new input.
- [F7] Schema identifiers are versioned constants.
  `REPORT_ENVELOPE_SCHEMA_VERSION = "cli.repo-retro.report.v1"` and
  `REPORT_SCHEMA_VERSION = "repo-retro.report.v1"` (`repo_retro.rs:52-53`).

## Conceptual Model (three layers)

- L1 — raw facts: counts and numstat. Deterministic. Already shipped, keep.
- L2 — structured pre-digestion: path-class churn split, archival facts,
  commit-frequency hotspots with a `netDeleted` flag. Deterministic. Missing
  today — this is the value gap.
- L3 — soft narrative: `themes`, `attentionItems`, `followUpQuestions`.
  Currently jumps from L1 straight to a line-churn-based L3 and gets it wrong.

The cut is between deterministic (L1 + L2 → CLI owns) and genuinely subjective
(L3 narrative → an agent consumer may override). L2 is the deterministic
pre-digestion an agent should not have to hand-roll on every run.

## Decisions

- [D1] Add a deterministic **L2 pre-digestion** layer to `repo-retro report`.
- [D2] Path-class taxonomy is **configurable / overridable**. Ship sensible
  built-in defaults; allow a repo-local config (glob → class) merged over the
  defaults, plus an explicit config-path flag. Built-in defaults must degrade
  gracefully in a repo with no plan/discussion convention (those classes are
  simply empty, never a misclassification).
- [D3] **Bump the report schema to v2 and break backward compatibility** —
  `cli.repo-retro.report.v2` / `repo-retro.report.v2`. Do the best design; do
  not dual-emit or shim v1. All consumers update in lockstep (see
  Cross-Repo Coordination).
- [D4] **Keep L3 as a noise-aware convenience layer that reads L2.** Do not
  delete the narrative layer (non-agent CLI users want a readable summary);
  rebuild it to consume `churnByClass` / `archival` / commit-frequency
  hotspots so it can no longer emit content-free or net-deleted nominations.
- [D5] **Refresh agent-runtime-kit consumers** in lockstep: the `meta:repo-retro`
  skill contract and the `reporting:project-retro` skill, which read this
  envelope.
- [D6] Rank `fileHotspots.topFiles` by **commit-touch count** (desc), with
  `changedLines` as the secondary key; each entry gains `class` and
  `netDeleted`.
- [D7] Net-deletion is the **primary archival signal** (`insertions == 0 &&
  deletions > 0`); any commit-scope heuristic (e.g. `chore(plans): archive`)
  is a secondary label only.

## Proposed schema v2 shape

```text
data.schema           = "repo-retro.report.v2"      # was .v1
schema_version        = "cli.repo-retro.report.v2"  # was .v1

data.git.churnByClass = {                  # NEW — the headline split
  <class>: { fileCount, commits, insertions, deletions, changedLines }
}                                          # classes: source | tests
                                           #   | productDocs | processArtifacts
                                           #   | other; class sums reconcile to
                                           #   summary.changedLines

data.git.fileHotspots.topFiles[]           # CHANGED
  += class            (string)             #   path's resolved class
  += netDeleted       (bool)               #   insertions == 0 && deletions > 0
  ranked by commits desc, then changedLines desc   # was changedLines only

data.git.fileHotspots.topAreas[]           # CHANGED
  += class            (string)             #   dominant class for the area

data.git.archival = {                      # NEW — archival as a labelled fact
  netDeletedFileCount,
  netDeletedFiles[]   ({ path, deletions, class }),
  processArtifactsDeletedLines,
  plansArchivedEstimate                    # commit-scope heuristic, secondary
}

data.analysis.*                            # CHANGED — reads L2, not raw churn
  themes              : lead with source/tests churn from churnByClass;
                        report process-doc churn separately and flag when it is
                        mostly archival; drop the bare "<area> had most lines".
  followUpQuestions   : nominate the top non-netDeleted iteration hotspot;
                        never nominate a netDeleted file; emit an archival
                        summary line instead.
```

Path-class default heuristics (overridable per [D2]):

- `tests` — reuse existing `is_test_path` (`repo_retro.rs:1085-1093`).
- `processArtifacts` — `docs/plans/**`, `docs/discussions/**`, and
  heuristic-system inbox / operation-record paths.
- `productDocs` — `README*`, `DEVELOPMENT*`, `docs/specs/**`,
  `docs/runbooks/**`, and other durable `*.md`.
- `source` — everything else (code, scripts, manifests, hooks, targets).
- `other` — fallback for anything a config explicitly carves out.

## Scope

- `repo-retro report` JSON and Markdown output for the v2 schema.
- New L2 fields (`churnByClass`, `archival`), changed `topFiles` ranking and
  `topFiles` / `topAreas` shape, and the L2-aware rebuild of `analysis.*`.
- Path-class classifier with built-in defaults and a repo-local override.
- Unit-test coverage and the JSON-contract / completion updates the change
  requires.
- Lockstep refresh of agent-runtime-kit consumers and the surface pin.

## Non-scope

- Remote API enrichment, new evidence inputs, or history-comparison changes.
- New top-level subcommands beyond `report`.
- Any v1 compatibility shim or dual-emit (explicitly rejected by [D3]).
- Re-litigating the raw L1 fact layer ([F5]); it stays as is.

## Implementation boundaries

- Confine logic to `crates/agent-workflow-primitives/src/repo_retro.rs` and its
  binary entrypoint; do not spread report logic into other crates.
- Classification and net-deletion are deterministic and judgement-free; the CLI
  must not infer subjective conclusions inside L2.
- Markdown output must stay a faithful render of the same envelope.

## Risks and guardrails

- RK1: v2 breaks every consumer until updated ([D3]). Guardrail: land nils-cli
  v2, the agent-runtime-kit consumer refresh, and the surface pin bump as one
  coordinated change; the EXACT-match version-pin gate in agent-runtime-kit
  blocks pushes until the pin matches, forcing lockstep.
- RK2: Default path classes are shaped by agent-runtime-kit conventions.
  Guardrail: defaults key off conventional paths; their absence yields an empty
  class, never a misclassification ([D2]).
- RK3: The commit-scope archival heuristic is fuzzy. Guardrail: net-deletion is
  primary, scope is a secondary label only ([D7]).
- RK4: `productDocs` vs `source` for `docs/specs` / `docs/runbooks` is a
  judgement call. Guardrail: it is configurable ([D2]) so a repo can retune.

## Cross-repo coordination

- repo-retro ships as the `repo-retro` binary in
  `crates/agent-workflow-primitives`; schema v2 lands in nils-cli.
- agent-runtime-kit consumers to refresh in lockstep ([D5]): the
  `meta:repo-retro` skill contract and the `reporting:project-retro` skill,
  both of which read this envelope.
- Surface pin: agent-runtime-kit pins the nils-cli surface with an EXACT-match
  gate; shipping a release that carries v2 requires a coordinated
  `meta:nils-cli-bump` plus the consumer refresh in the same change.

## Read-first references

- `crates/agent-workflow-primitives/src/repo_retro.rs` — report builder; schema
  consts `52-53`; `FileChangeSummary` `324-330`; hotspot/area ranking
  `992-1017`; `top_level_area` `1095-1097`; analysis builder `~1729-1845`.
- `docs/specs/crate-docs-placement-policy.md` — placement / lifecycle rules.
- `docs/specs/cli-output-contract-v1.md` — JSON contract conventions for the v2
  bump.
- agent-runtime-kit: `meta:repo-retro` and `reporting:project-retro` skills.

## Retention Intent

Plan-source coordination document. Cleanup-eligible after the v2 surface ships,
consumers are refreshed, and the tracker closes and archives.
