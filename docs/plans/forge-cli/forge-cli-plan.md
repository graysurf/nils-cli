# Plan: forge-cli v1 — Provider-Neutral Forge Operations

## Overview

Add a new workspace binary `forge-cli` that fronts every remote forge
operation (PR/MR lifecycle, Issue lifecycle, CI wait) today executed by
agent-kit skills as raw `gh` / `glab` calls. Two backends ship in
lock-step: GitHub (wraps `gh`) and GitLab (wraps `glab`). The binary
adopts `cli-output-contract-v1` from day one, enforces branch / body /
state policy at the type level, and exposes one macro (`pr deliver`)
that composes the agent-kit "open draft → wait CI → ready → merge"
flow in Rust. Sprint 0 lands the spec + this plan as a docs-only PR;
Sprints 1–8 then ship the crate atom-first, macro-second, with the
final sprint wiring the brew wrapper and cutting the `nils-cli` minor
release.

## Read First

- Primary source: docs/plans/forge-cli/forge-cli-discussion-source.md
- Source type: discussion-to-implementation-doc
- Companion sources (authoritative for contract / catalog / envelope):
  - crates/forge-cli/docs/specs/forge-cli-spec-v1.md
  - crates/forge-cli/docs/specs/forge-cli-ops-v1.yaml
  - docs/specs/cli-output-contract-v1.md
- Open questions carried into execution:
  - Wrapper + Homebrew tap formula land in this v1 (Sprint 8).
  - `glab ci status` text parser pinned to current installed `glab`
    minor; out-of-range versions trigger `UNAVAILABLE 69` with a
    "please upgrade/downgrade" hint, not a guessed parse.
  - `nils-cli` minor bump cut once Sprint 8 passes the acceptance gate,
    via `nils-cli-bump-version-tag-release` + tap formula bump.

## Scope

- In scope (v1):
  - New crate `crates/forge-cli/` (`nils-forge-cli` package, `forge-cli`
    binary) using `clap`, `serde`, `serde_json`, and the existing
    `nils-common::cli_contract` primitives.
  - Atoms: `auth status`, `repo view`, `pr create`, `pr view`,
    `pr list`, `pr edit`, `pr comment`, `pr ready`, `pr merge`,
    `pr close`, `pr checks`, `pr wait-checks`, `issue create`,
    `issue view`, `issue edit`, `issue comment`, `issue close`,
    `issue reopen`.
  - Macro: `pr deliver --kind feature|bug`.
  - Provider detection from `git remote get-url <--remote>` host plus
    `gh auth status` / `glab auth status` host match.
  - Lock-down validations (branch naming, body schema, title length,
    worktree clean, push state, default-branch protection, draft-merge
    refusal, required-check gating at merge time, merge method support,
    keep-branch conflict).
  - `--dry-run` rendering the constructed backend argv under
    `data.plan` for every op.
  - Per-repo `.forge-cli.toml` (merge method, body headings, checks
    timeout) and env overrides `FORGE_CLI_GH_BIN`, `FORGE_CLI_GLAB_BIN`,
    `FORGE_CLI_DEFAULT_PROVIDER`.
  - Tests: per-op fixture pair (gh + glab), parity harness, exit-code
    matrix, dry-run smoke. Token-shaped strings redacted in every
    fixture.
  - Wrapper `wrappers/forge-cli`, completions
    `completions/_forge-cli` + `completions/forge-cli.bash`.
  - Homebrew tap formula bump and `nils-cli` minor release at the end.

- Out of scope (v1 — explicitly deferred to v2 or later):
  - Release management (`gh release`, GitLab releases).
  - Label management.
  - Raw `gh api` / GitLab REST passthrough.
  - Issue *macros* (`issue deliver`, `issue close-when-prs-merged`,
    `issue cross-link`).
  - Repo creation, branch protection management, code review state
    mutation beyond `pr ready`.
  - Gitea / Forgejo backend.
  - agent-kit skill migration (happens after this v1 ships and is
    accepted; tracked separately).
  - Local-git operations beyond what is already in `git-cli` / shell
    wrappers (the spec keeps `git push`, local branch deletion, etc.
    outside `forge-cli`).

## Assumptions

1. `nils-common::cli_contract` already exposes `OutputFormat`,
   `Envelope<T>`, `EnvelopeError`, `exit::{SUCCESS, RUNTIME, USAGE,
   DATA, UNAVAILABLE, SOFTWARE}`, and `emit_parse_error`. `forge-cli`
   consumes them unchanged; the contract migration on existing binaries
   (`fdaf5d6`) shipped these primitives.
2. Workspace conventions for new crates are documented in
   `docs/runbooks/new-cli-crate-development-standard.md` and exemplified
   by `crates/git-cli/` (lib + bin, `nils-*` package name, edition +
   license inherited from workspace `Cargo.toml`). `forge-cli` mirrors
   that shape.
3. The local environment has both `gh` and `glab` installed and
   authenticated. CI fixtures replace the binaries with controlled
   stubs (via `FORGE_CLI_GH_BIN` / `FORGE_CLI_GLAB_BIN`) so no live
   network access is required for the default test gate.
4. `gh pr create --json` and `glab mr view -F json` outputs are stable
   across the installed minor releases on developer machines and CI.
   Backend stdout shape mismatches surface as `SOFTWARE 70`, not silent
   coercion.
5. Per workspace policy, `NILS_CLI_TEST_RUNNER=nextest bash
   scripts/ci/nils-cli-checks-entrypoint.sh` is the canonical local
   gate. `scripts/ci/cli-output-contract-lint.sh` is part of that gate
   and catches `--json` boolean / numeric exit literal regressions.
6. Each sprint lands as its own GitHub PR cut from `main` (Sprint 0
   first, others rebased onto `main` after Sprint 0 merges). PRs go
   through `create-feature-pr` → `close-feature-pr`; commits go through
   `semantic-commit`. Direct `git commit` and `gh pr create` are
   blocked by hook.

## Sprint 1: Crate scaffold + global flags + provider detection + read-only atoms

**Goal**: Land an empty but correctly-shaped `nils-forge-cli` crate
that already exposes the canonical command tree, parses every global
flag, detects the provider, plumbs `--dry-run`, and implements the two
read-only atoms (`auth status`, `repo view`) end to end on both
backends. Nothing else in the binary mutates remote state yet, so this
sprint is the safest place to debug provider detection, envelope
plumbing, and the subprocess wrapper layer.

**Demo/Validation**:

- Commands:
  - `cargo build -p nils-forge-cli`
  - `cargo test -p nils-forge-cli`
  - `forge-cli --help` (lists `pr`, `issue`, `repo`, `auth`,
    `completion`; lists `--format`, `--remote`, `--provider`, `--repo`,
    `--dry-run`).
  - `forge-cli auth status --format json` (envelope with
    `schema_version: cli.forge-cli.auth.status.v1`).
  - `forge-cli repo view --format json` (envelope with normalized
    `owner`, `name`, `default_branch`, `merge_methods_allowed`).
  - `forge-cli auth status --provider github --dry-run --format json`
    (envelope carries `data.plan = ["gh", "auth", "status"]`, no
    subprocess invoked).
