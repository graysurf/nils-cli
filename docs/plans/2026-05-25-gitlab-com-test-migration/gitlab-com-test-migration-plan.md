# Plan: GitLab Test Migration to gitlab.com

## Overview

Remove every reference to the internal Gamania GitLab server
(`gitlab.gamania.com`) and the Gamania-side sandbox project
(`terrylin/agent-runtime-testing`) from this repo, and bootstrap a
gitlab.com sandbox the maintainer (`graysury`) can drive directly. Three
ordered sprints:

1. **Sprint 1 — Source fixture migration**: flip 13 `gitlab.gamania.com`
   strings and 3 doc-comment mentions in `forge-cli` / `plan-issue-cli`
   source files; decouple the `auth status` tests from a specific username.
2. **Sprint 2 — Docs sweep**: rewrite 7 historical plan + runbook docs so
   they point at `gitlab.com` and the new sandbox slug.
3. **Sprint 3 — Live sandbox**: create
   `graysury/nils-cli-gitlab-sandbox` on gitlab.com (private), then run a
   live `forge-cli` GitLab-arm sweep to verify the migration end-to-end.

No CLI behavior, schema, or env-var changes. No new test gating.

## Read First

- Primary source:
  `docs/plans/gitlab-com-test-migration/gitlab-com-test-migration-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Open questions carried into execution: none
- Worktree: `.claude/worktrees/gitlab-com-test-migration` on branch
  `worktree-gitlab-com-test-migration`
- gitlab.com auth: `glab auth status --hostname gitlab.com` →
  logged in as `graysury` (verified 2026-05-25)

## Scope

- In scope:
  - 3 source files: `crates/forge-cli/src/ops/issue_view.rs`,
    `crates/forge-cli/src/ops/pr_comments.rs`,
    `crates/plan-issue-cli/src/provider.rs`.
  - 2 auth-status test files (integration + unit) + 2 incidental stub
    stderr files (`pr_deliver_chain.rs`, `exit_codes.rs`) reviewed for
    consistency.
  - 7 doc files: runbook + 6 plan docs.
  - Creating the live sandbox project on gitlab.com.
  - Live `forge-cli` sweep against the new sandbox.
- Out of scope:
  - Provider-detection or host-classification code changes.
  - Adding `FORGE_CLI_E2E=1` wiring.
  - Adding any new automated tests that hit the network.
  - Touching the existing `gitlab.gamania.com` auth profile in
    `glab-cli/config.yml`.
  - Migrating issues/MRs out of `terrylin/agent-runtime-testing`.

## Sprint 1: Source fixture migration

**Goal**: Every `crates/**/*.rs` file is free of `gitlab.gamania.com`,
`terrylin/agent-runtime-testing`, `graysurf`, and `graysury`. Tests still
pass. Auth-status tests no longer assert a specific username.

**Demo / Validation**:

- Commands:
  - `rg -n 'gitlab\.gamania\.com|terrylin/agent-runtime-testing' crates/`
    returns no matches.
  - `rg -n 'graysurf|graysury' crates/forge-cli/` returns matches only in
    files unrelated to `auth_status` / `pr_deliver_chain` / `exit_codes`
    (the agent-runtime-cli crate legitimately references the GitHub
    `graysurf/agent-runtime-kit` project).
  - `cargo test -p forge-cli` and `cargo test -p plan-issue-cli` pass.
- Verify: stub-binary integration tests still execute end-to-end; parser
  unit tests still demonstrate the user / host fields get extracted.

### Task 1.1: Swap `gitlab.gamania.com` in `forge-cli/src/ops/issue_view.rs`

- **Location**:
  - `crates/forge-cli/src/ops/issue_view.rs`
- **Description**: Replace 8 `gitlab.gamania.com` URL occurrences and the
  Gamania-flavored doc comments at lines 141 and 409 with `gitlab.com`.
  Replace `terrylin/agent-runtime-testing` with `graysury/nils-cli-gitlab-sandbox`
  in the same fixtures. The `Some("gitlab.gamania.com")` expected value at
  line 718 becomes `Some("gitlab.com")`. The adjacent regression test for
  the generic `gitlab.com/group/sub/project` case stays untouched.
- **Dependencies**: none
- **Complexity**: 1
- **Acceptance criteria**:
  - File contains zero `gitlab.gamania.com` / `terrylin/agent-runtime-testing`
    occurrences.
  - `cargo test -p forge-cli --lib ops::issue_view` is green.
  - `gitlab_host_from_url` / `gitlab_project_path_from_url` still receive
    real inputs (i.e., we did not collapse two URLs into one identical
    string).
- **Validation**:
  - `cargo test -p forge-cli --lib ops::issue_view`

### Task 1.2: Swap `gitlab.gamania.com` in `forge-cli/src/ops/pr_comments.rs`

- **Location**:
  - `crates/forge-cli/src/ops/pr_comments.rs`
- **Description**: Replace the 3 `gitlab.gamania.com` MR URL occurrences
  (lines 443 / 501 / 508) with `gitlab.com`, and the
  `terrylin/agent-runtime-testing` slug with
  `graysury/nils-cli-gitlab-sandbox`. The `Some("terrylin/agent-runtime-testing")`
  expected value at line 446 becomes `Some("graysury/nils-cli-gitlab-sandbox")`.
- **Dependencies**: none
- **Complexity**: 1
- **Acceptance criteria**:
  - File contains zero `gitlab.gamania.com` / `terrylin/agent-runtime-testing`
    occurrences.
  - `cargo test -p forge-cli --lib ops::pr_comments` is green.
- **Validation**:
  - `cargo test -p forge-cli --lib ops::pr_comments`

### Task 1.3: Swap `gitlab.gamania.com` in `plan-issue-cli/src/provider.rs`

- **Location**:
  - `crates/plan-issue-cli/src/provider.rs`
- **Description**: Replace 5 fixture occurrences (lines 317 / 331 / 334 /
  361 / 363) and the doc comment at line 57 with `gitlab.com` and
  `graysury/nils-cli-gitlab-sandbox`. The `classify_host("gitlab.gamania.com")`
  test asserts that a non-`gitlab.com` host classifies as `Provider::GitLab`;
  rewrite as `classify_host("gitlab.com")` or keep a single
  `classify_host("gitlab.example.com")` assertion if you want to preserve
  the wildcard-host coverage (allowed but not required by this plan).
- **Dependencies**: none
- **Complexity**: 1
- **Acceptance criteria**:
  - File contains zero `gitlab.gamania.com` / `terrylin/agent-runtime-testing`
    occurrences.
  - `cargo test -p plan-issue-cli --lib provider` is green.
  - `classify_host` still has at least one assertion that returns
    `Some(Provider::GitLab)` for an input string starting with `gitlab.`.
- **Validation**:
  - `cargo test -p plan-issue-cli --lib provider`

### Task 1.4: Decouple `auth status` tests from a specific username

- **Location**:
  - `crates/forge-cli/tests/integration/auth_status.rs`
  - `crates/forge-cli/src/ops/auth_status.rs` (unit tests at lines 234–263)
- **Description**: Two pieces:
  1. **Integration tests** (`tests/integration/auth_status.rs`): remove the
     `data.user == "graysurf"` assertions at lines 41 and 57, and remove
     the cross-backend user comparison at line 81 (
     `gh_env["data"]["user"] == glab_env["data"]["user"]`). The flow check
     (status code, schema_version, ok, provider, host, scopes count) is
     preserved. The stub stderr lines 10 / 17 keep a placeholder username
     (e.g., `testuser-gh` / `testuser-glab`) so the parser still receives a
     non-trivial input.
  2. **Unit tests** (`src/ops/auth_status.rs`): keep the parser-extracts-user
     coverage by replacing `Some("graysurf")` with a stable
     `Some("testuser-gh")` / `Some("testuser-glab")` and updating the
     corresponding stub stderr strings. The point is to keep proving that
     "the user string in stderr ends up in `payload.user`" without pinning
     a real account name.
- **Dependencies**: none
- **Complexity**: 2
- **Acceptance criteria**:
  - No assertion in either file pins `graysurf` or `graysury`.
  - `cargo test -p forge-cli --lib ops::auth_status` is green.
  - `cargo test -p forge-cli --test '*' auth_status` is green.
  - Stub stderr strings still produce a non-empty `payload.user` so the
    parser branch coverage is preserved.
- **Validation**:
  - `cargo test -p forge-cli auth_status`

### Task 1.5: Review incidental stub stderr in `pr_deliver_chain.rs` / `exit_codes.rs`

- **Location**:
  - `crates/forge-cli/tests/integration/pr_deliver_chain.rs:153`
  - `crates/forge-cli/tests/integration/exit_codes.rs:18`
- **Description**: Both files include a stub `gh auth status` stderr line
  that names `graysurf`. Neither file asserts on that value. To stay
  consistent with Task 1.4's decoupling, change the placeholder string to
  `testuser-gh` (or whichever stable placeholder Task 1.4 chose). No
  behavior change.
- **Dependencies**:
  - Task 1.4 (alignment on placeholder name)
- **Complexity**: 1
- **Acceptance criteria**:
  - Stub stderr lines use the chosen placeholder name.
  - `cargo test -p forge-cli` is green.
- **Validation**:
  - `cargo test -p forge-cli`

### Task 1.6: Sprint 1 audit + workspace test

- **Location**:
  - Worktree root
- **Description**: Run the rg audits and the full workspace test suite.
- **Dependencies**:
  - Task 1.1
  - Task 1.2
  - Task 1.3
  - Task 1.4
  - Task 1.5
- **Complexity**: 1
- **Acceptance criteria**:
  - `rg -n 'gitlab\.gamania\.com|terrylin/agent-runtime-testing' crates/`
    returns no matches.
  - `cargo test --workspace` is green.
- **Validation**:
  - `rg -n 'gitlab\.gamania\.com|terrylin/agent-runtime-testing' crates/`
  - `cargo test --workspace`

## Sprint 2: Docs sweep

**Goal**: Rewrite the 7 doc files so the only remaining mention of
`gitlab.gamania.com` or `terrylin/agent-runtime-testing` anywhere in the
worktree is in git history.

**Demo / Validation**:

- Commands:
  - `rg -n 'gitlab\.gamania\.com|terrylin/agent-runtime-testing'` against
    the whole worktree returns no matches in tracked files.
- Verify: cross-file pointers between plan docs still resolve.

### Task 2.1: Rewrite `provider-routing-runbook.md`

- **Location**:
  - `crates/plan-issue-cli/docs/runbooks/provider-routing-runbook.md`
- **Description**: This runbook is the durable post-promotion reference for
  provider routing. Replace `gitlab.gamania.com` with `gitlab.com` and
  `terrylin/agent-runtime-testing` with `graysury/nils-cli-gitlab-sandbox`
  in body text and examples. Where the runbook discusses self-hosted GitLab
  hosts conceptually, soften the example from "like `gitlab.gamania.com`"
  to "like `gitlab.example.com`" so the public-doc tone stays neutral.
- **Dependencies**: none
- **Complexity**: 1
- **Acceptance criteria**:
  - File contains zero `gitlab.gamania.com` / `terrylin/agent-runtime-testing`.
  - Cross-references inside the file still resolve to existing files /
    sections.
- **Validation**:
  - `rg -n 'gitlab\.gamania\.com|terrylin/agent-runtime-testing'
    crates/plan-issue-cli/docs/runbooks/provider-routing-runbook.md`

### Task 2.2: Rewrite `docs/plans/gitlab-mr-unblock/*.md`

- **Location**:
  - `docs/plans/gitlab-mr-unblock/gitlab-mr-unblock-discussion-source.md`
  - `docs/plans/gitlab-mr-unblock/gitlab-mr-unblock-plan.md`
- **Description**: Update the Source / Read-first / Sprint references that
  point at the Gamania sandbox. Leave the historical narrative ("sandbox
  sweep against …") intact, but rename the host and slug. Append a brief
  note that the sandbox moved to `graysury/nils-cli-gitlab-sandbox` on
  gitlab.com.
- **Dependencies**: none
- **Complexity**: 2
- **Acceptance criteria**:
  - Both files contain zero `gitlab.gamania.com` / `terrylin/agent-runtime-testing`.
  - Each file still reads as a coherent record of the original effort.
- **Validation**:
  - `rg -n 'gitlab\.gamania\.com|terrylin/agent-runtime-testing'
    docs/plans/gitlab-mr-unblock/`

### Task 2.3: Rewrite `docs/plans/plan-issue-cli-provider-abstraction/*.md`

- **Location**:
  - `docs/plans/plan-issue-cli-provider-abstraction/plan-issue-cli-provider-abstraction-discussion-source.md`
  - `docs/plans/plan-issue-cli-provider-abstraction/plan-issue-cli-provider-abstraction-plan.md`
  - `docs/plans/plan-issue-cli-provider-abstraction/plan-issue-cli-provider-abstraction-design-note.md`
- **Description**: Same approach as Task 2.2. Take care with example shell
  commands that paste a full `plan-issue --repo …` invocation — replace
  both host and slug so the example would actually run against the new
  sandbox.
- **Dependencies**: none
- **Complexity**: 2
- **Acceptance criteria**:
  - All three files contain zero `gitlab.gamania.com` / `terrylin/agent-runtime-testing`.
  - Example commands stay runnable against the new sandbox.
- **Validation**:
  - `rg -n 'gitlab\.gamania\.com|terrylin/agent-runtime-testing'
    docs/plans/plan-issue-cli-provider-abstraction/`

### Task 2.4: Rewrite `docs/plans/forge-cli-inbox*/*.md`

- **Location**:
  - `docs/plans/forge-cli-inbox/forge-cli-inbox-plan.md`
  - `docs/plans/forge-cli-inbox/forge-cli-inbox-discussion-source.md`
  - `docs/plans/forge-cli-inbox-latency/forge-cli-inbox-latency-plan.md`
  - `docs/plans/forge-cli-inbox-latency/forge-cli-inbox-latency-discussion-source.md`
- **Description**: Same as Task 2.2 / 2.3. The inbox docs mention the
  Gamania host in passing (e.g., "live probe examples"). Update host and
  slug, soften any "internal-only" framing to "GitLab instance".
- **Dependencies**: none
- **Complexity**: 1
- **Acceptance criteria**:
  - All four files contain zero `gitlab.gamania.com` / `terrylin/agent-runtime-testing`.
- **Validation**:
  - `rg -n 'gitlab\.gamania\.com|terrylin/agent-runtime-testing'
    docs/plans/forge-cli-inbox/ docs/plans/forge-cli-inbox-latency/`

### Task 2.5: Sprint 2 audit + local-fast CI

- **Location**:
  - Worktree root
- **Description**: Run a worktree-wide audit and the local-fast CI gate so
  the docs sweep does not regress markdown / docs-hygiene checks.
- **Dependencies**:
  - Task 2.1
  - Task 2.2
  - Task 2.3
  - Task 2.4
- **Complexity**: 1
- **Acceptance criteria**:
  - `rg -n 'gitlab\.gamania\.com|terrylin/agent-runtime-testing'` returns
    no matches in tracked files.
  - `scripts/ci/nils-cli-local-fast.sh` (or its DEVELOPMENT.md equivalent
    sequence) passes.
- **Validation**:
  - `rg -n 'gitlab\.gamania\.com|terrylin/agent-runtime-testing'`
  - `scripts/ci/nils-cli-local-fast.sh`

## Sprint 3: Live sandbox bootstrap + sweep

**Goal**: Create the new gitlab.com sandbox and prove the GitLab arm of
`forge-cli` works against it end-to-end, so the rewritten docs and tests
reflect a real, reachable target.

**Demo / Validation**:

- Commands:
  - `glab --hostname gitlab.com repo create graysury/nils-cli-gitlab-sandbox
    --visibility private --description "Live sandbox for nils-cli forge-cli GitLab arm validation"`.
  - `forge-cli --format json --repo graysury/nils-cli-gitlab-sandbox issue list
    --state all` returns `ok=true`.
  - Open a disposable MR via the `pr/deliver-gitlab-mr` skill (or the
    underlying `forge-cli pr create … --no-merge`) and confirm
    `ok=true` envelope at the create step.
- Verify: live sweep evidence saved into the worktree and referenced in
  the PR description.

### Task 3.1: Create `graysury/nils-cli-gitlab-sandbox`

- **Location**:
  - gitlab.com (external resource)
- **Description**: Use `glab --hostname gitlab.com repo create` to
  bootstrap the project as private with a README. Default branch `main`.
  No CI templates. Capture the project ID and HTTPS URL into Sprint 3
  evidence.
- **Dependencies**: none
- **Complexity**: 1
- **Acceptance criteria**:
  - Project visible at
    `https://gitlab.com/graysury/nils-cli-gitlab-sandbox`.
  - `glab --hostname gitlab.com repo view graysury/nils-cli-gitlab-sandbox`
    succeeds.
