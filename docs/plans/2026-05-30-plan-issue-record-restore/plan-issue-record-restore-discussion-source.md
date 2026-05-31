# plan-issue record restore (rehydrate bundle from issue) — Source

| Field              | Value                                                                                                                                                                                                                             |
| ------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Status             | Ready for plan generation                                                                                                                                                                                                         |
| Date               | 2026-05-30                                                                                                                                                                                                                        |
| Source             | Discussion 2026-05-30: a plan-tracking issue embeds full frozen snapshots of the bundle, but `plan-issue` has no inverse command to re-materialize them, so an unmerged bundle branch is the only place the canonical files live. |
| Intended next step | Generate a plan to add `plan-issue record restore`, making the issue a durable source-of-truth so the bundle branch / main-merge becomes a convenience, not a requirement.                                                        |

## Purpose

A plan-tracking issue already embeds the full content of all three
bundle files as frozen snapshots (the `source`, `plan`, and `state`
lifecycle comments). But `plan-issue` has no inverse command to turn
those snapshots back into the `docs/plans/<slug>/` files. Today, if the
bundle branch is pruned before it lands on the default branch, the
canonical files are gone from git and recovery means hand-copying out
of the issue comments. A `record restore` command closes that gap: the
issue becomes a true source-of-truth, so keeping the branch alive or
merging to main is a convenience rather than a durability requirement.

## Confirmed facts

- **Scope correction (verified during implementation, 2026-05-30).** Only
  the `source` and `plan` roles embed the bundle file's full content
  verbatim inside a `<details>` block. The `state` role is **not** a
  verbatim file snapshot: `record open` renders it with
  `render_record_post_comment_with_display` from structured `StateData`
  (status / scope / task ledger / prs / blockers), which drops the
  execution-state file's `# … — Execution State` H1, injects a
  `- Profile:` bullet, and wraps the task ledger in `<details open>`.
  Its hidden payload is structured `StateData` and carries **no file
  path**. Restore therefore targets `source` and `plan` only; see the
  revised Scope / Decisions below. Verified on `sympoies/nils-cli#651`
  (`crates/plan-issue-cli/src/execute.rs:474-509`,
  `templates/lifecycle_record/snapshot.md.tera`).
- `record open` / `record attach` embed the `source` and `plan` file
  content verbatim inside a `<details>` block, each followed by a trailer
  comment `<!-- plan-issue-record-payload:hex:<hex> -->`. The content is
  not HTML-escaped, so it round-trips byte-for-byte. Verified on
  `sympoies/nils-cli#651`.
- The hex payload (`plan-issue-record.payload.v2`) for `source` / `plan`
  carries metadata **only** — `path`, `commit`, `title`, `summary` —
  **not** the file bytes. Decoded from the `#651` source payload:
  `{"role":"source","data":{"path":"docs/plans/.../...-discussion-source.md","commit":"bad23a4…","title":null,"summary":null}}`.
  The file content lives only in the visible `<details>` markdown block.
- `plan-issue` exposes no restore / extract / materialize / sync
  command. `plan-issue record --help` lists only `open`, `attach`,
  `post`, `repair-dashboard`, `close`, `audit`, `template`.
- `source` and `plan` are posted once at open (and re-attachable);
  `state` is re-posted across the lifecycle via `record post`. So a
  faithful restore must take the **latest** snapshot per role.
- The issue body + comments are already retrievable through the same
  provider read path `record audit` consumes (it accepts
  `--body-file` / `--comments-json`), so restore can reuse that and run
  offline when given the JSON.

## Decisions (locked at this source doc)

1. Add `plan-issue record restore --repo <owner/repo> --issue <N>
   --out <dir>` that reconstructs the bundle's `source` and `plan`
   files from the issue's latest snapshot of each role. The `state`
   file is out of scope: it is rendered, not a verbatim snapshot (see
   Confirmed facts), and its payload carries no path.
2. Extraction source: take the file bytes from each role's visible
   `<details>` snapshot block, keyed by the canonical `path` from that
   role's hex payload. The payload supplies path + provenance; the
   `<details>` block supplies content.
3. Restore the **latest** snapshot per role (state evolves over the
   lifecycle). Record each role's snapshot `commit` from the payload as
   provenance in the output, but never require that commit to still
   exist — recovering when the commit is gone is the whole point.
4. Reuse the `record audit` read path: resolve the issue via the
   provider by default, and accept offline `--body-file` /
   `--comments-json` inputs so restore works without network.
5. Non-destructive by default: refuse to overwrite existing files
   unless `--force`. Support `--format json` returning the restored
   file paths and each role's recorded commit.
6. Scope to the two verbatim-snapshot bundle roles (`source`, `plan`).
   The `state` role is rendered, not snapshotted, so it is out of
   scope; the issue still shows its latest rendered form for humans and
   a fresh execution-state is regenerable. Session / validation /
   review / closeout comments are lifecycle records, not bundle files,
   and are out of scope.