- Verify: `forge-cli` returns `64` on unknown provider host, `69` when
  `gh`/`glab` is missing (force via
  `FORGE_CLI_GH_BIN=/bin/false`), `0` on success; envelope is
  snake_case; no inline numeric exit literals appear under
  `crates/forge-cli/src/`.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 1.1: Scaffold `crates/forge-cli/` package and workspace wiring

- **Location**:
  - `crates/forge-cli/Cargo.toml` (new)
  - `crates/forge-cli/src/main.rs` (new)
  - `crates/forge-cli/src/lib.rs` (new)
  - `crates/forge-cli/README.md` (new — minimal pointer to spec)
  - `Cargo.toml` (workspace `members`)
- **Description**: Create the `nils-forge-cli` package mirroring
  `crates/git-cli/Cargo.toml` (lib `forge_cli` + bin `forge-cli`,
  edition / license inherited, version pinned to current workspace
  version). Wire it into the workspace `members`. `main.rs` is a thin
  wrapper that calls `forge_cli::run()` and exits with its returned
  code. `README.md` is a one-paragraph pointer to
  `crates/forge-cli/docs/specs/forge-cli-spec-v1.md`.
- **Dependencies**:
  - none
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - `cargo build -p nils-forge-cli` succeeds.
  - `cargo test --workspace --no-run` succeeds (no dangling reference).
  - Package version matches the current workspace version and
    `nils-common` dependency is on the workspace path / version.
  - Crate appears in `cargo metadata --no-deps` output.
- **Validation**:
  - `cargo build -p nils-forge-cli`
  - `cargo metadata --no-deps --format-version 1 | jq '.packages[].name' | grep -F nils-forge-cli`

### Task 1.2: Define clap command tree + global flags

- **Location**:
  - `crates/forge-cli/src/cli.rs` (new)
  - `crates/forge-cli/src/lib.rs`
- **Description**: Build the clap derive tree exactly matching the
  spec's command topology (`pr`, `issue`, `repo`, `auth`, `completion`
  parents with all v1 children declared, even if their handlers are
  `todo!()` stubs in this sprint). Wire global flags:
  `--format` (enum: text or json),
  `--remote <name>` (default `origin`),
  `--provider` (enum: github or gitlab; optional, auto-detected when
  absent),
  `--repo <owner/name>` (optional),
  `--dry-run` (bool, default false).
  No `--json` boolean alias.
- **Dependencies**:
  - Task 1.1
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - `forge-cli --help` lists every subcommand listed in spec §"Command
    tree".
  - `forge-cli pr --help`, `forge-cli issue --help`, etc. list every
    expected child subcommand.
  - Global flags are present on every subcommand (verified by snapshot
    of `forge-cli pr create --help`).
  - No `--json` flag appears anywhere.
- **Validation**:
  - `cargo test -p nils-forge-cli cli::help`
  - Manual: `cargo run -p nils-forge-cli -- --help`

### Task 1.3: Implement provider detection

- **Location**:
  - `crates/forge-cli/src/provider.rs` (new)
  - `crates/forge-cli/tests/integration/provider.rs` (new)
- **Description**: Implement the spec's detection ladder: explicit
  `--provider` → `git remote get-url <--remote>` host parse → cached
  `gh auth status` / `glab auth status` host match → otherwise
  `USAGE 64` with `error.kind = "provider_unsupported"`. URL parser
  accepts `https://`, `ssh://`, `git@host:owner/repo.git`, and the
  enterprise-host variants (`*.ghe.com`, internal GitLab hostnames).
  Auth status calls are memoised per `forge-cli` invocation (one call
  per provider, in `OnceCell` or equivalent). Provider lookup result is
  carried in a `ProviderContext` that downstream ops accept.
- **Dependencies**:
  - Task 1.2
- **Complexity**:
  - 6
- **Acceptance criteria**:
  - Detection table covers `github.com`, `gitlab.com`,
    `*.gitlab.<corp>`, `git@github.com:<owner>/<repo>.git`,
    `ssh://git@gitlab.com/<owner>/<repo>.git`, and rejects an
    unknown host with the documented error kind.
  - Auth fallback is exercised by a test that mocks `gh auth status`
    via `FORGE_CLI_GH_BIN`.
  - Repeat calls within a run hit the cache (assert via call counter
    in a test stub).
- **Validation**:
  - `cargo test -p nils-forge-cli provider`
  - Manual: `forge-cli auth status --provider github` from a non-git
    directory (succeeds; remote URL not consulted).

### Task 1.4: Subprocess wrapper + envelope serializer + dry-run plumbing

- **Location**:
  - `crates/forge-cli/src/backend.rs` (new)
  - `crates/forge-cli/src/envelope.rs` (new)
  - `crates/forge-cli/src/error.rs` (new)
  - `crates/forge-cli/tests/integration/backend.rs` (new)
- **Description**: Implement the canonical "run backend subprocess,
  parse JSON, wrap in envelope" loop in one place. Inputs: a typed
  argv vector + an expected response shape. Outputs: an
  `nils_common::cli_contract::Envelope<T>` on success or the typed
  `ForgeError` on failure. Stderr is captured, tail-trimmed to 2 KiB,
  token-redacted (regex matching `gh[ps]_*`, `glpat-*`, `ghr_*`,
  `gho_*`), and placed under `data.error.detail` when the call fails.
  `--dry-run` short-circuits before subprocess invocation and instead
  returns `Envelope { ok: true, data: { plan: argv, provider } }`. Exit
  codes are routed exclusively through `nils_common::cli_contract::exit`
  constants; no numeric literals in this module.
- **Dependencies**:
  - Task 1.3
- **Complexity**:
  - 7
- **Acceptance criteria**:
  - `--dry-run` produces a stable `data.plan` array and never invokes
    `gh` / `glab`.
  - Missing backend binary (`FORGE_CLI_GH_BIN=/bin/false`) maps to
    `UNAVAILABLE 69` with `error.kind = "backend_missing"`.
  - Backend non-zero exit propagates as `RUNTIME 1` plus
    `error.kind = "backend_error"` and a redacted stderr tail.
  - Token-shaped strings in fixture stderr are replaced with
    `<redacted-token>` before emission.
  - Subprocess invocation goes through a single audited code path
    (unit test asserts only one `Command::new` site).
- **Validation**:
  - `cargo test -p nils-forge-cli backend`
  - `bash scripts/ci/cli-output-contract-lint.sh`

### Task 1.5: Implement `auth status` atom

- **Location**:
  - `crates/forge-cli/src/ops/auth_status.rs` (new)
  - `crates/forge-cli/src/ops/mod.rs`
  - `crates/forge-cli/tests/fixtures/github/auth_status/` (new)
  - `crates/forge-cli/tests/fixtures/gitlab/auth_status/` (new)
  - `crates/forge-cli/tests/integration/auth_status.rs` (new)
