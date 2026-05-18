# Workspace Doc Retention Matrix v1

## Purpose

This matrix records finalized documentation ownership and retention decisions for the simplified
workspace architecture.

Decision fields:

- `scope`: `workspace-level` | `crate-local` | `transient-dev-record`
- `lifecycle`: `canonical` | `delete`
- `decision`: `keep` | `delete` | `move`

## Workspace-Level Inventory (Keep)

| Path | Scope | Lifecycle | Decision | Rationale |
| --- | --- | --- | --- | --- |
| `README.md` | `workspace-level` | `canonical` | `keep` | Workspace overview and contributor entrypoint. |
| `DEVELOPMENT.md` | `workspace-level` | `canonical` | `keep` | Required checks and contributor workflow contract. |
| `AGENTS.md` | `workspace-level` | `canonical` | `keep` | Agent execution policy for this repository. |
| `BINARY_DEPENDENCIES.md` | `workspace-level` | `canonical` | `keep` | Shared runtime/tooling dependency contract. |
| `docs/runbooks/cli-completion-development-standard.md` | `workspace-level` | `canonical` | `keep` | Canonical completion architecture and checks. |
| `docs/runbooks/crates-io-status-script-runbook.md` | `workspace-level` | `canonical` | `keep` | Workspace crates.io status workflow. |
| `docs/runbooks/new-cli-crate-development-standard.md` | `workspace-level` | `canonical` | `keep` | New CLI crate standards. |
| `docs/runbooks/test-cleanup-governance.md` | `workspace-level` | `canonical` | `keep` | Stale-test lifecycle and CI guardrails. |
| `docs/specs/cli-service-json-contract-guideline-v1.md` | `workspace-level` | `canonical` | `keep` | Service-consumed CLI JSON contract guidance. |
| `docs/specs/codex-gemini-cli-parity-contract-v1.md` | `workspace-level` | `canonical` | `keep` | Shared Codex/Gemini parity contract. |
| `docs/specs/codex-gemini-runtime-contract.md` | `workspace-level` | `canonical` | `keep` | Shared provider runtime contract. |
| `docs/specs/completion-contract-template.md` | `workspace-level` | `canonical` | `keep` | Per-crate completion migration contract template. |
| `docs/specs/completion-coverage-matrix-v1.md` | `workspace-level` | `canonical` | `keep` | Completion obligations and enforcement metadata matrix. |
| `docs/specs/crate-docs-placement-policy.md` | `workspace-level` | `canonical` | `keep` | Workspace docs placement policy. |
| `docs/specs/third-party-artifacts-contract-v1.md` | `workspace-level` | `canonical` | `keep` | Third-party artifacts generation contract. |
| `docs/specs/workspace-ci-entrypoint-inventory-v1.md` | `workspace-level` | `canonical` | `keep` | CI owner-script inventory and keep/delete criteria. |
| `docs/specs/workspace-shared-crate-boundary-v1.md` | `workspace-level` | `canonical` | `keep` | Shared crate ownership boundaries. |
| `docs/specs/workspace-test-cleanup-lane-matrix-v1.md` | `workspace-level` | `canonical` | `keep` | Test cleanup sequencing and lane policy. |
| `docs/specs/workspace-doc-retention-matrix-v1.md` | `workspace-level` | `canonical` | `keep` | Doc ownership and retention source of truth (this file). |

## Crate-Local Inventory (Keep)

All paths below are classified as `scope=crate-local`, `lifecycle=canonical`, `decision=keep`.
Rationale: each file is owned by one crate and lives under `crates/<crate>/docs/**`.

This section is the authoritative crate-local docs inventory. It MUST match the canonical find
output exactly:

```bash
find crates -type f -name '*.md' \
  -not -path '*/tests/*' \
  -not -path '*/src/*' \
  -not -path '*/assets/*' \
  -not -path 'crates/*/README.md' \
  -not -path 'crates/plan-tooling/plan-template.md'
```