- **Validation**:
  - `glab --hostname gitlab.com repo view graysury/nils-cli-gitlab-sandbox`

### Task 3.2: Live `forge-cli` GitLab-arm sweep

- **Location**:
  - Worktree root + sandbox project
- **Description**: With the rebuilt `forge-cli` binary, exercise:
  - `forge-cli --repo graysury/nils-cli-gitlab-sandbox issue list --state all`
  - `forge-cli --repo graysury/nils-cli-gitlab-sandbox auth status`
  - One disposable MR through `pr create` (no merge), then `pr view`, then
    `pr close`.
  - Capture each envelope (`--format json`) into
    `evidence/sprint-3-sweep/` inside the worktree (or use
    `nils-cli agent-out project --topic gitlab-com-test-migration --mkdir`
    if the project-specific output dir is preferred).
  - Note: the `evidence/sprint-3-sweep/` directory was removed post-closeout;
    the original captured envelopes are preserved in the PR #521 diff.
- **Dependencies**:
  - Task 3.1
  - Task 1.6 (so source fixtures point at the new sandbox)
  - Task 2.5 (so docs point at the new sandbox)
- **Complexity**: 2
- **Acceptance criteria**:
  - Every captured envelope has `ok=true` for the expected step.
  - No envelope contains `error.kind = glab_version_unsupported` or
    `error.kind = repo_not_found`.
  - Evidence files committed (or attached to the PR description).
- **Validation**:
  - Manual review of captured envelopes.

### Task 3.3: Final consolidation + PR readiness

- **Location**:
  - Worktree root
- **Description**: Final audit, run the relevant DEVELOPMENT.md gates,
  draft the PR title / body referencing the discussion source and
  evidence. No code or docs change in this task — it is the close-out.
- **Dependencies**:
  - Task 3.1
  - Task 3.2
- **Complexity**: 1
- **Acceptance criteria**:
  - `cargo test --workspace` is green on the final commit.
  - `scripts/ci/nils-cli-local-fast.sh` (or DEVELOPMENT.md equivalent)
    passes on the final commit.
  - `rg -n 'gitlab\.gamania\.com|terrylin/agent-runtime-testing'` returns
    zero matches in tracked files.
  - PR description links the discussion source, this plan, and the
    Sprint 3 evidence.
- **Validation**:
  - `cargo test --workspace`
  - `scripts/ci/nils-cli-local-fast.sh`
  - `rg -n 'gitlab\.gamania\.com|terrylin/agent-runtime-testing'`