- **Description**: Implement the spec's `auth.status` atom. Backend
  stdout is text (both `gh auth status` and `glab auth status` are
  text-only), parsed into `{ provider, host, user?, scopes }` via a
  small per-backend parser. Empty `scopes` is allowed. Envelope
  schema literal: `cli.forge-cli.auth.status.v1`.
- **Dependencies**:
  - Task 1.4
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - Both backends parse to byte-identical envelopes except for
    `data.provider` and `data.host`.
  - Fixture pair lands under
    `tests/fixtures/{github,gitlab}/auth_status/{stdout,stderr,exit}`.
  - Token-shaped strings in fixtures are pre-redacted to
    `<redacted-token>`.
- **Validation**:
  - `cargo test -p nils-forge-cli auth_status`

### Task 1.6: Implement `repo view` atom

- **Location**:
  - `crates/forge-cli/src/ops/repo_view.rs` (new)
  - `crates/forge-cli/tests/fixtures/github/repo_view/` (new)
  - `crates/forge-cli/tests/fixtures/gitlab/repo_view/` (new)
  - `crates/forge-cli/tests/integration/repo_view.rs` (new)
- **Description**: Implement the spec's `repo.view` atom. `gh repo
  view --json …` JSON is mapped to the canonical
  `{ owner, name, url, default_branch, merge_methods_allowed }` shape
  in snake_case. `glab repo view -F json` is mapped through a
  per-backend deserializer that handles its slightly different field
  names (`default_branch` vs `defaultBranchRef.name`,
  `merge_methods_allowed` derived from the boolean trio for GitHub /
  the corresponding GitLab fields). Schema literal:
  `cli.forge-cli.repo.view.v1`.
- **Dependencies**:
  - Task 1.4
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - Output envelope is byte-identical between backends except for
    `data.url` host and `data.provider`.
  - `merge_methods_allowed` enumerates exactly the values the backend
    reports (`squash`, `merge`, `rebase` subset).
  - Test pair covers a repo with all three methods enabled and a repo
    with only `squash`.
- **Validation**:
  - `cargo test -p nils-forge-cli repo_view`

### Task 1.7: Sprint 1 exit-code matrix + workspace gate

- **Location**:
  - `crates/forge-cli/tests/integration/exit_codes.rs` (new)
  - `scripts/ci/cli-output-contract-lint.sh` (allowlist update if any)
- **Description**: Add the binary's first exit-code matrix covering
  `SUCCESS` (auth status with stubbed `gh`), `USAGE` (unknown
  subcommand), `DATA` (parse failure on a deliberately mangled
  `.forge-cli.toml`), `UNAVAILABLE` (missing backend via
  `FORGE_CLI_GH_BIN=/bin/false`), and `SOFTWARE` (mangled fixture
  JSON). `RUNTIME` is deferred to Sprint 3 where check failures
  naturally produce it. Ensure
  `bash scripts/ci/cli-output-contract-lint.sh` passes against the
  scaffolded crate.
- **Dependencies**:
  - Task 1.5
  - Task 1.6
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - Five tests cover five exit-code constants; each test asserts the
    literal constant from `nils_common::cli_contract::exit`, not the
    numeric value.
  - `scripts/ci/cli-output-contract-lint.sh` passes.
- **Validation**:
  - `cargo test -p nils-forge-cli exit_codes`
  - `bash scripts/ci/cli-output-contract-lint.sh`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh`

## Sprint 2: PR atoms (create / view / list / edit / comment / ready / close)

**Goal**: Land every PR/MR lifecycle atom except `merge` and the
check atoms. `pr create` carries the heaviest validation surface
(branch / title / body / worktree / push) and is the prerequisite for
the macro; the read/append atoms are mostly thin parse-and-render
layers on top of Sprint 1's subprocess wrapper.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-forge-cli pr_`
  - `forge-cli pr create --kind feature --title "demo: test" --body-file <(printf '## Summary\nx\n\n## Test plan\ny\n') --dry-run --format json`
  - `forge-cli pr view 1 --dry-run --format json`
  - `forge-cli pr list --dry-run --format json`
- Verify: every PR atom emits a snake_case envelope with the spec's
  schema literal; `pr create` refuses dirty worktree, unpushed HEAD,
  branch/kind mismatch, body without `## Summary` or `## Test plan`,
  and title > 70 chars — each rejection maps to `DATA 65` with the
  documented `error.kind`.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 2.1: PR body / title / branch validation module

- **Location**:
  - `crates/forge-cli/src/validations.rs` (new)
  - `crates/forge-cli/tests/integration/validations.rs` (new)
- **Description**: Implement the validation rules from
  `forge-cli-ops-v1.yaml::validations_catalog` that PR ops will share:
  `branch_name`, `branch_kind_matches`, `title_length`, `body_summary`,
  `body_test_plan`, `worktree_clean`, `head_pushed`. Each returns a
  typed result mapping to one of the `error.kind` values listed in
  spec §"Lock-down policy". The body parser MUST treat any non-empty
  text under the configured H2 heading (default `## Summary` /
  `## Test plan`, overridable via `.forge-cli.toml`) as the section
  body; the H2 line itself must not count as content.
- **Dependencies**:
  - Task 1.7
- **Complexity**:
  - 6
- **Acceptance criteria**:
  - Every rule has a positive + negative unit test.
  - Body parser correctly rejects "section present but empty" and
    "section absent", with distinct `error.kind` values
    (`body_missing_summary` for absent, also for empty; spec maps both
    to the same kind because the user-visible failure is identical).
  - Branch / kind mismatch produces `branch_kind_mismatch`, not
    `branch_name_invalid`.
- **Validation**:
  - `cargo test -p nils-forge-cli validations`

### Task 2.2: `pr create` atom

- **Location**:
  - `crates/forge-cli/src/ops/pr_create.rs` (new)
  - `crates/forge-cli/tests/fixtures/github/pr_create/` (new)
  - `crates/forge-cli/tests/fixtures/gitlab/pr_create/` (new)
  - `crates/forge-cli/tests/integration/pr_create.rs` (new)
