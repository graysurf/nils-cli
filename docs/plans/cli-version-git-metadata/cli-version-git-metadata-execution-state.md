<!-- execute-from-tracking-issue:state:v1 -->
# CLI version git metadata — Execution State

## Execution State

- Status: complete
- Target scope: whole plan
- Execution window: whole plan (two sprints)
- Current task: complete
- Next task: none
- Last updated: 2026-05-29 Asia/Taipei
- Branch/commit/PR/release: branch `feat/cli-version-git-metadata`; commit/PR pending
- Source document: docs/plans/cli-version-git-metadata/cli-version-git-metadata-plan.md
- Discussion source document: docs/plans/cli-version-git-metadata/cli-version-git-metadata-discussion-source.md
- Source issue: none
- Tracking issue: <https://github.com/sympoies/nils-cli/issues/624>
- Source snapshot: pending
- Plan snapshot: pending
- Initial execution state snapshot: pending
- Direct source-doc execution waiver: not applicable

## Task Ledger

| ID       | Status | Task                                                         | Evidence                                                                                                                                | Notes                                                            |
| -------- | ------ | ------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------- |
| Task 1.1 | done   | Scaffold `nils-build-info` with the zero-dependency build.rs | added crates/nils-build-info build.rs capturing git describe and rustc; cargo publish -p nils-build-info --dry-run --allow-dirty passed | leaf crate; emits NILS_GIT_DESCRIBE + NILS_RUSTC_VERSION         |
| Task 1.2 | done   | Public surface and unit tests                                | added GIT_DESCRIBE/RUSTC_VERSION consts and long_version helper with unit tests; cargo test -p nils-build-info passed                   | GIT_DESCRIBE / RUSTC_VERSION consts + `long_version(pkg)` helper |
| Task 2.1 | done   | Add `long_version` to every binary's clap definition         | wired long_version across required binaries; all-binary -V/--version smoke passed including plan-issue/plan-issue-local                 | keep `-V` clean semver; add long form; per-crate dep             |
| Task 2.2 | done   | Tests, completion/asset audits, and full required checks     | cargo test -p agent-runtime-cli; completion flag and asset audits; bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast passed    | guard agent-runtime-cli version test + strict audits             |

## Validation

| Command                                                      | Status | Summary                                                            | Artifact |
| ------------------------------------------------------------ | ------ | ------------------------------------------------------------------ | -------- |
| `cargo test -p nils-build-info`                              | pass   | build-info unit tests passed                                       | —        |
| `cargo test -p agent-runtime-cli`                            | pass   | agent-runtime version integration coverage passed                  | —        |
| `bash scripts/ci/completion-flag-parity-audit.sh --strict`   | pass   | required completion flag parity passed                             | —        |
| `bash scripts/ci/completion-asset-audit.sh --strict`         | pass   | required completion asset audit passed                             | —        |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` | pass   | workspace local-fast gate passed after docs and build-script fixes | —        |

## Blockers

- none

## Session Log

- 2026-05-29: Bundle drafted from the 2026-05-29 versioning discussion. All
  three open questions resolved at the source doc (rollout = all binary crates
  in one PR; rustc version included; single-line long-version format).
  Implementation not yet started; this bundle drives `record open` of the
  tracking issue, then the standard execute / deliver / closeout flow.
- 2026-05-29: Implemented `nils-build-info`, wired long `--version` output
  across required CLI binaries while preserving clean `-V`, fixed the shared
  `plan-issue` parser command name for both executable flavors, and reran the
  local-fast gate successfully. Added the required crate docs index and
  hardened build-script git rerun paths before the final validation pass.
