# Plan: nils-cli-bump-version-tag-release tap integration

## Overview

Today's `nils-cli` release flow stops at `git push origin v<version>` in the
`nils-cli` repo. Everything that has to happen in `sympoies/homebrew-tap` —
formula URL/sha256 bump, semantic commit, prefix tag (`nils-cli-v<version>`)
to trigger the tap-side `release.yml`, and post-publish brew smoke — is
manual. Two recent incidents exposed the gap:

- v0.7.4 (2026-04-25): formula was hand-bumped and pushed to `main`, but no
  `nils-cli-v0.7.4` prefix tag was created. Tap `release.yml` therefore never
  ran, and no tap-side GitHub Release was published. Still missing today.
- v0.7.6 (2026-04-26): same omission. The prefix tag was added retroactively
  in this session; tap `release.yml` ran (24955264588) only after manual
  catch-up.

Both incidents traced to the skill's scope: it owns `nils-cli` repo state but
is silent about the tap. This plan enumerates the concrete gaps and sketches
how to fold the tap stage into the same skill (or split it into a sibling
skill) so future bumps go through one entry point and never need ad-hoc
recovery.

## Current skill scope (`.agents/skills/nils-cli-bump-version-tag-release/`)

The skill ends after these eight steps inside the `nils-cli` work tree:

1. Validate inputs, prereqs, RUSTC_WRAPPER, dirty tree, branch.
2. Bump workspace + crate `Cargo.toml` versions and path-dep pins.
3. Update README release tag examples.
4. Verify via CI gate on `main` (preferred) or full release checks fallback.
5. Refresh `Cargo.lock` and regenerate `THIRD_PARTY_LICENSES.md` /
   `THIRD_PARTY_NOTICES.md`.
6. `semantic-commit` the bump.
7. Annotate tag `v<version>` and push.
8. Trust the upstream `release.yml` to build platform tarballs asynchronously.

Everything beyond step 8 — i.e. anything that touches the tap — is operator
memory.

## Concrete gaps (post-step-8)

| # | Gap | Evidence | Impact |
| --- | --- | --- | --- |
| 1 | No wait for `nils-cli` `release.yml` artifacts to publish | Manual `gh run view` polling in this session | Tap step depends on artifacts but skill can't sequence it |
| 2 | No fetch of artifact sha256 | Manual `curl https://github.com/graysurf/nils-cli/releases/download/v<ver>/...sha256` × 4 | Operator copies hex into formula by hand |
| 3 | No formula edit | Manual Edit on `Formula/nils-cli.rb` URL + sha256 lines | Drift risk, copy-paste errors |
| 4 | No `ruby -c` / `brew style` validation | DEVELOPMENT.md mandates these but skill never runs them | Bad formula can be committed |
| 5 | No tap-side `semantic-commit` | Hand-typed `chore(formula): bump nils-cli to v<ver>` | Inconsistent message body across releases |
| 6 | No tap-side prefix tag (`nils-cli-v<version>`) | Missing for v0.7.4 (still missing) and v0.7.6 (added manually today) | Tap `release.yml` never fires → no brew-test, no tap GitHub Release |
| 7 | No tap location discovery | `~/Project/sympoies/homebrew-tap` is implicit in operator's head | Skill cannot run on a fresh machine without manual path |
| 8 | No tap dirty-tree / drift guard | `git pull` clobbered by stale `v0.3.5` tag in this session | Opaque failure; operator must improvise `fetch --no-tags` |
| 9 | No idempotent resume | If sha256 fetch fails (artifacts not built yet) operator restarts from scratch | Slow recovery, easy to skip steps |
| 10 | No post-publish `brew test` smoke | DEVELOPMENT.md recommends `brew reinstall && brew test` | Unverified that the bumped formula installs end-to-end |
| 11 | No multi-formula awareness | Tap also ships `agent-workspace-launcher`; current pattern can't be reused without parameterizing the formula name | Diverging ad-hoc flows for AWL bumps |
| 12 | Hardcoded artifact origin | Formula URLs point at `graysurf/nils-cli` (not `sympoies/nils-cli`); skill has no way to validate this matches what release.yml uploads | Future repo move would silently break the bump |

## Proposed scope expansion

### Option A — extend the existing skill (recommended)

Add a tap stage to `nils-cli-bump-version-tag-release.sh` that runs after the
`nils-cli` tag push, gated by a flag (default-on with `--skip-tap` to opt
out). Steps:

1. Resolve the tap work tree:
   - `--tap-dir <path>` flag, or
   - `NILS_CLI_HOMEBREW_TAP_DIR` env, or
   - convention `<nils-cli repo parent>/homebrew-tap`.
   - Hard-fail with a clear message if none resolves to a git work tree.
2. `git fetch --no-tags origin main` + `git merge --ff-only origin/main`
   (avoid the `v0.3.5` clobber pattern).
3. Wait for `nils-cli` `release.yml` run on the just-pushed tag to reach
   `completed success` — reuse the same `gh run list --workflow release.yml`
   query the operator did manually.
4. For each `(arch, os)` matrix entry, fetch the published `.tar.gz.sha256`
   sidecar and parse the hex. Source repo is whatever the existing formula
   already points at (parse from `Formula/nils-cli.rb`); do not hardcode.