- **Description**: Implement `pr create` per spec §"pr create" and
  the ops YAML's argv template. Use Task 2.1's validator chain.
  `gh pr create` does not return JSON; after creation, the
  implementation MUST call `gh pr view --json …` to fetch the new
  PR's metadata and produce the envelope. For GitLab, `glab mr
  create` returns the URL on stdout; the implementation does an
  immediate `glab mr view -F json` follow-up using the parsed `iid`.
  Body content may come from `--body` or `--body-file`; passing both
  is a `USAGE 64` parse error.
- **Dependencies**:
  - Task 2.1
- **Complexity**:
  - 8
- **Acceptance criteria**:
  - Output envelope: `cli.forge-cli.pr.create.v1` with `data = {
    number, url, head, base, draft, title, kind, provider }`.
  - Validation failure paths each emit `DATA 65` with the right
    `error.kind` (covered in matrix test).
  - `--draft` defaults to `true`.
  - Reviewer/label flags pass through to the backend argv when
    supplied; no-op otherwise.
- **Validation**:
  - `cargo test -p nils-forge-cli pr_create`

### Task 2.3: `pr view`, `pr list`, `pr close` atoms

- **Location**:
  - `crates/forge-cli/src/ops/pr_view.rs` (new)
  - `crates/forge-cli/src/ops/pr_list.rs` (new)
  - `crates/forge-cli/src/ops/pr_close.rs` (new)
  - `crates/forge-cli/tests/fixtures/github/pr_view/` (new)
  - `crates/forge-cli/tests/fixtures/gitlab/pr_view/` (new)
  - `crates/forge-cli/tests/fixtures/github/pr_list/` (new)
  - `crates/forge-cli/tests/fixtures/gitlab/pr_list/` (new)
  - `crates/forge-cli/tests/fixtures/github/pr_close/` (new)
  - `crates/forge-cli/tests/fixtures/gitlab/pr_close/` (new)
  - `crates/forge-cli/tests/integration/pr_view.rs` (new)
  - `crates/forge-cli/tests/integration/pr_list.rs` (new)
- **Description**: Implement three read/no-validation atoms.
  `pr view` accepts either a numeric id or a branch name (resolved
  via `gh pr view <branch>` / `glab mr list --source-branch`).
  `pr list` supports `--state`, `--author`, `--head`, `--limit`. All
  outputs go through the snake_case envelope with `data.state` mapped
  to the spec's `enum<open|closed|merged>` (GitLab's `opened` becomes
  `open`; `locked` becomes `closed`; everything else is a `SOFTWARE
  70`). `pr close` has no envelope payload beyond `{ number, url,
  state }`.
- **Dependencies**:
  - Task 2.2
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - State normalization is covered by paired fixtures (open / closed /
    merged on both backends).
  - `pr list --limit 1 --dry-run` produces a backend argv that
    includes the limit clamp.
  - `pr view <branch>` resolves to a numeric id via the documented
    fallback.
- **Validation**:
  - `cargo test -p nils-forge-cli pr_view pr_list pr_close`

### Task 2.4: `pr edit`, `pr comment`, `pr ready` atoms

- **Location**:
  - `crates/forge-cli/src/ops/pr_edit.rs` (new)
  - `crates/forge-cli/src/ops/pr_comment.rs` (new)
  - `crates/forge-cli/src/ops/pr_ready.rs` (new)
  - `crates/forge-cli/tests/fixtures/github/pr_edit/` (new)
  - `crates/forge-cli/tests/fixtures/gitlab/pr_edit/` (new)
  - `crates/forge-cli/tests/fixtures/github/pr_comment/` (new)
  - `crates/forge-cli/tests/fixtures/gitlab/pr_comment/` (new)
  - `crates/forge-cli/tests/fixtures/github/pr_ready/` (new)
  - `crates/forge-cli/tests/fixtures/gitlab/pr_ready/` (new)
  - `crates/forge-cli/tests/integration/pr_edit.rs` (new)
  - `crates/forge-cli/tests/integration/pr_comment.rs` (new)
  - `crates/forge-cli/tests/integration/pr_ready.rs` (new)
- **Description**: Implement the three mutating atoms with the
  validations declared in the ops YAML: `pr edit` re-runs
  `title_length` (when `--title` set) and the body checks (when
  `--body` / `--body-file` set); `pr comment` has no validation;
  `pr ready` runs `worktree_clean`. Argv construction follows the
  spec / YAML. After mutation, each op calls the backend's `view`
  command to fetch the fresh PR state and emits the canonical
  envelope.
- **Dependencies**:
  - Task 2.1
  - Task 2.2
- **Complexity**:
  - 6
- **Acceptance criteria**:
  - `pr edit --add-label x` and `--remove-label y` produce the right
    backend argv on both providers.
  - `pr ready` on a non-draft PR succeeds (idempotent — backend's own
    response is bubbled up).
  - `pr comment --body-file -` reads from stdin.
- **Validation**:
  - `cargo test -p nils-forge-cli pr_edit pr_comment pr_ready`

## Sprint 3: Check atoms (`pr checks` + `pr wait-checks`)

**Goal**: Implement the two check atoms. GitHub uses `gh pr checks
--json`; GitLab has no equivalent JSON output in the currently
installed `glab` minor, so the implementation pins the parser to that
exact minor and fails-fast on out-of-range versions with
`UNAVAILABLE 69` and a "please upgrade/downgrade glab" hint.
`pr wait-checks` reuses `pr.checks` schema, layering polling and
timeout semantics on top.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-forge-cli pr_checks pr_wait_checks`
  - `forge-cli pr checks 1 --dry-run --format json`
  - `forge-cli pr wait-checks 1 --interval 1s --timeout 5s --dry-run --format json`
- Verify: `state` value is one of the spec's normalized enum on both
  backends; `pr wait-checks` exit code matrix: required-all-success →
  `SUCCESS 0`, any-failure / cancelled / timed_out → `RUNTIME 1` with
  `error.kind = "checks_failed"`, deadline reached → `UNAVAILABLE 69`
  with `error.kind = "checks_timeout"`.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 3.1: `pr checks` GitHub backend

- **Location**:
  - `crates/forge-cli/src/ops/pr_checks.rs` (new)
  - `crates/forge-cli/tests/fixtures/github/pr_checks/` (new)
  - `crates/forge-cli/tests/integration/pr_checks_github.rs` (new)
- **Description**: Implement `pr checks` for GitHub by calling `gh pr
  checks <id> --json name,state,conclusion,bucket,workflow,link,
  startedAt,completedAt,description,isRequired`. Normalize the
  response into the canonical schema: required-only filtering
  (when `--required-only=true`, default), aggregate `state` derived
  per the spec's terminal-state mapping. Schema literal:
  `cli.forge-cli.pr.checks.v1`.
- **Dependencies**:
  - Task 2.4
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - Five fixtures cover all-success, mixed-failure, all-pending,
    cancelled, and empty-checks shapes.
  - `--required-only=false` includes optional checks under
    `data.checks` but they still feed the gating decision per the
    spec ("`--required-only=true` ignores non-required checks for the
    gating decision but still reports them in `data.checks`").
- **Validation**:
  - `cargo test -p nils-forge-cli pr_checks_github`

### Task 3.2: `pr checks` GitLab text parser with version fail-fast

- **Location**:
  - `crates/forge-cli/src/ops/pr_checks_gitlab.rs` (new)
  - `crates/forge-cli/src/glab_version.rs` (new)
  - `crates/forge-cli/tests/fixtures/gitlab/pr_checks/` (new)
  - `crates/forge-cli/tests/integration/pr_checks_gitlab.rs` (new)
- **Description**: At startup of any GitLab check op, call `glab
  --version`, parse the minor (e.g. `glab 1.45.0`). If outside the
  pinned support range (current local install's minor ± 0 — single
  minor only, per locked decision), return `UNAVAILABLE 69` with
  `error.kind = "glab_version_unsupported"` and a hint to upgrade /
  downgrade. Inside the supported minor, parse `glab ci status -b
  <branch>` text output. The parser is small, deliberately strict,
  and covered by ≥ 6 fixtures (all-success, one-failure,
  mixed-states, pending-only, empty pipeline, manual-only).
- **Dependencies**:
  - Task 3.1
- **Complexity**:
  - 8
- **Acceptance criteria**:
  - Version probe is cached for the lifetime of the `forge-cli`
    invocation.
  - Out-of-range version produces `UNAVAILABLE 69` and never invokes
    `glab ci status`.
  - In-range parser produces an envelope byte-identical to the
    GitHub backend's envelope shape for the same logical state (with
    different `data.provider` and link host).
  - Tests must NOT call out to live `glab`; they stub via
    `FORGE_CLI_GLAB_BIN`.
