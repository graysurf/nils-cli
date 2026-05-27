# GitLab Test Migration to gitlab.com Implementation Handoff

| Field | Value |
| --- | --- |
| Status | Ready for implementation |
| Date | 2026-05-25 |
| Source | Conversation requirement: stop referencing the internal Gamania GitLab server in this public CLI repo; route GitLab-side tests / docs / live sandbox at `gitlab.com` instead |
| Intended next step | Land Sprint 1 (fixture swap + auth-status decoupling), then Sprint 2 (docs sweep), then Sprint 3 (live sandbox on gitlab.com) |

## Purpose

`sympoies/nils-cli` currently mentions `gitlab.gamania.com` (Gamania internal
GitLab instance) and `terrylin/agent-runtime-testing` (the Gamania-side
sandbox project) in unit-test fixtures, plan/runbook docs, and a few
auth-status stubs. The repo is published to GitHub and crates.io, so the
internal-host references should be removed and the live GitLab sandbox should
move to a `gitlab.com` project the maintainer can drive directly.

This plan migrates the three reference surfaces in this order:

1. **Source-level test fixtures** — flip the strings to `gitlab.com` and a
   neutral sandbox slug; drop the username assertions that pin the maintainer
   account.
2. **Docs / runbook references** — rewrite the 7 plan + runbook files that
   name the Gamania host or `terrylin/agent-runtime-testing`.
3. **Live sandbox** — create `graysury/nils-cli-gitlab-sandbox` on
   gitlab.com and validate the GitLab MR delivery skills against it.

No CLI behavior or schema changes. No new test gating. No new env vars.

## Confirmed facts

- `glab auth status --hostname gitlab.com` reports `graysury` as the logged-in
  user; the gitlab.com account has 0 projects today
  (`projects?membership=true` returns `[]`). [A1]
- Repo references to `gitlab.gamania.com` live in three source files (pure
  `#[test]` fixtures, no network) and seven doc files (historical
  plan/runbook handoffs). [F1]
  - `crates/forge-cli/src/ops/issue_view.rs` — 8 URL hits + doc comments at
    lines 141 / 409. The fixture purpose was to verify
    `gitlab_host_from_url` extracts the host from `web_url` even on a
    non-`gitlab.com` instance. [F2]
  - `crates/forge-cli/src/ops/pr_comments.rs` — 3 URL hits in the MR-notes
    fixture. [F3]
  - `crates/plan-issue-cli/src/provider.rs` — 5 fixture hits (host
    classification + ssh/https remote parsing) + doc comment at line 57. [F4]
- `FORGE_CLI_E2E=1` is only mentioned in spec / plan docs; no test wiring
  currently consumes it. All GitLab integration tests under
  `crates/forge-cli/tests/integration/` use stub `glab` binaries. There is no
  live-network test that depends on the Gamania server. [F5]
- `crates/forge-cli/tests/integration/auth_status.rs` and
  `crates/forge-cli/src/ops/auth_status.rs` pin the GitHub + GitLab user name
  to `graysurf` (GitHub identity), even though the gitlab.com identity is
  actually `graysury`. The integration test compares `user` across backends
  for parity. [F6]
- `crates/forge-cli/tests/integration/pr_deliver_chain.rs:153` and
  `crates/forge-cli/tests/integration/exit_codes.rs:18` carry the same
  `graysurf` stub stderr but do not assert on the value. [F7]

## Decisions

1. **Swap all `gitlab.gamania.com` strings to `gitlab.com`** in source fixtures
   and the `terrylin/agent-runtime-testing` slug to `graysury/nils-cli-gitlab-sandbox`.
   The fixtures still parse a `web_url` correctly (the parser code path is
   exercised), but they no longer represent a self-hosted GitLab instance.
   The narrow "non-`gitlab.com` host extraction" coverage is intentionally
   dropped because it is not needed for this maintainer's workflow.
2. **Decouple `auth status` tests from a specific username.** Integration
   tests drop the `data.user == "graysurf"` assertions entirely. Unit-test
   parser fixtures in `crates/forge-cli/src/ops/auth_status.rs` keep
   structural coverage that the parser extracts a non-empty username, but
   stop asserting the exact string. The integration parity test compares
   schema/ok/host across backends — not the username.
