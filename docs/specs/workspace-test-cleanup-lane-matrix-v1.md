# Workspace Test Cleanup Lane Matrix v1

## Purpose

This spec freezes the Sprint 3 stale-test cleanup map so subagent lanes keep deterministic `remove`, `rewrite`, `keep`, and `defer`
decisions while running in parallel.

Canonical audit inputs:

- `bash scripts/dev/workspace-test-stale-audit.sh --format tsv`
- `$AGENT_HOME/out/workspace-test-cleanup/stale-tests.tsv`
- `$AGENT_HOME/out/workspace-test-cleanup/decision-rubric.md`
- `$AGENT_HOME/out/workspace-test-cleanup/crate-tiers.tsv`
- `$AGENT_HOME/out/workspace-test-cleanup/execution-manifest.md`

## Decision Matrix

| Signal + condition | Decision mode | Tier | Lane routing | Notes |
| --- | --- | --- | --- | --- |
| `helper_fanout` with `fanout=0`, `review=auto` | `remove` | `safe` | `serial-*` for frozen crates, otherwise `parallel` | Deterministic orphan helper candidate. |
| `helper_fanout` with `fanout=0`, `review=manual-review` | `defer` | `high-risk` | `serial-*` for frozen crates, otherwise `parallel` | Macro/reflection risk blocks automatic removal. |
| `helper_fanout` with `fanout>0` | `keep` | `medium` | `serial-*` for frozen crates, otherwise `parallel` | Helper still has live callsites. |
| `allow_dead_code` marker in tests | `rewrite` | `medium` | `serial-*` for frozen crates, otherwise `parallel` | Replace obsolete helper/test shape without dropping coverage. |
| `deprecated_path_marker` marker in tests/path | `rewrite` | `high-risk` | `serial-*` for frozen crates, otherwise `parallel` | Requires explicit replacement coverage and contract check. |
| `test_module` on contract-protected path (`contract/parity/json/exit-code`) | `keep` (or `rewrite` with equivalent coverage) | `high-risk` | `serial-*` for frozen crates, otherwise `parallel` | Never treat protected modules as opportunistic `remove`. |

## Frozen Serial-Group Order

The serialized order is frozen by Sprint 3 Task 3.1 and is no longer recomputed from top-N score drift:

| Order | Crate | Candidates | Helper Signals | allow(dead_code) | Score | Serial Group |
| ---: | --- | ---: | ---: | ---: | ---: | --- |
| 1 | `git-cli` | 37 | 27 | 1 | 94 | `serial-1` |
| 2 | `agent-docs` | 32 | 20 | 1 | 75 | `serial-2` |
| 3 | `macos-agent` | 37 | 7 | 2 | 57 | `serial-3` |
| 4 | `fzf-cli` | 19 | 5 | 7 | 50 | `serial-4` |
| 5 | `memo` | 25 | 5 | 5 | 50 | `serial-5` |

All non-listed crates stay `parallel`.

## Crate Tier Snapshot

From the current `execution-manifest.md`:

| Crate | Safe | Medium | High-Risk | Serial Group |
| --- | ---: | ---: | ---: | --- |
| agent-docs | 1 | 30 | 1 | serial-2 |
| api-gql | 0 | 5 | 1 | parallel |
| api-grpc | 0 | 2 | 1 | parallel |
| api-rest | 0 | 9 | 1 | parallel |
| api-test | 0 | 3 | 2 | parallel |
| api-testing-core | 0 | 24 | 0 | parallel |
| api-websocket | 0 | 2 | 2 | parallel |
| cli-template | 0 | 1 | 0 | parallel |
| codex-cli | 0 | 29 | 11 | parallel |
| fzf-cli | 0 | 18 | 1 | serial-4 |
| gemini-cli | 0 | 30 | 10 | parallel |
| git-cli | 13 | 22 | 2 | serial-1 |
| git-lock | 0 | 15 | 2 | parallel |
| git-scope | 0 | 15 | 3 | parallel |
| git-summary | 0 | 10 | 1 | parallel |
| image-processing | 0 | 8 | 0 | parallel |
| macos-agent | 0 | 35 | 2 | serial-3 |
| memo | 0 | 22 | 3 | serial-5 |
| nils-common | 0 | 1 | 1 | parallel |
| nils-term | 0 | 1 | 0 | parallel |
| nils-test-support | 0 | 6 | 0 | parallel |
| plan-issue-cli | 0 | 17 | 3 | parallel |
| plan-tooling | 0 | 9 | 2 | parallel |
| screen-record | 0 | 19 | 1 | parallel |
| semantic-commit | 0 | 11 | 1 | parallel |

## Baseline Update Policy (No Silent Regression Hiding)

- `scripts/ci/test-stale-audit-baseline.tsv` remains a strict subset of the frozen S3T1 orphan-helper allowlist enforced by
  `scripts/ci/test-stale-audit.sh`.
- Allowed baseline movement during cleanup: row deletions after replacement coverage is merged and stale helper removal is validated.
- Disallowed baseline movement: adding or rewriting rows to absorb new regressions without first updating governance/spec policy and explicit
  review evidence.
- `deprecated_path_marker` regressions are never baselined; they must be rewritten or removed.

## Validation

```bash
test -f docs/specs/workspace-test-cleanup-lane-matrix-v1.md
bash scripts/dev/workspace-test-stale-audit.sh --format tsv
bash scripts/ci/test-stale-audit.sh --strict
rg -n 'serial|parallel|remove|rewrite|defer' docs/specs/workspace-test-cleanup-lane-matrix-v1.md
```