- **Validation**:
  - `cargo test -p nils-forge-cli pr_checks_gitlab glab_version`

### Task 3.3: `pr wait-checks` polling loop

- **Location**:
  - `crates/forge-cli/src/ops/pr_wait_checks.rs` (new)
  - `crates/forge-cli/tests/integration/pr_wait_checks.rs` (new)
- **Description**: Implement the blocking poll on top of `pr.checks`.
  Inputs: `<id>` (numeric or branch), `--timeout` (default `30m`),
  `--interval` (default `20s`), `--required-only` (default `true`).
  Loop sleeps `--interval` between snapshots; emits a single envelope
  at the end (no streaming). Exit-code mapping per spec:
  - all required `success` → `SUCCESS 0`.
  - any required `failure`/`cancelled`/`timed_out` → `RUNTIME 1`,
    `error.kind = checks_failed`.
  - timeout reached → `UNAVAILABLE 69`,
    `error.kind = checks_timeout`.
  Polling timer uses `tokio::time::sleep` if a runtime is convenient,
  otherwise `std::thread::sleep`; implementation decides. `--dry-run`
  short-circuits with `data.plan` showing the would-be invocation
  loop summary, not an infinite list.
- **Dependencies**:
  - Task 3.1
  - Task 3.2
- **Complexity**:
  - 7
- **Acceptance criteria**:
  - Test "succeeds on third poll" passes deterministically via a
    fixture sequence.
  - Test "fails on first poll with required failure" exits
    `RUNTIME 1`.
  - Test "times out after N intervals" exits `UNAVAILABLE 69` and
    reports `duration_ms` ≥ timeout.
  - Schema literal stays `cli.forge-cli.pr.checks.v1` (shared with
    snapshot atom).
- **Validation**:
  - `cargo test -p nils-forge-cli pr_wait_checks`

## Sprint 4: `pr merge` with lock-down + TTL-zero required-check re-check

**Goal**: The riskiest single atom. Lands every lock-down validation
listed in spec §"Lock-down policy" rules 4 / 6 / 7 / 8 / 9 / 10 in one
place, plus the TTL-zero re-check that addresses the
`github-pr-required-check-gating` operation record. After this sprint
the binary can complete the macro flow except for the macro
composition step itself.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-forge-cli pr_merge`
  - `forge-cli pr merge 1 --method squash --dry-run --format json`
  - `forge-cli pr merge 1 --method merge --allow-non-default-base --dry-run --format json`
- Verify: refusal paths produce `DATA 65` with the documented
  `error.kind`; `--method` overrides `.forge-cli.toml`; `--keep-branch`
  prevents `--delete-branch` (gh) / `--remove-source-branch` (glab).

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 4.1: Merge-method resolution + `.forge-cli.toml` loader

- **Location**:
  - `crates/forge-cli/src/config.rs` (new)
  - `crates/forge-cli/tests/integration/config.rs` (new)
  - `crates/forge-cli/tests/fixtures/config/` (new)
- **Description**: Implement the per-repo `.forge-cli.toml` loader
  (search upwards from CWD to the git toplevel, stop at toplevel). Map
  recognized keys to typed structs; unknown keys generate a
  `warnings[]` entry, never an error (forward compat for v2). Provide
  the resolution function: explicit flag > `.forge-cli.toml` > spec
  default. Cover `[merge].method`, `[merge].delete_branch`,
  `[body].summary_heading`, `[body].test_plan_heading`,
  `[branch].feature_prefix`, `[branch].bug_prefix`, `[checks].timeout`,
  `[checks].interval`, `[checks].required_only`.
- **Dependencies**:
  - Task 3.3
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - Loader finds `.forge-cli.toml` at any ancestor up to git toplevel.
  - Unknown key produces exactly one warning per key, prefixed
    `unknown-config-key:`.
  - Resolution precedence is verified by a unit test for each
    setting.
- **Validation**:
  - `cargo test -p nils-forge-cli config`

### Task 4.2: TTL-zero required-check re-check helper

- **Location**:
  - `crates/forge-cli/src/ops/required_check_gate.rs` (new)
  - `crates/forge-cli/tests/integration/required_check_gate.rs` (new)
- **Description**: Extract the "snapshot required checks right now and
  bail if not all green" logic into a reusable helper. This is called
  by `pr merge` immediately before the backend subprocess invocation
  even when `pr wait-checks` succeeded earlier in the macro. Calls
  `pr.checks` internally (no new subprocess wrapper). Returns one of
  `Ok(())`, `Err(checks_pending)` → `DATA 65`, or `Err(checks_failed)`
  → `RUNTIME 1`. The split between pending and failed mirrors the
  ops YAML's `required_checks_green` rule.
- **Dependencies**:
  - Task 3.3
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - Pending re-check exits `DATA 65` with
    `error.kind = "checks_pending"`.
  - Failed re-check exits `RUNTIME 1` with
    `error.kind = "checks_failed"`.
  - Helper is invoked even when `pr wait-checks` ran < 1s before
    (no caching across atoms).
- **Validation**:
  - `cargo test -p nils-forge-cli required_check_gate`

### Task 4.3: `pr merge` atom

- **Location**:
  - `crates/forge-cli/src/ops/pr_merge.rs` (new)
  - `crates/forge-cli/tests/fixtures/github/pr_merge/` (new)
  - `crates/forge-cli/tests/fixtures/gitlab/pr_merge/` (new)
  - `crates/forge-cli/tests/integration/pr_merge.rs` (new)
- **Description**: Wire every lock-down rule from spec §"Lock-down
  policy" that applies to `pr merge`: `worktree_clean`,
  `draft_merge_refused` (require non-draft state from `pr view`),
  `default_branch_protected` (compare base to `repo view`'s
  `default_branch` unless `--allow-non-default-base`),
  `required_checks_green` via Task 4.2's helper,
  `merge_method_supported` (intersect `--method` with
  `repo view`'s `merge_methods_allowed`), `keep_branch_conflict`.
  Backend argv per ops YAML: `gh pr merge <id> --{method}
  --delete-branch?` and `glab mr merge <id> --squash?
  --remove-source-branch?`. Post-merge: re-fetch via `pr view` to
  populate `merge_sha` and report `deleted_branch`. Schema literal:
  `cli.forge-cli.pr.merge.v1`.
- **Dependencies**:
  - Task 4.1
  - Task 4.2
- **Complexity**:
  - 9
- **Acceptance criteria**:
  - All six lock-down failure paths exit `DATA 65` with the
    documented `error.kind` (or `RUNTIME 1` for `checks_failed`),
    each covered by a test.
  - Successful merge envelope contains `merge_sha`, `method`,
    `deleted_branch`.
  - `--method` overrides `.forge-cli.toml`; `--method` not in
    `merge_methods_allowed` exits with
    `error.kind = "merge_method_unsupported"`.
  - `--keep-branch` mutually exclusive with `--delete-branch` (which
    is implicit-true by default); explicit conflict path covered.
- **Validation**:
  - `cargo test -p nils-forge-cli pr_merge`

## Sprint 5: Issue atoms (create / view / edit / comment / close / reopen)

**Goal**: Complete the Issue surface. Issue ops have a much smaller
validation footprint than PRs (no branch / body / push state), so
this sprint is mostly a structural mirror of Sprint 2's PR atoms.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-forge-cli issue_`
  - `forge-cli issue create --title "demo" --body "..." --dry-run --format json`
  - `forge-cli issue view 1 --dry-run --format json`