3. **Rewrite all 7 plan / runbook references**, not just the durable runbook.
   Historical plan docs become inconsistent if half still name the Gamania
   server. `gitlab.gamania.com` → `gitlab.com`; `terrylin/agent-runtime-testing`
   → `graysury/nils-cli-gitlab-sandbox`.
4. **Create `graysury/nils-cli-gitlab-sandbox` on gitlab.com**, visibility
   `private`, default branch `main`, README only. This is the new home for
   live `pr/create-gitlab-mr`, `pr/close-gitlab-mr`, and `pr/deliver-gitlab-mr`
   validation sweeps.
5. **No code-behavior changes.** This plan is a string + docs migration plus
   a sandbox bootstrap; the provider abstraction, host classification, and
   envelope contracts stay as they are.

## Scope

- In scope:
  - String swap in 3 source files (13 fixture occurrences + 3 doc comments).
  - Drop / soften username assertions across 2 auth-status test files.
  - Rewrite 7 doc files (1 runbook + 6 plan docs) under
    `crates/plan-issue-cli/docs/runbooks/` and `docs/plans/`.
  - Create the gitlab.com sandbox project.
  - Manual live sweep of `forge-cli` GitLab-arm commands against the new
    sandbox; record evidence inside the worktree.
- Out of scope:
  - Adding gated live tests (`FORGE_CLI_E2E=1`).
  - Reworking provider-detection or host-classification logic.
  - Changing the published crates' behavior, CLI surface, or schemas.
  - Importing or migrating data from `terrylin/agent-runtime-testing`.
  - Touching the existing `gitlab.gamania.com` glab auth profile.

## Requirements

- R1. After Sprint 1, no source file (`crates/**/*.rs`) references
  `gitlab.gamania.com`, `terrylin/agent-runtime-testing`, `graysurf`, or
  `graysury` outside the agent-runtime-cli (which legitimately references
  the GitHub user `graysurf`).
- R2. After Sprint 2, the only remaining references to `gitlab.gamania.com`
  in the whole worktree are in: (a) git history, (b) `THIRD_PARTY_*`
  generated files if they happen to mention it (none expected), and
  (c) `docs/plans/gitlab-com-test-migration/**` (this migration plan
  itself, which must name what is being moved). No other `docs/plans/**`,
  `crates/**/docs/**`, or top-level docs file references the Gamania host
  or sandbox slug.
- R3. After Sprint 3, `graysury/nils-cli-gitlab-sandbox` exists on
  `gitlab.com`, can be addressed by `forge-cli --repo`, and has at least
  one disposable MR successfully exercised through
  `pr deliver --kind feature --no-merge`.
- R4. `cargo test --workspace` is green throughout. No new failing tests
  introduced.
- R5. The local-fast CI gate from
  `scripts/ci/nils-cli-local-fast.sh` (or its DEVELOPMENT.md equivalent)
  passes on the migration branch before PR.

## Acceptance criteria

- AC-1. `rg -n 'gitlab\.gamania\.com|terrylin/agent-runtime-testing' --type rust`
  returns no matches.
- AC-2. `rg -n 'gitlab\.gamania\.com|terrylin/agent-runtime-testing' docs/plans/ crates/*/docs/`
  returns matches only inside `docs/plans/gitlab-com-test-migration/` (this
  migration plan itself).
- AC-3. `cargo test --workspace` passes.
- AC-4. `forge-cli --format json --repo graysury/nils-cli-gitlab-sandbox issue list --state all`
  returns an `ok=true` envelope against the live sandbox.
- AC-5. A live `pr deliver --kind feature --no-merge` sweep on the sandbox
  advances past the version probe and emits an `ok=true` envelope at the
  create step (subsequent failures, if any, must not be host-related).

## Validation plan