Top-level `crates/<crate>/README.md` files and `crates/<crate>/docs/README.md` index files are
tracked separately in the [Crate Top-Level README Inventory](#crate-top-level-readme-inventory-keep)
below; the canonical `find` glob `crates/*/README.md` excludes any `README.md` under a crate root
because BSD-style `-path` lets `*` cross slash boundaries.

- `crates/api-websocket/docs/specs/websocket-cli-contract-v1.md`
- `crates/api-websocket/docs/specs/websocket-request-schema-v1.md`
- `crates/codex-cli/docs/runbooks/json-consumers.md`
- `crates/codex-cli/docs/specs/codex-cli-diag-rate-limits-and-auth-json-contract-v1.md`
- `crates/gemini-cli/docs/runbooks/json-consumers.md`
- `crates/gemini-cli/docs/specs/gemini-cli-diag-rate-limits-and-auth-json-contract-v1.md`
- `crates/image-processing/docs/runbooks/llm-svg-workflow.md`
- `crates/memo-cli/docs/runbooks/memo-cli-agent-workflow.md`
- `crates/memo-cli/docs/specs/memo-cli-command-contract-v1.md`
- `crates/memo-cli/docs/specs/memo-cli-json-contract-v1.md`
- `crates/memo-cli/docs/specs/memo-cli-release-policy.md`
- `crates/memo-cli/docs/specs/memo-cli-storage-schema-v1.md`
- `crates/memo-cli/docs/specs/memo-cli-workflow-extension-contract-v1.md`
- `crates/nils-common/docs/specs/markdown-helpers-contract-v1.md`
- `crates/plan-issue-cli/docs/specs/plan-issue-cli-contract-v2.md`
- `crates/plan-issue-cli/docs/specs/plan-issue-gate-matrix-v1.md`
- `crates/plan-issue-cli/docs/specs/plan-issue-state-machine-v1.md`
- `crates/plan-tooling/docs/runbooks/split-prs-build-task-spec-cutover.md`
- `crates/plan-tooling/docs/specs/plan-source-bundle-contract-v1.md`
- `crates/plan-tooling/docs/specs/split-prs-contract-v1.md`
- `crates/plan-tooling/docs/specs/split-prs-contract-v2.md`

## Crate Top-Level README Inventory (Keep)

All paths below are classified as `scope=crate-local`, `lifecycle=canonical`, `decision=keep`.
Rationale: every workspace member crate ships a top-level `README.md` (rendered on crates.io and
as the GitHub crate-directory landing page) and a `docs/README.md` index for crate-local docs.
These files are excluded from the canonical Crate-Local Inventory `find` pattern, so they are
tracked here.

Top-level crate READMEs (one per workspace member, 25 total):

- `crates/agent-docs/README.md`
- `crates/api-gql/README.md`
- `crates/api-grpc/README.md`
- `crates/api-rest/README.md`
- `crates/api-test/README.md`
- `crates/api-testing-core/README.md`
- `crates/api-websocket/README.md`
- `crates/cli-template/README.md`
- `crates/codex-cli/README.md`
- `crates/fzf-cli/README.md`
- `crates/gemini-cli/README.md`
- `crates/git-cli/README.md`
- `crates/git-lock/README.md`
- `crates/git-scope/README.md`
- `crates/git-summary/README.md`
- `crates/image-processing/README.md`
- `crates/macos-agent/README.md`
- `crates/memo-cli/README.md`
- `crates/nils-common/README.md`
- `crates/nils-term/README.md`
- `crates/nils-test-support/README.md`
- `crates/plan-issue-cli/README.md`
- `crates/plan-tooling/README.md`
- `crates/screen-record/README.md`
- `crates/semantic-commit/README.md`

Crate `docs/README.md` index files (one per workspace member, 25 total):

- `crates/agent-docs/docs/README.md`
- `crates/api-gql/docs/README.md`
- `crates/api-grpc/docs/README.md`
- `crates/api-rest/docs/README.md`
- `crates/api-test/docs/README.md`
- `crates/api-testing-core/docs/README.md`
- `crates/api-websocket/docs/README.md`
- `crates/cli-template/docs/README.md`
- `crates/codex-cli/docs/README.md`
- `crates/fzf-cli/docs/README.md`
- `crates/gemini-cli/docs/README.md`
- `crates/git-cli/docs/README.md`
- `crates/git-lock/docs/README.md`
- `crates/git-scope/docs/README.md`
- `crates/git-summary/docs/README.md`
- `crates/image-processing/docs/README.md`
- `crates/macos-agent/docs/README.md`
- `crates/memo-cli/docs/README.md`
- `crates/nils-common/docs/README.md`
- `crates/nils-term/docs/README.md`
- `crates/nils-test-support/docs/README.md`
- `crates/plan-issue-cli/docs/README.md`
- `crates/plan-tooling/docs/README.md`
- `crates/screen-record/docs/README.md`
- `crates/semantic-commit/docs/README.md`

## Transient/Obsolete Inventory (Delete or Move)

All transient/obsolete entries previously tracked here have been removed from the working tree and
are kept off-tree by the `removed_transient_docs` allowlist in
`scripts/ci/docs-hygiene-audit.sh`. No active rows remain in this matrix.

If a new transient/obsolete artifact is added in the future, append it here with `scope`,
`lifecycle`, `decision`, `reason`, and an inbound-reference proof; update
`scripts/ci/docs-hygiene-audit.sh` to assert the file stays removed once delete is decided.