- Verify: every issue atom emits a snake_case envelope with the
  spec's schema literal; `title_length` violation on `issue create`
  exits `DATA 65` with `error.kind = "title_too_long"`.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 5.1: `issue create`, `issue view`, `issue close`, `issue reopen`

- **Location**:
  - `crates/forge-cli/src/ops/issue_create.rs` (new)
  - `crates/forge-cli/src/ops/issue_view.rs` (new)
  - `crates/forge-cli/src/ops/issue_close.rs` (new)
  - `crates/forge-cli/src/ops/issue_reopen.rs` (new)
  - `crates/forge-cli/tests/fixtures/github/issue_create/` (new)
  - `crates/forge-cli/tests/fixtures/gitlab/issue_create/` (new)
  - `crates/forge-cli/tests/fixtures/github/issue_view/` (new)
  - `crates/forge-cli/tests/fixtures/gitlab/issue_view/` (new)
  - `crates/forge-cli/tests/fixtures/github/issue_close/` (new)
  - `crates/forge-cli/tests/fixtures/gitlab/issue_close/` (new)
  - `crates/forge-cli/tests/fixtures/github/issue_reopen/` (new)
  - `crates/forge-cli/tests/fixtures/gitlab/issue_reopen/` (new)
  - `crates/forge-cli/tests/integration/issue_create.rs` (new)
  - `crates/forge-cli/tests/integration/issue_view.rs` (new)
  - `crates/forge-cli/tests/integration/issue_close.rs` (new)
  - `crates/forge-cli/tests/integration/issue_reopen.rs` (new)
- **Description**: Mirror the structure of Sprint 2's PR atoms.
  `issue create` validates only `title_length`. Output envelopes
  follow the ops YAML's `data` shape. Reopen and close are simple
  argv passthroughs with a follow-up `issue view --json` for the
  fresh state.
- **Dependencies**:
  - Task 1.7
- **Complexity**:
  - 6
- **Acceptance criteria**:
  - Schema literals: `cli.forge-cli.issue.create.v1`, `.view.v1`,
    `.close.v1`, `.reopen.v1`.
  - State normalization (`open` / `closed`) matches PR
    normalization rules.
  - Fixture pairs land for every op.
- **Validation**:
  - `cargo test -p nils-forge-cli issue_create issue_view issue_close issue_reopen`

### Task 5.2: `issue edit`, `issue comment`

- **Location**:
  - `crates/forge-cli/src/ops/issue_edit.rs` (new)
  - `crates/forge-cli/src/ops/issue_comment.rs` (new)
  - `crates/forge-cli/tests/fixtures/github/issue_edit/` (new)
  - `crates/forge-cli/tests/fixtures/gitlab/issue_edit/` (new)
  - `crates/forge-cli/tests/fixtures/github/issue_comment/` (new)
  - `crates/forge-cli/tests/fixtures/gitlab/issue_comment/` (new)
  - `crates/forge-cli/tests/integration/issue_edit.rs` (new)
  - `crates/forge-cli/tests/integration/issue_comment.rs` (new)
- **Description**: `issue edit` runs `title_length` when `--title`
  set; supports `--add-label` / `--remove-label` / `--add-assignee`.
  `issue comment` has no validation; supports `--body` /
  `--body-file`. Same backend invocation + follow-up `issue view`
  pattern as Sprint 2.
- **Dependencies**:
  - Task 5.1
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - Label add/remove argv constructed correctly on both backends.
  - `--body-file -` reads from stdin.
- **Validation**:
  - `cargo test -p nils-forge-cli issue_edit issue_comment`

## Sprint 6: `pr deliver` macro

**Goal**: Compose Sprints 1–4's atoms into the canonical macro that
matches agent-kit's `deliver-{feature,bug}-pr` flow. The macro is the
single biggest behavioural lock-in for forge-cli: callers must NOT be
able to skip steps or reorder them.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-forge-cli pr_deliver`
  - `forge-cli pr deliver --kind feature --title "demo" --body-file <(printf '## Summary\nx\n\n## Test plan\ny\n') --dry-run --format json`
  - `forge-cli pr deliver --kind bug --no-merge --dry-run --format json`
- Verify: envelope schema `cli.forge-cli.pr.deliver.v1`, `data.steps[]`
  contains exactly the steps that ran in order; a failing step
  short-circuits and is the last entry; outer exit code matches the
  failing atom's exit code (no remapping).

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 6.1: Macro composition + step envelope

- **Location**:
  - `crates/forge-cli/src/macros/pr_deliver.rs` (new)
  - `crates/forge-cli/tests/integration/pr_deliver.rs` (new)
- **Description**: Implement the sequence per spec §"Macro: pr
  deliver": `auth.status` → `repo.view` → `pr.create` →
  `pr.wait-checks` → `pr.ready` (skip if `--no-merge`) →
  `pr.merge` (skip if `--no-merge`). Each step calls into the
  underlying atom's pure function (no subprocess re-spawn through a
  child binary). The macro accumulates per-step envelopes under
  `data.steps[]` as `{ step, ok, schema_version, payload }`. On
  failure the macro propagates the failing atom's exit code
  unchanged.
- **Dependencies**:
  - Task 4.3
- **Complexity**:
  - 8
- **Acceptance criteria**:
  - Successful end-to-end run lists all six steps (or four with
    `--no-merge`).
  - Failure at any step omits later steps from `data.steps[]` (no
    "step did not run" entries; spec explicit).
  - `--no-merge` exits with macro's outer code = `pr.wait-checks`
    exit code (typically `0`).
  - Outer exit on `pr.create` validation failure is `DATA 65`, not
    a remapped runtime code.
  - Test asserts byte-stable envelope ordering of `data.steps[]`.
- **Validation**:
  - `cargo test -p nils-forge-cli pr_deliver`

### Task 6.2: Macro CLI surface + dry-run plan rendering

- **Location**:
  - `crates/forge-cli/src/cli.rs`
  - `crates/forge-cli/tests/integration/pr_deliver_cli.rs` (new)
- **Description**: Wire the `pr deliver` subcommand to the macro from
  Task 6.1. Surface all flags from the ops YAML (`--kind`, `--title`,
  `--body`, `--body-file`, `--head`, `--base`, `--method`,
  `--reviewer`, `--timeout`, `--no-merge`,
  `--allow-non-default-base`). `--dry-run` collects each atom's own
  `data.plan` into a top-level `data.plan_steps[]` array so the
  caller can preview the full chain.
- **Dependencies**:
  - Task 6.1
- **Complexity**:
  - 4
- **Acceptance criteria**:
  - `forge-cli pr deliver --help` lists every documented flag.
  - `--dry-run` emits `data.plan_steps[]` with one entry per atom that
    would run; no subprocess is invoked.
  - `--no-merge` excludes `pr.ready` and `pr.merge` from the dry-run
    plan.
- **Validation**:
  - `cargo test -p nils-forge-cli pr_deliver_cli`

## Sprint 7: Parity harness + exit-code matrix + fixture corpus

**Goal**: Lock the v1 contract behind tests. A single parity harness
asserts the envelope is byte-identical across providers for every
atom (modulo `data.provider` and host fragments of URLs). The
exit-code matrix covers all six sysexits paths. The fixture corpus is
fully redacted and stable.

**Demo/Validation**:

- Commands:
  - `cargo test -p nils-forge-cli parity`
  - `cargo test -p nils-forge-cli exit_codes_full`
  - `bash scripts/ci/cli-output-contract-lint.sh`
  - `grep -RInE 'gh[ps]_[A-Za-z0-9]+|glpat-[A-Za-z0-9_-]+|ghr_[A-Za-z0-9]+|gho_[A-Za-z0-9]+' crates/forge-cli/tests/fixtures/`
- Verify: parity harness passes for every paired op; grep returns no
  un-redacted token-shaped strings in any fixture; exit-code matrix
  covers `SUCCESS / RUNTIME / USAGE / DATA / UNAVAILABLE / SOFTWARE`.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 7.1: Parity harness

- **Location**:
  - `crates/forge-cli/tests/integration/parity.rs` (new)
  - `crates/forge-cli/tests/support/parity.rs` (new)
- **Description**: Build a small driver that iterates the spec's
  parity matrix and, for each row, runs both backends through the
  same logical input, then diffs the resulting envelopes. The diff
  IGNORES `data.provider` and any `data.url` host fragment (a
  normalizer replaces `https://gitlab.com/<owner>` with
  `https://<host>/<owner>` before compare). Any other field difference
  is a failure.
