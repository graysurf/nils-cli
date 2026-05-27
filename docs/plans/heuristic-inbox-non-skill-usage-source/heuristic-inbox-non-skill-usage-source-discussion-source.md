# heuristic-inbox `new` non-skill-usage source mode — Source

| Field              | Value                                                                                                                       |
| ------------------ | --------------------------------------------------------------------------------------------------------------------------- |
| Status             | Ready for implementation                                                                                                    |
| Date               | 2026-05-28                                                                                                                  |
| Source             | sympoies/nils-cli#585: `heuristic-inbox new` only accepts `--from-skill-usage`, so non-skill-usage findings cannot scaffold |
| Intended next step | Implement the `--from-evidence` / `--manual` source modes in a single small PR that closes #585                             |

## Purpose

`heuristic-inbox new` requires `--from-skill-usage <PATH>`, so it can only
scaffold a curated error-inbox entry from a `skill-usage.record.v1` envelope.
Findings diagnosed mid-session that do not originate from a single named-skill
invocation have no scaffolding path: they must be hand-authored, which then
trips the non-obvious `verify --strict` `missing raw evidence pointer` check
(`crates/agent-workflow-primitives/src/heuristic_inbox.rs` `verify_case`, the
`raw_records.is_empty()` branch).

## Why this is a gap

The Heuristic System Promotion Ladder treats "important unresolved workflow
gap -> curated error-inbox entry" **without** requiring a skill-usage envelope;
skill-usage is only the preferred path when friction happens inside an active
named-skill workflow. So the CLI requirement is narrower than the policy.
`ingest-evidence` already offers a non-skill-usage evidence path, so decoupling
`new` from skill-usage is consistent with the existing design.

## Concrete instance

2026-05-27: a worktree-signing root-cause case was diagnosed live with no
skill-usage record. `new` could not be used; the entry was hand-written and
first failed `verify --strict` with `missing raw evidence pointer` until a
`Raw record:` line was added by hand.

## Confirmed facts

- `NewArgs.from_skill_usage` is a required `PathBuf`; there is no other source
  flag on `new`.
- `redact_ingest_source` (used by `ingest-evidence`) already performs
  home-path normalization, raw-skill-usage rejection, size, binary, and
  secret-pattern checks and returns redacted text plus violations.
- `verify_case` requires, for an inbox `ENTRY.md`: the seven required sections,
  non-empty `Status` fields (status / first observed / area / severity), a
  valid status and severity, and at least one `- Raw record:` pointer line.

## Decision (locked at this source doc)

Implement the issue's option 3 framing — all three sources, mutually exclusive:

1. `--from-evidence <PATH>`: scaffold from an arbitrary, already-redacted
   evidence file, reusing `ingest-evidence` redaction; copy the redacted file
   under the case `evidence/` directory and point `Raw record:` at it.
2. `--manual`: scaffold a skeleton, auto-filling
   `Raw record: not captured (manual diagnosis, <date>)` so `verify --strict`
   passes while recording the absence of evidence.
3. Exactly one of `--from-skill-usage | --from-evidence | --manual` is
   required (clap `ArgGroup`). Skill-usage stays the documented preferred path.

## Out of scope

- The heuristic-inbox SKILL / `HEURISTIC_SYSTEM.md` doc update describing the
  non-skill-usage path — those docs live in `agent-runtime-kit`, tracked as a
  runtime-kit follow-up after this CLI change lands (matching the established
  CLI-then-runtime split).
- Any change to `ingest-evidence`, `verify`, or the redaction rules themselves.

## Execution

- Recommended plan: docs/plans/heuristic-inbox-non-skill-usage-source/heuristic-inbox-non-skill-usage-source-plan.md
- Recommended execution state: docs/plans/heuristic-inbox-non-skill-usage-source/heuristic-inbox-non-skill-usage-source-execution-state.md