1. `cargo test --workspace` after Sprint 1.
2. `rg` audits per AC-1 / AC-2 after each sprint.
3. Local-fast CI gate after Sprint 2 (`scripts/ci/nils-cli-local-fast.sh`).
4. Sandbox bootstrap via `glab --hostname gitlab.com repo create
   graysury/nils-cli-gitlab-sandbox --visibility private`.
5. Live `forge-cli` sweep against the sandbox; capture the envelopes into
   the worktree under `evidence/` (or the project-defined agent-out path)
   for the PR description.

## Findings table

| ID | Source | Disposition |
| --- | --- | --- |
| F-1 | Source fixtures naming `gitlab.gamania.com` / `terrylin/agent-runtime-testing` | Sprint 1 |
| F-2 | `auth status` tests pinning username `graysurf` | Sprint 1 |
| F-3 | 7 plan/runbook docs referencing the Gamania host or sandbox slug | Sprint 2 |
| F-4 | No gitlab.com sandbox project exists yet under `graysury` | Sprint 3 |
| F-5 | gitlab.com identity is `graysury`, not `graysurf` (parity drift if not decoupled) | Sprint 1 (mitigated by F-2 decoupling) |

## Risks and guardrails

- **R-1**: Dropping the self-hosted-host fixture removes the regression
  guardrail for the "host extracted from `web_url`" code path on non-`gitlab.com`
  hosts. **Mitigation**: the parser is still exercised through the same
  fixtures with `gitlab.com` URLs; if a non-`gitlab.com` regression matters
  in the future, a dedicated test against `gitlab.example.com` (RFC reserved)
  can be reintroduced without re-naming a real internal server.
- **R-2**: Username-decoupling could weaken parser unit-test coverage if the
  assertion is removed naively. **Mitigation**: keep a structural assertion
  (e.g., `payload.user.is_some()` plus a substring containment check) and
  use a neutral stub value such as `testuser-gh` / `testuser-glab` so the
  parser still has to extract the right substring from stub stderr.
- **R-3**: Live sandbox creation could collide with a future namespace
  reservation on gitlab.com. **Mitigation**: project is private and named
  `nils-cli-gitlab-sandbox` (no general-purpose name); rerunnable.
- **R-4**: Doc rewrites in historical plan files may invalidate references
  cited from other plans / runbooks. **Mitigation**: after the doc sweep,
  `rg -n 'gitlab\.gamania\.com|terrylin/agent-runtime-testing'` across the
  whole worktree must return zero matches; any cross-file pointer that
  breaks gets resolved in the same sprint.

## Execution

- Recommended plan: docs/plans/gitlab-com-test-migration/gitlab-com-test-migration-plan.md
- Recommended execution state: docs/plans/gitlab-com-test-migration/gitlab-com-test-migration-execution-state.md
- Status: ready
- Next-task source: Sprint 1 in the plan

## Retention intent

Promote after merge. The "neutral hostname / decoupled username" pattern
documented here is the canonical playbook for any future fixture that
needs to look like GitLab without naming a real account or instance.

## Read-first references

- Source fixture sites:
  - `crates/forge-cli/src/ops/issue_view.rs:670,678,683,715,718,732,739`
    (plus comments at 141 and 409)
  - `crates/forge-cli/src/ops/pr_comments.rs:443,501,508`
  - `crates/plan-issue-cli/src/provider.rs:317,331,334,361,363` (plus comment
    at 57)
- Auth-status sites:
  - `crates/forge-cli/tests/integration/auth_status.rs:10,17,41,57,81`
  - `crates/forge-cli/src/ops/auth_status.rs:239,244,256,261`
  - `crates/forge-cli/tests/integration/pr_deliver_chain.rs:153`
  - `crates/forge-cli/tests/integration/exit_codes.rs:18`
- Doc sites:
  - `crates/plan-issue-cli/docs/runbooks/provider-routing-runbook.md`
  - `docs/plans/gitlab-mr-unblock/gitlab-mr-unblock-{plan,discussion-source}.md`
  - `docs/plans/plan-issue-cli-provider-abstraction/*.md`
  - `docs/plans/forge-cli-inbox/*.md`
  - `docs/plans/forge-cli-inbox-latency/*.md`

## Source type

`discussion-to-implementation-doc`