- **Dependencies**:
  - Task 5.2
  - Task 6.2
- **Complexity**:
  - 7
- **Acceptance criteria**:
  - Parity rows for `auth status`, `repo view`, `pr create`,
    `pr view`, `pr list`, `pr edit`, `pr comment`, `pr ready`,
    `pr merge`, `pr close`, `pr checks`, `pr wait-checks`,
    `issue create`, `issue view`, `issue edit`, `issue comment`,
    `issue close`, `issue reopen`, `pr deliver` all pass.
  - One deliberate fixture mutation (e.g. flip a field on GitLab
    side) is caught by the harness in a separate negative test.
- **Validation**:
  - `cargo test -p nils-forge-cli parity`

### Task 7.2: Exit-code matrix completion

- **Location**:
  - `crates/forge-cli/tests/integration/exit_codes_full.rs` (new)
- **Description**: Extend Sprint 1's matrix to cover every code +
  every triggering kind. One test per `(exit_constant, error.kind)`
  pair from the spec's exit code table and the lock-down `error.kind`
  table. Each test asserts the constant name from
  `nils_common::cli_contract::exit`, not the numeric value.
- **Dependencies**:
  - Task 7.1
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - Every row in spec §"Exit code map" has at least one test.
  - Every row in spec §"Lock-down policy"'s `error.kind` table has at
    least one test.
  - `bash scripts/ci/cli-output-contract-lint.sh` passes.
- **Validation**:
  - `cargo test -p nils-forge-cli exit_codes_full`
  - `bash scripts/ci/cli-output-contract-lint.sh`

### Task 7.3: Fixture redaction audit

- **Location**:
  - `crates/forge-cli/tests/fixtures/README.md` (new)
  - `scripts/ci/forge-cli-fixture-lint.sh` (new)
- **Description**: Add a tiny lint script that greps the fixture tree
  for token-shaped strings (`gh[ps]_*`, `glpat-*`, `ghr_*`, `gho_*`,
  `Bearer [A-Za-z0-9._-]+`) and fails on any match. Wire it into
  `nils-cli-checks-entrypoint.sh --docs-only` so PR review catches
  un-redacted fixtures before merge. Audit every existing fixture
  added by Sprints 1–6 and replace any token-shaped placeholder with
  `<redacted-token>`.
- **Dependencies**:
  - Task 7.2
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - `bash scripts/ci/forge-cli-fixture-lint.sh` returns 0.
  - A synthetic regression fixture (a fake `ghp_aaa...` string) is
    caught by the lint and reported with file path + line.
  - Script appears in `--docs-only` checks and `DEVELOPMENT.md`.
- **Validation**:
  - `bash scripts/ci/forge-cli-fixture-lint.sh`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`

## Sprint 8: Wrapper + completion + Homebrew tap formula + nils-cli minor bump

**Goal**: Ship `forge-cli` as a first-class workspace binary that
users can `brew install` and shell-complete. After this sprint the
binary is feature-complete and the v1 acceptance gate can be checked.

**Demo/Validation**:

- Commands:
  - `wrappers/forge-cli --help` (delegates to the installed binary).
  - `compdef _forge-cli forge-cli && forge-cli pr <TAB>` (zsh
    completion lists subcommands).
  - `bash completions/forge-cli.bash && complete -p forge-cli`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh`
  - `brew bump-formula-pr` dry-run on the tap (manual step).
- Verify: brew tap formula installs the binary and the wrapper;
  shell completions list every subcommand from spec §"Command tree";
  workspace gate is green; `nils-cli` minor is bumped to the next
  patch boundary.

**PR grouping intent**: `group`
**Execution Profile**: `serial`

### Task 8.1: `wrappers/forge-cli` + shell completions

- **Location**:
  - `wrappers/forge-cli` (new — bash wrapper mirroring
    `wrappers/git-cli`)
  - `completions/_forge-cli` (new — zsh completion)
  - `completions/forge-cli.bash` (new — bash completion)
  - `crates/forge-cli/src/cli.rs` (add `completion` subcommand that
    emits clap-generated scripts per workspace standard)
- **Description**: Follow
  `docs/runbooks/cli-completion-development-standard.md` and the
  `git-cli` precedent. The `forge-cli completion zsh|bash` subcommand
  prints the script; checked-in completion files are the snapshot of
  that output and a generation test asserts they stay in sync.
- **Dependencies**:
  - Task 7.3
- **Complexity**:
  - 5
- **Acceptance criteria**:
  - `forge-cli completion zsh` and `forge-cli completion bash`
    succeed and print non-empty content.
  - Checked-in completion files equal the output of the corresponding
    subcommand (test asserts byte-equality, regenerates on diff).
  - `wrappers/forge-cli` resolves the binary the same way
    `wrappers/git-cli` does.
