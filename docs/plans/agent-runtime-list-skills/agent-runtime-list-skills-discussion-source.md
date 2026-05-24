# Discussion Source: agent-runtime list-skills Subcommand

## Purpose

Provide a stable, machine-readable enumeration of the skills `agent-runtime
install` would activate for a given product, source root, and live home.
Today, the `agent-runtime-kit` sandbox install rehearsal CI gate
(`scripts/ci/sandbox-install-rehearsal.sh`) recovers this information by
regex-parsing `agent-runtime install --dry-run` text output, because the
product CLIs (Codex / Claude) never shipped the originally specced
`<product-cli> --home <dir> --list-skills` contract. The rehearsal script
declares this fallback explicitly in its header comment.

This source documents the decision to move the contract onto `agent-runtime`
itself rather than waiting for the product CLIs, so the rehearsal and any
future consumers can read a structured surface.

## Problem Statement

- The rehearsal script depends on dry-run text parsing that diverges per
  product (codex vs claude regex branches), is brittle to install plan
  formatting changes, and cannot easily carry richer per-skill metadata such
  as link-mode or expected-discoverability.
- `agent-runtime-cli` already computes structured skill data twice during
  normal flows: once through `install::plan::InstallPlan` while building the
  apply plan, and once through `doctor::skill_surface::SkillSurfaceReport`
  while classifying the active surface for the `doctor` subcommand. Neither
  is exposed as a top-level subcommand.
- Resolved Decision #8 in
  `agent-runtime-kit/docs/source/inventory-target-architecture.md` pinned
  the rehearsal contract to a product-CLI flag that has not materialised, so
  the gate has been working around the gap rather than completing it.

## Proposal

Add `agent-runtime list-skills` as a read-only subcommand parallel to
`render`, `install`, `doctor`, and `audit-drift`. It computes the skill list
from the source-root + product + live-home triple by reusing the existing
link-map + install-plan + skill-surface modules. No filesystem mutation.

### Public Surface

- Subcommand: `agent-runtime list-skills`.
- Required flags:
  - `--source-root <path>`: absolute agent-runtime-kit checkout root.
  - `--product <claude|codex>`: same product domain as `install` /
    `audit-drift`.
- Optional flags:
  - `--live-home <path>`: absolute live-home dir, accepted for parity with
    `install`; not required for enumeration today but reserved for future
    filesystem-validating checks.
  - `--format text|json` (default `text`).
  - `--include-warnings`: surface `skill_surface` warnings (e.g. file-symlink
    SKILL.md leaves) inline in JSON / text output.

### JSON v1 Schema

```json
{
  "product": "codex",
  "source_root": "/abs/path/agent-runtime-kit",
  "live_home": null,
  "skills": [
    {
      "id": "reporting.daily-brief",
      "source": "build/codex/plugins/reporting/skills/daily-brief",
      "destination": "skills/reporting/daily-brief",
      "link_mode": "directory",
      "discoverable": true,
      "warnings": []
    }
  ]
}
```

- `id` is the canonical `<domain>.<skill>` slug currently used by the
  sandbox rehearsal expected-skills pin files.
- `link_mode` mirrors `SymlinkLinkMode` values: `file`, `directory`,
  `recursive-file`.
- `discoverable` reflects `doctor::skill_surface::CodexDiscoverability`,
  collapsed to a boolean for non-codex products that don't carry the
  classification.
- The output is sorted by `id` for deterministic diffs.

### Text Output

Single line per skill: `<id>\t<link_mode>\t<destination>`. Designed to be
piped through `cut -f1 | sort > observed.txt` and diffed against the pinned
`expected-skills.txt`.

## Out Of Scope

- Live-home introspection (reading what skills the product CLI actually
  loaded). The contract enumerates the install-plan view, which is what the
  rehearsal cares about. Real product-loaded enumeration remains a product-CLI
  responsibility if it ever ships.
- Mutating `--live-home`. The subcommand is read-only.
- Cross-product enumeration (`--product all`). Out of scope for v1.

## Resolved Decisions

1. **Owner is `agent-runtime`, not the product CLI.** Product CLIs never
   shipped `--home <dir> --list-skills`; collecting the data inside
   `agent-runtime` removes the cross-tool dependency and avoids changing
   either product CLI.
2. **JSON v1 schema is stable on the listed field names.** Field additions
   are non-breaking; field removals or renames are breaking and must bump
   the schema marker.
3. **Output is deterministic and sort-stable.** Sorted by skill `id`, no
   wall-clock or hash-randomized iteration in either format. This satisfies
   `agent-runtime` Resolved Decision #9.
4. **Skill discovery reuses the existing modules.** `LinkMap::load` →
   `InstallPlan::build` is the canonical computation path; only the
   formatting layer is new.

## Open Questions

None carried into execution. The cross-repo `agent-runtime-kit` rehearsal
swap is intentionally scheduled after a nils-cli release that contains
`list-skills`, because the rehearsal script can only adopt the new
subcommand once it is installable.

## References

- Existing install plan model:
  `crates/agent-runtime-cli/src/install/plan.rs`
- Existing skill-surface classifier:
  `crates/agent-runtime-cli/src/doctor/skill_surface.rs`
- Existing rehearsal regex fallback:
  `agent-runtime-kit/scripts/ci/sandbox-install-rehearsal.sh`
- Resolved Decision #8 (sandbox install rehearsal):
  `agent-runtime-kit/docs/source/inventory-target-architecture.md`

## Execution

- Recommended plan: docs/plans/agent-runtime-list-skills/agent-runtime-list-skills-plan.md
- Recommended execution state: docs/plans/agent-runtime-list-skills/agent-runtime-list-skills-execution-state.md
