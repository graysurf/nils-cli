---
name: nils-cli-prune-stale-code
description: Find and remove outdated/unused code in the nils-cli workspace (unused dependencies and stale dead-code suppressions) using compiler-verified deletions, then deliver through the GitHub PR workflow.
---

# Nils CLI Prune Stale Code

Use this skill when asked to clean up outdated, dead, or unused code across the
`nils-cli` workspace. It targets the cleanup surface that the workspace's
existing gates do **not** already eliminate, and it treats the **compiler as the
authority** for every deletion: nothing is removed on a static scanner's word
alone.

Read this first — it changes the strategy: the workspace already runs
`cargo clippy --all-targets --all-features -- -D warnings` (so genuinely dead
*private* code cannot survive on `main`) and `scripts/ci/test-stale-audit.sh`
(orphaned test helpers and deprecated-path leftovers, governed by
`docs/runbooks/test-cleanup-governance.md`). Do not re-hunt those. The genuine
remaining surface is:

1. **Unused direct dependencies** — clippy does not flag these.
2. **Stale dead-code escape hatches** — `#[allow(dead_code)]` /
   `#[allow(unused*)]` sites whose code is now either used (suppression is
   stale) or genuinely dead (item is removable).
3. Dead `pub` items reachable only from now-removed call paths, and obvious
   leftover consts/breadcrumbs.

Expect a **modest** result on a well-maintained repo. A small, fully-verified
change set is the success condition — report the small surface as a finding;
never manufacture risky deletions to inflate the diff.

## Contract

Prereqs:

- Run inside the `nils-cli` git work tree.
- Follow `DEVELOPMENT.md` for Rust tooling. Required: `cargo`, `cargo-nextest`,
  `rg`, `python3`, `zsh`. Optional but recommended: `cargo-machete`
  (`cargo install --locked cargo-machete`) for the unused-dependency scan.
- Start from a clean working tree on a fresh branch off `main`
  (`chore/<slug>` or `refactor/<slug>`).

Inputs:

- Optional scope hints: a crate, dependency, module, or "deps only" /
  "dead-code only".
- Optional constraints: areas that must stay untouched, draft-vs-ready PR.

Outputs:

- A candidate report from `scripts/nils-cli-prune-stale-code.sh` (read-only).
- A behavior-preserving change set: removed unused dependencies and/or stale
  dead-code suppressions and the dead items they were hiding.
- Regenerated `Cargo.lock`, `THIRD_PARTY_LICENSES.md`, and
  `THIRD_PARTY_NOTICES.md` whenever dependencies changed.
- Full local CI-parity evidence, then a PR via `semantic-commit` +
  `forge-cli pr create` / `$deliver-github-pr`.
- An honest summary of what was kept and why (intentional API, forward-compat,
  test-exercised, serde capture fields).

Exit codes (helper script):

- `0`: report produced
- `2`: usage error, missing root, or `--machete` forced without prerequisites

Failure modes:

- Removing a dependency that the source imports under a **different lib name**
  than its package name (e.g. package `nils-agent-out` imported as
  `agent_out`). `cargo-machete` and naive grep both miss this; only the
  compiler catches it. Always rebuild and revert false positives.
- Removing a `serde` field from a struct marked `#[serde(deny_unknown_fields)]`
  — this changes parse behavior (previously-valid input is now rejected). Treat
  such fields as `keep`.
- Deleting code that is intentionally kept: re-exported API families,
  documented forward-compat fields, methods exercised only by integration tests
  (separate crates, so they look dead to the lib), or platform `cfg_attr`
  allows.
- Changing dependencies without regenerating `Cargo.lock` and the THIRD_PARTY
  artifacts, which fails CI's `third-party-artifacts-audit` and locked-build
  gates.

## Scripts

- `.agents/skills/nils-cli-prune-stale-code/scripts/nils-cli-prune-stale-code.sh`

Read-only candidate scanner. Prints unused-dependency candidates (via
`cargo-machete` when available), `#[allow(dead_code)]` / `#[allow(unused*)]`
sites split into production vs test buckets, and `serde(deny_unknown_fields)`
structs (the field-removal trap). It never edits files or runs tests.