- **Validation**:
  - `cargo test -p nils-forge-cli completion`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh`

### Task 8.2: Homebrew tap formula update

- **Location**:
  - `wrappers/forge-cli`
  - tap repository (external — `homebrew-tap`): the formula update is
    driven by the existing `nils-cli-bump-version-tag-release` flow,
    so this task only ensures the workspace side is ready (wrapper
    path, completion install hooks, README mention) and lists the tap
    formula bump as a Sprint 9 release action.
- **Description**: Audit `Formula/nils-cli.rb`-equivalent in the tap
  for the install hooks pattern used by `git-cli`. Confirm
  `forge-cli` follows the same convention so the bump skill picks it
  up automatically. No tap-repo edit lands in this PR; the bump
  happens in Task 8.3.
- **Dependencies**:
  - Task 8.1
- **Complexity**:
  - 2
- **Acceptance criteria**:
  - `wrappers/forge-cli` is referenced in the binary list the bump
    skill consumes (verify via dry-run of the bump skill).
  - README cross-link from `nils-cli` workspace README to the
    `forge-cli` crate README is in place.
- **Validation**:
  - Manual dry-run of `nils-cli-bump-version-tag-release` (do not
    execute the bump in this PR).

### Task 8.3: `nils-cli` minor bump + tag + tap formula bump

- **Location**:
  - workspace `Cargo.toml` (`workspace.package.version`)
  - per-crate `Cargo.toml` patches the bump skill emits
  - `CHANGELOG.md` (if present) — release notes entry for `forge-cli`
  - external `homebrew-tap` formula
- **Description**: After Sprint 8.1 and 8.2 land on `main`, run the
  workspace's release flow via the `nils-cli-bump-version-tag-release`
  skill (minor bump). The skill handles annotated tag, push, tap
  formula bump, and post-release verification. The acceptance gate is
  considered met once the resulting release passes
  `nils-cli-verify-required-checks` and the tap formula installs the
  new binary.
- **Dependencies**:
  - Task 8.1
  - Task 8.2
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - Workspace version bumps minor (e.g. `0.10.x` → `0.11.0`).
  - Tag `vX.Y.0` is pushed.
  - Tap formula bump PR is merged.
  - `brew upgrade nils-cli` installs `forge-cli` on a clean tap
    checkout.
- **Validation**:
  - `nils-cli-verify-required-checks` reports green.
  - Manual: `brew upgrade nils-cli && forge-cli --version`.

## Testing Strategy

- **Unit**: every op's pure helpers (provider detection, body parser,
  validation rules, glab text parser, merge-method resolver) have at
  least one positive + one negative unit test in the same file as the
  helper.
- **Integration (per op)**: each atom has a paired fixture under
  `crates/forge-cli/tests/fixtures/{github,gitlab}/<op>/` and an
  integration test driving the backend stub via `FORGE_CLI_GH_BIN` /
  `FORGE_CLI_GLAB_BIN`. Fixtures cover the success path plus every
  documented error kind.
- **Parity**: Sprint 7 harness asserts envelope byte-equality across
  backends per parity row.
- **Exit-code matrix**: per workspace policy + spec §"Exit code
  map" — one test per `(exit_constant, error.kind)` pair.
- **Dry-run smoke**: every op's integration test includes a
  `--dry-run` invocation that asserts the `data.plan` array shape;
  this is what proves no live network call is required for the
  default gate.
- **Lint**: `bash scripts/ci/cli-output-contract-lint.sh` + new
  `bash scripts/ci/forge-cli-fixture-lint.sh` run as part of the
  docs-only gate.
- **Workspace gate**: `NILS_CLI_TEST_RUNNER=nextest bash
  scripts/ci/nils-cli-checks-entrypoint.sh` stays green after every
  sprint.
- **End-to-end (opt-in)**: tests behind `FORGE_CLI_E2E=1` exercise a
  designated sandbox repo; default CI does not run them. These guard
  against drift in `gh` / `glab` JSON shape between the fixture
  snapshot and the live binary.

## Risks & gotchas

- `gh pr create` does not return JSON; the implementation MUST do a
  follow-up `gh pr view --json …` to populate the envelope. Skipping
  this step would leave `merge_sha` and `head` empty on success and
  break the parity test.
- `glab` JSON output for `ci status` does not exist in the installed
  minor; the text parser is brittle by definition. Pinning to the
  current minor + fail-fast (Sprint 3.2) keeps blast radius bounded
  but creates a tooling-upgrade hazard: developers who upgrade `glab`
  outside the supported minor will see `UNAVAILABLE 69`. The error
  message must point them at the exact supported range.
- `pr wait-checks` shares schema with `pr checks` deliberately. A
  refactor that splits them later must NOT change the schema literal
  without bumping to `v2`.
- The macro propagates inner exit codes unchanged. Callers that grep
  for a specific code without also reading `error.kind` will
  misclassify a `DATA 65` from `pr.create` as the same class as a
  `DATA 65` from `pr.merge`. This is intentional; the spec's
  callers-must-branch-on-error.kind contract is documented in spec
  §"Exit code map".
- `.forge-cli.toml` is read at startup and not re-read mid-run. A
  long-running `pr wait-checks` that survives a `.forge-cli.toml`
  edit will use the original values; this is acceptable for v1 but
  worth a release-note line.
- Token redaction relies on regex matching of the four documented
  prefixes. Backend stderr that contains a personal access token in
  an undocumented shape will leak. The Sprint 7 fixture lint provides
  test-time defence; production runs rely on the regex set staying
  in sync with provider docs.
- Workspace tests reuse one process; tests that mutate environment
  variables (e.g. `FORGE_CLI_GH_BIN`) MUST use the `EnvGuard` pattern
  from `nils-test-support` to avoid cross-test bleed.
- The parity test normalises `data.url` host fragments. A future
  change that introduces backend-specific path fragments (e.g.
  `/merge_requests/` vs `/pull/`) needs to extend the normalizer or
  the test will start flapping. Capture URL shape divergence in
  `data.url` and leave the host normalization narrow.

## Rollback plan

- **Sprint 1 rollback**: revert the new crate; nothing else in the
  workspace depends on `nils-forge-cli` yet.
- **Sprint 2–6 rollback**: each sprint is its own PR. Reverting a
  single sprint leaves earlier ops working; the macro Sprint 6
  rollback is the only one that visibly impacts user-facing flows
  because `pr deliver` would disappear, but the atoms remain.
- **Sprint 7 rollback**: revert removes the parity / matrix tests but
  leaves the binary intact. Re-introduce by cherry-picking the
  reverted commits.
- **Sprint 8 rollback**: leave wrapper + completions in place but
  revert the brew tap formula bump in a follow-up. The minor bump
  cannot be unbumped; a patch follow-up bump that ships only docs is
  the recovery path.
- **Cross-sprint rollback**: if the binary is found unsafe in the
  field, the agent-kit migration (out of v1 scope) is the
  fail-safe — agent-kit skills keep calling `gh` / `glab` directly
  until forge-cli is re-validated.