## Scope

- The `record restore` subcommand and its snapshot parser (the inverse
  of the existing snapshot renderer).
- Round-trip tests proving `open` then `restore` reproduces the bundle.

## Non-scope

- Restoring the `state` (execution-state) file: it is rendered from
  structured payload data, not embedded verbatim, and its payload
  carries no path. A future enhancement could make it restorable by
  embedding the raw execution-state verbatim at `record open` or
  carrying path + content in the payload; out of scope here.
- Changing the snapshot format or embedding full content / a content
  hash in the hex payload (a possible separate hardening — see Risks).
- Restoring non-bundle lifecycle roles (session / validation / review /
  closeout).
- Auto-committing or auto-merging the restored files.

## Implementation boundaries

- Keep the restore parser symmetric with the existing snapshot renderer
  (same `<details>` + payload-trailer format) and co-located so a format
  change updates both; guard the symmetry with a round-trip test.
- Use the same provider read `record audit` uses, and support the same
  offline `--body-file` / `--comments-json` inputs.
- No new third-party dependency (preserve `third-party-artifacts` and
  the `Cargo.lock` locked-build gate).

## Requirements

- `record restore --repo <owner/repo> --issue <N> --out <dir>` writes
  the `source` and `plan` files at their canonical paths under the
  output directory, from the latest snapshot of each role.
- Idempotent and non-destructive: refuses to clobber existing files
  without `--force`; `--format json` lists restored paths and each
  role's recorded commit.
- Runs offline when given `--comments-json` / `--body-file`.

## Acceptance criteria

- Round-trip: `record open` a bundle, then `record restore --out <tmp>`
  reproduces the `source` and `plan` files matching the originals
  (byte-exact, modulo a documented trailing-newline normalization if
  any).
- Restoring an issue with multiple `source` snapshots yields the latest
  snapshot, not an earlier one.
- A missing required role (`source` or `plan`) produces a clear error;
  `--force` governs overwrite of existing files.
- DEVELOPMENT.md required checks plus the completion audits pass with no
  new dependency.

## Validation plan

- `cargo test -p nils-plan-issue-cli` (snapshot parser, open->restore
  round-trip for source/plan, nested-`<details>` content, latest-per-
  role selection, missing-role error, overwrite / `--force`).
- Manual: `record restore --comments-json <#651 export> --out <tmp>`
  and `diff` the restored `source` / `plan` against the committed
  bundle — expect a byte-exact match (verified 2026-05-30).
- Full required checks (`nils-cli-checks-entrypoint.sh --local-fast`)
  and the completion audits before PR.

## Findings

| Priority | Issue | Evidence | Fix location | Acceptance |
| --- | --- | --- | --- | --- |
| HIGH | No inverse of `record open`: bundle snapshots embedded in the issue cannot be re-materialized into files, so an unmerged/pruned branch loses the canonical bundle | `plan-issue record --help` (open/attach/post/repair-dashboard/close/audit/template — no restore); `#650` payload carries only path+commit, content only in `<details>` | new `record restore` subcommand in `plan-issue-cli` + the snapshot parser | round-trip `open`->`restore` reproduces the bundle |

## Risks and guardrails

- Snapshot drift: extraction reads the visible `<details>` block, so a
  hand-edited snapshot would restore the edited text. Mitigation: treat
  snapshots as frozen; a future hardening could embed a
  `content_sha256` in the payload for integrity verification (noted
  non-scope).
- Renderer/parser divergence: a format change to `record open` could
  silently break restore. Mitigation: keep them symmetric and pin an
  `open`->`restore` round-trip test.
- Latest-vs-initial state: restoring the wrong snapshot would resurrect
  stale state. Mitigation: explicit latest-per-role selection plus a
  test that mutates state then restores.

## Execution

- Recommended plan: docs/plans/plan-issue-record-restore/plan-issue-record-restore-plan.md
- Recommended execution state: docs/plans/plan-issue-record-restore/plan-issue-record-restore-execution-state.md
- Status: ready for plan generation; to be tracked by a plan-tracking issue.
- Next-task source: this document.

## Retention intent

- Plan-scoped. Clean up `docs/plans/plan-issue-record-restore/` after
  execution lands and the PR merges, unless promoted into a plan-issue
  runbook.

## Read-first references

- The `plan-issue` snapshot renderer module (the `<details>` +
  `plan-issue-record-payload:hex` format `record open` emits).
- The `record audit` provider read path (`--body-file` /
  `--comments-json`) — restore reuses it.
- `plan-issue record` subcommand wiring (where `restore` is added).
- `sympoies/nils-cli#650` — a live tracking issue usable as a restore
  fixture.

## Recommended next artifact

- A plan (`*-plan.md`) sequencing: snapshot parser -> `record restore`
  subcommand -> round-trip + edge tests -> required checks, tracked via
  `create-plan-tracking-issue`.