```bash
.agents/skills/nils-cli-prune-stale-code/scripts/nils-cli-prune-stale-code.sh
```

Every entry is a **candidate only** — the workflow below adjudicates each with
the compiler.

## Workflow

1. **Branch and survey.** Branch off `main`, then run the helper to get the
   candidate report. Cross-check that you are not duplicating the test-stale
   audit's domain (`bash scripts/ci/test-stale-audit.sh --strict` already owns
   orphaned test helpers).

2. **Unused dependencies — verify, then let the compiler confirm.**
   - For each `cargo-machete` hit, grep the owning crate for the dependency's
     usage. Remember a dep may be imported under its **lib name**, a `package =`
     rename, a derive macro, or a re-export (e.g. `usvg` reached via
     `resvg::usvg::`). When the lib name differs from the package name, the
     scanner's verdict is unreliable.
   - Remove the dependency line from the crate's `Cargo.toml`, then run
     `cargo clippy --all-targets --all-features -- -D warnings`. Compilation is
     the authority: a clean build confirms the removal; an `unresolved import`
     / `unlinked crate` error means it was a false positive — restore that one
     line and move on.

3. **Dead-code suppressions — remove the hatch, let clippy adjudicate.** For
   each production `#[allow(dead_code)]` / `#[allow(unused*)]` site, remove the
   attribute and rebuild under `-D warnings`:
   - **Error "never used"** → the item is genuinely dead → delete the item
     (and the attribute).
   - **Clean build** → the suppression was stale (the code is actually used,
     sometimes only cross-module or internally) → keep the code, drop only the
     now-unnecessary attribute.
   - **Keep (do not touch the attribute or the code)** when the suppression is
     correct:
     - members of a deliberately stable API family (e.g. re-exported exit-code
       constants or an error-constructor set kept for symmetry);
     - fields with a comment documenting forward-compat / pass-through intent;
     - items used only by integration tests (separate crates, so the lib's own
       dead-code analysis flags them — the allow is required);
     - `serde` capture fields, especially under `deny_unknown_fields`;
     - platform-conditional `cfg_attr(..., allow(dead_code))`.

4. **Regenerate dependency artifacts** whenever a dependency changed:

   ```bash
   cargo build --workspace            # refresh Cargo.lock
   bash scripts/generate-third-party-artifacts.sh --write
   ```

5. **Validate with full CI parity.** Because dependency edits touch the
   workspace graph, run the full gate, not just `--local-fast`:

   ```bash
   NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh
   ```

   Confirm `third-party-artifacts-audit --strict` reports `drift=0` and the run
   ends with `ok: all nils-cli checks passed`.

6. **Deliver through PR.**
   - Confirm the working tree contains only the intended cleanup.
   - Commit with `semantic-commit` (`--type chore` or `--type refactor`); do not
     run `git commit` directly.
   - Render the body with `agent-runtime pr-body render` and create the PR with
     `forge-cli pr create` (or `$deliver-github-pr`). Branch prefix must match
     `--kind` (`chore/` → `--kind chore`, `refactor/` → `--kind refactor`).
     Apply one `type::`, one `area::`, and one `size::` label; add `risk::low`
     for a verified behavior-preserving cleanup. Default to a draft PR unless
     the user asked for ready-for-review; do not auto-merge without explicit
     instruction.
   - Put the candidate report, kept-vs-removed rationale, and the
     `nils-cli-checks-entrypoint.sh` result in `## Test plan`.
   - If nothing was safely removable, do not open a PR — report the surveyed
     candidates and why each was kept.

## Boundary

Behavior-preserving cleanup only. This skill does not change features, lower the
coverage gate or any audit threshold, edit `scripts/ci/test-stale-audit-baseline.tsv`
(that lives under `docs/runbooks/test-cleanup-governance.md` and the
`nils-cli-maintain-test-coverage` skill), or remove code that is intentionally
kept. It is a project-local skill: it must not mutate runtime-kit manifests,
rendered product output, global runtime homes, credentials, or cache state. If
candidate detection should become a stable repository command, extract that
deterministic behavior into released `nils-cli` tooling first and keep this
skill as the orchestration layer.
