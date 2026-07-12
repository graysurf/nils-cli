# Codex Optional Usage Windows Implementation Handoff

## Status

- Date: 2026-07-12
- Status: approved for L2 implementation and end-to-end delivery
- Source: user-provided rollout announcement and screenshots, live
  `codex-cli diag rate-limits` API evidence, current nils-cli and Agent Console
  consumer code, official OpenAI Codex source.
- Intended next step: execute the linked L2 plan through release and live smoke.

## Purpose

Restore every local Codex usage surface after OpenAI temporarily removed the
five-hour restriction and began returning a weekly-only rate-limit object.
Preserve compatibility when the five-hour window returns.

## Confirmed Facts

- The live API returned a valid 604800-second `primary_window` and a null
  `secondary_window` for every configured Codex account.
- nils-cli required both fields and returned `invalid-usage-payload`, causing
  the prompt/table to serve stale cache and the Agent Console reader to expose
  empty/error usage.
- Official OpenAI Codex models each window independently as optional and maps
  each present window independently.
- Agent Console UI components already iterate a variable-length window list.
- Current nils-cli diagnostic redaction removes credentials but not upstream
  email, user ID, or account ID fields.

## Decisions

- Model upstream windows as independently optional; do not special-case only
  `secondary_window: null`.
- Make the dynamic `windows` array authoritative for consumers and retain the
  v1 `summary` object as a compatibility projection.
- Never fabricate a missing five-hour window as zero usage.
- A valid weekly-only response refreshes cache and is not stale.
- Update the live `sympoies-infra` reader to prefer `windows` and keep summary
  fallback for previously released nils-cli binaries.
- Keep Agent Console production UI code unchanged unless live acceptance proves
  a renderer defect.
- Remove PII from diagnostic output while retaining safe plan and numeric usage
  fields needed by consumers.

## Scope

- nils-cli Codex rate-limit parser, derived values, JSON/text/table output,
  prompt cache, writeback, tests, and contract documentation.
- The in-progress `agent-session /usage` consumer contract and regression test.
- nils-cli PR, release, tap update, and installed binary verification.
- `sympoies-infra` host usage reader normalization, tests, PR, deploy, and live
  service/UI smoke.

## Non-Scope

- OpenAI entitlement or reset behavior.
- Claude usage collection.
- Agent Console layout or visual redesign.
- Persisting raw live API payloads in plan evidence.

## Requirements

1. Accept zero, one, or two independently optional upstream windows.
2. Reject malformed present window objects without treating valid missing/null
   siblings as malformed.
3. Emit only actual windows in JSON and text surfaces.
4. Preserve the existing two-window behavior and v1 envelope.
5. Refresh or clear optional cache fields so removed windows do not survive as
   stale display data.
6. Keep the host reader compatible with both dynamic-window and legacy-summary
   nils-cli results.
7. Prevent credentials and PII from appearing in diagnostic or browser-facing
   output.
8. Release, install, deploy, and prove the behavior against live state.

## Acceptance Criteria

- Weekly-only fixtures and the live weekly-only API return success.
- Prompt output and the all-account table show current weekly usage without a
  stale five-hour value.
- JSON results contain exactly the live windows and no PII.
- `agent-session /usage` and the host reader normalize a weekly-only result.
- Agent Console renders the weekly meter rather than an empty dash.
- Required local and provider checks pass in both repositories.
- The nils-cli release, local installation, host reader deployment, and live
  smoke all succeed.

## Validation Plan

- Capture test-first red evidence before Rust production edits.
- Run focused codex-cli tests during implementation and the repository
  `--local-fast` gate before delivery.
- Run testing, maintainability, and API-contract specialist review before merge.
- Run the nils-cli release workflow and installed binary live API smoke.
- Run `sympoies-infra` usage-reader tests and repository validation, then its
  repo-owned host install/deploy and smoke commands.
- Query only whitelisted live fields and inspect the rendered Agent Console
  usage widget after deployment.

## Risks and Guardrails

- Upstream rollout state may change while work runs; deterministic fixtures are
  the contract evidence and live smoke proves current compatibility.
- The existing shared checkout is dirty; use managed clean worktrees only.
- Do not print or retain full live API objects, auth files, service environment,
  or bearer tokens.
- Do not merge either PR before its independent review and provider checks pass.

## Execution

Recommended plan: docs/plans/2026-07-12-codex-optional-usage-windows/2026-07-12-codex-optional-usage-windows-plan.md

Recommended execution state: docs/plans/2026-07-12-codex-optional-usage-windows/2026-07-12-codex-optional-usage-windows-execution-state.md

- Status: execute now through release, deploy, and live smoke.
- Next-task source: Sprint 1, Task 1.1 in the recommended plan.
- Retention intent: transient plan source; archive with the completed L2 bundle.

## Read-First References

- `crates/codex-cli/src/rate_limits/render.rs`
- `crates/codex-cli/src/rate_limits/cache.rs`
- `crates/codex-cli/src/prompt_segment/refresh.rs`
- `crates/codex-cli/docs/specs/codex-cli-diag-rate-limits-and-auth-json-contract-v1.md`
- `crates/agent-session/src/serve.rs`
- `graysurf/sympoies-infra/host/agent-console/bin/agent_console_usage.py`
