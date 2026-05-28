<!-- execute-from-tracking-issue:state:v1 -->
# CLI version git metadata — Execution State

## Execution State

- Status: not started
- Target scope: whole plan
- Execution window: whole plan (two sprints)
- Current task: Task 1.1 — scaffold `nils-build-info` with the build.rs
- Next task: Task 1.2 — public surface and unit tests
- Last updated: 2026-05-29 Asia/Taipei
- Branch/commit/PR/release: pending (implementation not started)
- Source document: docs/plans/cli-version-git-metadata/cli-version-git-metadata-plan.md
- Discussion source document: docs/plans/cli-version-git-metadata/cli-version-git-metadata-discussion-source.md
- Source issue: none
- Tracking issue: pending (this bundle drives `plan-issue record open`)
- Source snapshot: pending
- Plan snapshot: pending
- Initial execution state snapshot: pending
- Direct source-doc execution waiver: not applicable

## Task Ledger

| ID       | Status      | Task                                                         | Evidence | Notes                                                            |
| -------- | ----------- | ------------------------------------------------------------ | -------- | ---------------------------------------------------------------- |
| Task 1.1 | not started | Scaffold `nils-build-info` with the zero-dependency build.rs | —        | leaf crate; emits NILS_GIT_DESCRIBE + NILS_RUSTC_VERSION         |
| Task 1.2 | not started | Public surface and unit tests                                | —        | GIT_DESCRIBE / RUSTC_VERSION consts + `long_version(pkg)` helper |
| Task 2.1 | not started | Add `long_version` to every binary's clap definition         | —        | keep `-V` clean semver; add long form; per-crate dep             |
| Task 2.2 | not started | Tests, completion/asset audits, and full required checks     | —        | guard agent-runtime-cli version test + strict audits             |

## Validation

| Command                                                      | Status  | Summary | Artifact |
| ------------------------------------------------------------ | ------- | ------- | -------- |
| `cargo test -p nils-build-info`                              | planned | —       | —        |
| `cargo test -p agent-runtime-cli`                            | planned | —       | —        |
| `bash scripts/ci/completion-flag-parity-audit.sh --strict`   | planned | —       | —        |
| `bash scripts/ci/completion-asset-audit.sh --strict`         | planned | —       | —        |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` | planned | —       | —        |

## Blockers

- none

## Session Log

- 2026-05-29: Bundle drafted from the 2026-05-29 versioning discussion. All
  three open questions resolved at the source doc (rollout = all binary crates
  in one PR; rustc version included; single-line long-version format).
  Implementation not yet started; this bundle drives `record open` of the
  tracking issue, then the standard execute / deliver / closeout flow.