5. Edit `Formula/nils-cli.rb` URL + sha256 lines via the same Python in-place
   editor used for `Cargo.toml`; refuse to overwrite if anything other than
   URL/sha256 lines would be touched.
6. Run `ruby -c Formula/nils-cli.rb` + `HOMEBREW_NO_AUTO_UPDATE=1 brew style
   Formula/nils-cli.rb`.
7. `semantic-commit` with the canonical body
   `chore(formula): bump nils-cli to v<version>` + bullet about URL/sha256.
8. Push the tap `main` commit.
9. Annotate + push prefix tag `nils-cli-v<version>` to trigger the tap
   `release.yml` (this is the step that's been missed today).
10. Optionally wait for the tap `release.yml` run to reach `completed
    success` (gated by `--wait-tap-release`, default-on for releases, off
    for `--skip-tap-wait`).

Also add a resume entrypoint:
`--from-tap`: skip steps 1-8 of the existing skill and run only the tap
stage, given an existing `v<version>` tag in `nils-cli`. Lets the operator
recover from a half-finished release without re-bumping versions.

### Option B — split into a sibling skill

`nils-cli-bump-version-tag-release` stays narrow; add a new
`nils-cli-bump-homebrew-formula` skill that consumes a `--version` and
performs steps 1-10 above. The two are chained by a thin wrapper (or
`/release` slash command) so user-facing UX stays "one command." Trade-off:
cleaner single-responsibility, more wiring; slightly higher maintenance for
the wrapper / `/release` dispatcher.

Either option closes gaps 1-10. Gap 11 (AWL) is addressed by parameterizing
the formula name in whichever option is taken — Option B does this naturally
since the new skill is formula-scoped.

## Phased implementation outline

1. **Sprint 1 — read-only scaffolding.** Add tap discovery, sha256 fetcher,
   and `release.yml` waiter behind a `--dry-run-tap` flag. Skill prints what
   it would commit without touching the tap. Lands as a no-op for current
   users.
2. **Sprint 2 — formula edit + commit.** Implement in-place URL/sha256 edit
   with the Python editor; add `ruby -c` / `brew style` gates;
   `semantic-commit` the bump; push `main`. Still no tag yet — operators can
   verify the diff before opting in to step 3.
3. **Sprint 3 — prefix tag + release wait.** Push `nils-cli-v<version>`,
   optionally wait for tap `release.yml`. After this sprint, today's manual
   pain is fully automated.
4. **Sprint 4 — `--from-tap` recovery + multi-formula parameterization.**
   Resume mode for half-finished releases; parameterize formula name so the
   same machinery covers `agent-workspace-launcher`.
5. **Sprint 5 — post-publish smoke.** `brew update-reset`, `brew reinstall`,
   and `brew test` against the published formula; report PASS/FAIL.

Each sprint is independently shippable and individually reduces operator
toil.

## Open questions / trade-offs

- **Where does the tap live?** Today it's `~/Project/sympoies/homebrew-tap`
  for this operator, but the skill should not assume that path. Lean toward
  `NILS_CLI_HOMEBREW_TAP_DIR` env (overridable) plus convention fallback.
- **Should the skill also auto-prune stale tap tags (e.g. `v0.3.5`)?**
  Answer: no, that's destructive and out of scope. Skill should `fetch
  --no-tags` to avoid the conflict; cleanup is a separate one-shot.
- **Default for `--wait-tap-release`?** Default-on so releases are
  end-to-end verified, but allow `--skip-tap-wait` for the operator to
  return control quickly when CI is reliable.
- **Coupling to `graysurf/nils-cli` artifact origin.** Parse from existing
  formula instead of hardcoding so a future repo move surfaces as a single
  formula edit, not a hidden assumption in the skill.
- **`/release` slash command.** Per `Alternate entry points` in SKILL.md the
  same flow is reachable via `/release --version X.Y.Z`; whichever option is
  chosen, the wrapper at `<repo>/.agents/scripts/release.sh` should keep
  forwarding args unchanged so the dispatcher contract stays intact.

## Acceptance criteria

- A clean run of `nils-cli-bump-version-tag-release --version <next>` ends
  with all of:
  - `nils-cli` `v<next>` tag pushed and `release.yml` green.
  - Tap formula at `v<next>` URL/sha256 on `main` with semantic-commit.
  - Tap `nils-cli-v<next>` prefix tag pushed; tap `release.yml` green.
  - Operator received zero manual prompts during the tap stage.
- A retry of the same command at the same version is a no-op (idempotent).
- `--from-tap --version <next>` covers the recovery path used in this
  session (skip nils-cli bump, do tap stage only).

## Sources

- `.agents/skills/nils-cli-bump-version-tag-release/SKILL.md`
- `.agents/skills/nils-cli-bump-version-tag-release/scripts/nils-cli-bump-version-tag-release.sh`
- `~/Project/sympoies/homebrew-tap/DEVELOPMENT.md`
- `~/Project/sympoies/homebrew-tap/.github/workflows/release.yml`
- Today's session: nils-cli runs 24936022343 (v0.7.4) and 24954986627
  (v0.7.6); tap run 24955264588 (v0.7.6, manually triggered).
