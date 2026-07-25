# `semantic-commit default-branch` Redesign — Implementation Handoff

- Status: promoted into an active local-only L2 plan
- Date: 2026-07-25
- Source: `discussion-to-implementation-doc` from the converged CLI-boundary
  and command-design discussion
- Ownership: workspace-level, transient development record
- Intended next step: derive the local-only implementation plan from this
  source, then implement, validate, commit to local `main`, build, install, and
  deploy without using GitHub
- Delivery boundary: local repositories and local runtime homes only

## Purpose

Replace the current `semantic-commit local-default` command with a clearer,
self-contained `semantic-commit default-branch` command while preserving the
three-CLI ownership model:

- `git-cli` owns repository, branch, checkout, and worktree mechanics.
- `semantic-commit` owns commit authoring, message validation, signing, and
  local commit evidence.
- `forge-cli` owns provider interaction and remote delivery.

The replacement remains a narrow exception for one explicitly authorized
signed commit on the primary checkout's default branch. It is not the normal
development path, does not contact a remote, and does not deliver to a
provider.

This change is intentionally breaking. The old `local-default` command, output
contract, receipt names, and related `forge-cli` option are removed rather than
retained as aliases or compatibility shims.

## Evidence keys

- `[U1]` Maintainer decision: retain the three CLIs and strengthen their
  specialist boundaries.
- `[U2]` Maintainer decision: subsequent implementation is delivered through
  local `main`, followed by local build, install, and runtime deployment.
  GitHub PRs are unavailable and GitHub Actions must not be used.
- `[U3]` Maintainer decision: do not retain a `local-default` alias; design the
  replacement as the only supported command.
- `[F1]` `crates/semantic-commit/src/local_default.rs` currently owns argument
  parsing, repository preflight, forced signing, postcondition verification,
  cached-upstream accounting, receipt construction, output, and Git process
  helpers in one module.
- `[F2]` The current preflight verifies that the checked-out branch equals the
  caller-provided `--expected-branch`; it does not independently prove that the
  branch is the repository's default branch.
- `[F3]` The runtime-kit
  `core/hooks/shared/block-unsafe-default-delivery.py` special admission path
  likewise compares the current branch with `--expected-branch`, while its
  ordinary commit path separately resolves the cached default branch.
- `[F4]` `crates/nils-common/src/local_default_receipt.rs` defines the current
  strict receipt consumed by
  `crates/forge-cli/src/ops/repo_push_default.rs`.
- `[F5]` `forge-cli` accepts current receipts for later provider delivery only
  when the cached upstream transition was `aligned` to `ahead-by-one`.
- `[F6]` Active references span `semantic-commit`, `nils-common`,
  `forge-cli`, `agent-hook`, generated completions, runtime-kit source hooks,
  policies, rendered surfaces, goldens, and hook tests. This is a coupled
  cross-repository contract change.
- `[F7]` `scripts/install-local-release-binaries.sh` is the repository-owned
  local release build/install entrypoint.
- `[F8]` Runtime-kit `scripts/sync-runtime-surfaces.sh` is dry-run by default
  and, with `--apply`, installs rendered surfaces and the digest-pinned
  `agent-hook` policy bundle into local runtime homes.

## Confirmed findings

| Priority | Finding | Evidence | Required correction |
| --- | --- | --- | --- |
| P1 | The public name `local-default` is policy jargon and does not clearly identify the exceptional target as the default branch. | `[U1]`, current `semantic-commit --help` | Make `default-branch` the sole canonical subcommand. |
| P1 | The command contract is not self-contained: it trusts a caller-supplied branch name instead of independently proving the default branch. | `[F2]`, `[F3]` | Resolve and verify the authoritative cached default branch inside both the CLI and hook admission path. |
| P1 | Removing the old command affects a receipt consumer and two hook implementations, so a partial rollout can block all governed local-main commits. | `[F4]`, `[F6]` | Deliver both source repositories before replacing installed binaries or deployed hooks; verify the cutover from a fresh invocation. |
| P2 | The public surface exposes implementation acknowledgements (`--expected-branch`, `--remote-mode local-only`) that should be invariants derived by the command. | `[F1]` | Remove the flags and derive the values internally. |
| P2 | Dry-run and validate-only paths construct the final receipt type even though no commit exists and the strict final receipt invariants are not satisfied. | `[F1]`, `[F4]` | Give preview output a separate non-adoptable schema; emit a durable receipt only after a successful commit. |
| P2 | The command accepts an already-ahead upstream state even though `forge-cli` refuses to adopt that receipt later. | `[F5]` | For remote-backed repositories, require an aligned cached upstream before authoring the one exceptional commit. |
| P2 | One large module manually forwards commit options and duplicates knowledge about which options take values. | `[F1]` | Share typed commit arguments and split orchestration, preflight, transaction, and receipt responsibilities internally. |

## Decisions

### D1 — Preserve the three-CLI boundary

The command remains in `semantic-commit`.

- Do not move it to `git-cli`.
- Do not move it to `forge-cli`.
- Do not create a fourth delivery CLI.
- Do not broaden this implementation into cleanup of the existing
  `git-cli commit context*` overlap.

### D2 — Replace, do not alias

The only supported exceptional command becomes:

```text
semantic-commit default-branch
```

The following compatibility surfaces are forbidden:

- no visible or hidden `local-default` subcommand;
- no clap alias;
- no parser fallback;
- no wrapper translating the old name;
- no dual completion entry;
- no old receipt schema accepted by the new `forge-cli` implementation;
- no old `forge-cli --local-default-receipt` option alias.

After cutover, an invocation of `semantic-commit local-default ...` must fail as
an unknown subcommand with the standard usage exit code and must perform no Git
read or mutation beyond ordinary clap startup.

### D3 — Name the user intent, not the hook mechanism

The canonical help text should communicate the complete outcome:

```text
Create exactly one governed signed commit on the primary checkout's default
branch. Never contacts or updates a remote.
```

Do not use `override`, `bypass`, `waiver`, or `exception` as the command name.
The documentation may call it an exceptional workflow, but the CLI surface
names the intended Git outcome.

### D4 — Keep one atomic transaction

The command remains one user-visible transaction:

```text
resolve and validate state
→ create one forced-signed semantic commit
→ verify postconditions
→ write one durable receipt
```

Do not split this into public `prepare`, `commit`, and `finalize` commands.
Internal modules may separate those responsibilities, but no mutable state may
be exposed between user-visible phases.

### D5 — Make the command self-validating

The CLI must independently prove every semantic property claimed by its name.
Hook admission provides an additional fail-closed gate; it is not the source of
truth for command correctness.

Default-branch resolution is local and network-free:

- The target must be a non-bare repository's primary checkout with attached
  `HEAD`.
- When the current branch has a configured upstream, resolve the upstream
  remote and its cached symbolic default branch without `fetch`,
  `ls-remote`, provider lookup, or any other network operation.
- The primary checkout branch, current branch, upstream branch, and cached
  remote default branch must agree.
- The cached upstream commit must equal `--expect-head` before mutation.
- A configured but missing, ambiguous, behind, diverged, or already-ahead
  upstream fails closed.
- For a repository with no configured remotes and no upstream metadata, the
  attached primary-checkout branch is the local default and may proceed.
- A repository with remotes but no authoritative cached default/upstream state
  fails closed.

The command must repeat the relevant branch, `HEAD`, upstream, clean-state, and
signature checks after the commit.

### D6 — Minimize the public command contract

The mutating form is:

```bash
semantic-commit default-branch \
  --expect-head <full-lowercase-object-id> \
  --receipt-out <new-absolute-path-outside-repository> \
  [--repo <absolute-repository-path>] \
  <message-construction-options> \
  [--format text|json]
```

The preview form is:

```bash
semantic-commit default-branch \
  --expect-head <full-lowercase-object-id> \
  [--repo <absolute-repository-path>] \
  <message-construction-options> \
  --dry-run \
  [--format text|json]
```

Required behavior:

- `--receipt-out` is required only for mutation.
- `--repo`, when present, must be absolute.
- Commit message construction reuses the typed arguments and validators owned
  by the ordinary `semantic-commit commit` path.
- `--amend`, `--allow-empty`, `--message-only`, and `--no-edit` remain
  unsupported.

Removed public options:

- `--expected-branch`;
- `--remote-mode`;
- `--validate-only`.

Message-only validation remains available through
`semantic-commit commit --validate-only`. Full exceptional-transaction
preflight uses `default-branch --dry-run`.

### D7 — Replace the receipt contract

The final durable receipt uses a new strict schema:

```text
cli.semantic-commit.default-branch.v1
```

The Rust types and modules use `DefaultBranch*` /
`default_branch_*` terminology. The final receipt:

- records repository fingerprint, default branch, old and new `HEAD`, parent,
  tree, verified signature, and staged-file count;
- records the cached upstream identity and exact
  `aligned` → `ahead-by-one` transition for remote-backed repositories;
- records an untracked local transition for a remote-free repository;
- records `network_observed=false` and `provider_mutated=false`;
- records `default_branch_committed=true`;
- records `provider_delivery_attempted=false` and
  `provider_delivered=false`;
- contains facts, not a policy claim such as
  `provider_reconciliation_required`;
- is created atomically at a new private path outside the repository;
- is never committed to the repository.

Dry-run JSON uses a separate non-adoptable schema:

```text
cli.semantic-commit.default-branch.preview.v1
```

Preview mode never writes a receipt file. `forge-cli` must reject preview
output and every old `cli.semantic-commit.local-default.v1` receipt.

### D8 — Keep provider delivery in `forge-cli`

Rename the adoption option to:

```text
forge-cli repo push-default --default-branch-receipt <path>
```

Do not retain `--local-default-receipt`.

Receipt creation does not authorize provider delivery. `forge-cli` continues to
require a fresh explicit provider-delivery authorization, expected remote base,
reason file, exact destination validation, compare-and-swap push, and remote
read-back. None of those provider operations are exercised during this
local-only implementation and deployment.

### D9 — Keep hook admission exact and duplicated defensively

Update both hook surfaces:

- nils-cli Rust effect classification under `crates/agent-hook`;
- agent-runtime-kit source hooks under `core/hooks/shared`.

The hooks may admit only the exact `semantic-commit default-branch` shape with:

- a full lowercase `--expect-head`;
- an absolute outside-repository `--receipt-out` for mutation;
- an explicitly resolvable repository target;
- a primary checkout;
- an attached branch that equals the authoritative cached default branch;
- no forbidden commit modes.

The CLI repeats these checks independently. Missing or ambiguous state fails
closed with actionable recovery text.

### D10 — Local-only delivery and deployment

The implementation is delivered only to the local `main` branches of:

- `sympoies/nils-cli`;
- `graysurf/agent-runtime-kit`.

There is no provider delivery for this work:

- no GitHub branch or PR;
- no GitHub issue or review thread;
- no `git push`;
- no `forge-cli` provider mutation;
- no GitHub Actions run, workflow dispatch, rerun, or status dependency;
- no release tag, GitHub Release, crates.io publish, or Homebrew update;
- no edits to `.github/workflows/**`.

All required build, test, coverage, completion, render, hook, install, doctor,
and runtime acceptance evidence is produced locally.

## Scope

### `sympoies/nils-cli`

Expected active surfaces include:

- `crates/semantic-commit/src/cli.rs`;
- replacement of `crates/semantic-commit/src/local_default.rs` with a
  `default_branch` module layout;
- `crates/semantic-commit/src/completion.rs`;
- `crates/semantic-commit/src/lib.rs`;
- `crates/semantic-commit/README.md`;
- semantic-commit unit and integration tests;
- `crates/nils-common/src/local_default_receipt.rs` and its exports, renamed
  and redesigned for the new receipt;
- `crates/forge-cli/src/cli.rs`;
- `crates/forge-cli/src/ops/repo_push_default.rs`;
- forge-cli README and integration tests;
- `crates/agent-hook/src/effect.rs` and focused tests;
- generated zsh and bash completion assets for `semantic-commit` and
  `forge-cli`;
- active workspace docs and contract references.

### `graysurf/agent-runtime-kit`

Expected active source surfaces include:

- `core/hooks/shared/block-unsafe-default-delivery.py`;
- `core/hooks/shared/hook_common.py`;
- `core/hooks/shared/session-coordination-guard.py`;
- `core/policies/git-delivery.md`;
- `core/policies/intent-cards.md`;
- `core/policies/files-hooks-validation.md` when its exact command wording is
  affected;
- `core/hooks/README.md` when operator guidance is affected;
- hook tests under `tests/hooks/`;
- source templates and goldens that render the home policy.

Generated `build/**` outputs are refreshed through the repository renderer and
validated against goldens; do not hand-edit generated output as the source of
truth. Immutable historical archive and heuristic records are not rewritten.

## Non-scope

- Moving any commit-authoring behavior into `git-cli`.
- Moving local commit creation into `forge-cli`.
- Removing or redesigning `git-cli commit context*`.
- Changing the normal managed-worktree + PR workflow.
- Adding provider delivery to `semantic-commit`.
- Adding network access to `semantic-commit default-branch`.
- Generalizing the receipt into an arbitrary commit provenance format.
- Amending, squashing, merging, or creating multiple commits in one
  `default-branch` invocation.
- GitHub PR, issue, release, Actions, workflow, or remote-push work.
- Public release or package publication.
- Retrofitting immutable historical documents with the new command name.

## Internal implementation boundaries

Keep the public transaction singular while splitting the implementation into
cohesive components:

```text
default_branch/
├── command.rs       # typed args, text/JSON output, orchestration
├── preflight.rs     # repository snapshot and default-branch resolution
├── transaction.rs   # forced signed commit and postconditions
└── receipt.rs       # preview/final schemas and atomic receipt write
```

The exact filenames may follow neighboring crate conventions, but the
responsibilities must remain separated.

The ordinary commit and default-branch paths must share typed message
construction and validation. Do not retain a manually synchronized
`option_takes_value` forwarding list.

Git subprocess execution should use one bounded, testable adapter shared by the
new modules. Tests must be able to prove that no forbidden network-capable Git
operation is invoked.

## Test-first contract delta

Before production edits, add meaningful failing tests for at least:

1. `semantic-commit default-branch --help` exists and `local-default` is
   unknown.
2. A primary checkout on a non-default branch is rejected even when the caller
   would previously have supplied the same branch through `--expected-branch`.
3. The new mutating surface does not accept `--expected-branch`,
   `--remote-mode`, or `--validate-only`.
4. A remote-backed aligned default branch succeeds and produces the new strict
   receipt.
5. Remote-backed missing, ambiguous, behind, diverged, and already-ahead states
   fail before commit creation.
6. A remote-free primary repository succeeds without a remote acknowledgement
   flag.
7. Dry-run emits only the preview schema, writes no receipt, and cannot be
   adopted by `forge-cli`.
8. `forge-cli --default-branch-receipt` accepts only the new final receipt and
   the old option/receipt fail.
9. Rust and Python hook classifiers admit the exact new form, reject malformed
   forms, and do not recognize the old command as an exception.
10. Generated completions contain `default-branch` and exclude
    `local-default`.

Do not waive the red phase: this is a breaking behavior and cross-boundary
contract change.

## Acceptance criteria

### CLI and receipt

- `semantic-commit --help` lists `default-branch` and does not list
  `local-default`.
- `semantic-commit local-default --help` exits with the standard usage error.
- `semantic-commit default-branch --help` documents the minimal public
  contract and the no-network/no-provider guarantee.
- The CLI itself, without relying on hooks, rejects a primary checkout whose
  current branch is not the authoritative cached default branch.
- Exactly one signed single-parent commit is created from `--expect-head`.
- The worktree and index are clean after success.
- No fetch, remote lookup, push, or provider call occurs.
- Only `cli.semantic-commit.default-branch.v1` final receipts pass strict
  receipt parsing.
- Preview output is non-adoptable and never written as a durable receipt.

### `forge-cli`

- `--default-branch-receipt` revalidates branch, exact head/parent/tree,
  signature, repository fingerprint, aligned-to-ahead-by-one provenance, live
  remote base, destination, compare-and-swap, and read-back when provider
  delivery is separately authorized.
- `--local-default-receipt` is an unknown option.
- Old final receipts and new preview output are rejected before provider
  mutation.

### Hooks and policy

- Both Rust and Python hook surfaces recognize only the new command.
- The default-branch identity is independently checked by the command and
  admission hook.
- Hook guidance names `semantic-commit default-branch` and explains the
  current-request local-only authorization requirement.
- Active source, rendered surfaces, goldens, and completion assets agree.
- Legacy terminology remains only in negative migration tests, this handoff,
  and immutable historical records; there is no active alias or compatibility
  implementation.

### Three-CLI boundary

- `git-cli` gains no commit mutation or delivery command from this change.
- `semantic-commit` performs no provider mutation.
- `forge-cli` does not author commits.
- Documentation consistently distinguishes commit authoring, local completion,
  and provider delivery.

### Local delivery and deployment

- Both repositories contain one locally verified signed `main` commit for
  their owned changes and remain unpushed.
- The nils-cli release-default binary set is built locally and installed
  through `scripts/install-local-release-binaries.sh`.
- Runtime-kit surfaces and the policy bundle are previewed and then applied
  locally through `scripts/sync-runtime-surfaces.sh --no-pull`.
- Fresh command and hook invocations accept the new surface and reject the old
  surface.
- `agent-hook doctor` and relevant `agent-runtime doctor` checks pass for the
  installed products.
- No GitHub Actions or other provider evidence is required or consulted.

## Local validation plan

### Focused nils-cli checks

Run focused tests while implementing:

```bash
cargo test -p nils-semantic-commit
cargo test -p nils-common
cargo test -p nils-forge-cli
cargo test -p nils-agent-hook
```

When completion code or assets change:

```bash
zsh -n completions/zsh/_semantic-commit
bash -n completions/bash/semantic-commit
zsh -n completions/zsh/_forge-cli
bash -n completions/bash/forge-cli
zsh -f tests/zsh/completion.test.zsh
bash scripts/ci/completion-freshness-audit.sh --strict
bash scripts/ci/completion-flag-parity-audit.sh --strict
```

Run the declared local gate:

```bash
bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast
```

Because GitHub Actions is explicitly unavailable and must not be used, replace
the normal provider CI evidence with full local parity and coverage:

```bash
NILS_CLI_TEST_RUNNER=nextest \
  bash scripts/ci/nils-cli-checks-entrypoint.sh

NILS_CLI_TEST_RUNNER=nextest \
  bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage
```

Also run the strict documentation audits:

```bash
bash scripts/ci/docs-placement-audit.sh --strict
bash scripts/ci/docs-hygiene-audit.sh --strict
bash scripts/ci/markdownlint-audit.sh --strict
```

### Runtime-kit checks

Run focused hook tests during implementation and both declared final gates:

```bash
bash tests/hooks/run.sh
bash scripts/ci/all.sh
```

Validate rendered sources and goldens through the canonical repository gate;
do not accept hand-edited `build/**` output as evidence.

### Cross-boundary smoke

Use disposable local repositories and isolated receipt/output directories to
prove:

- the new command succeeds on an aligned primary default branch;
- a non-default primary branch is rejected;
- remote-free behavior succeeds;
- ahead, behind, diverged, missing, and ambiguous cached states fail;
- the new final receipt is accepted by strict parsing;
- preview and old receipts are rejected;
- no network-capable Git or provider command is observed;
- help, completion, hook, and receipt consumer surfaces use the same name.

## Local-main delivery and deployment contract

This ordering is mandatory because the old installed command and old deployed
hooks are the only currently admitted bootstrap path, while the completed
source tree intentionally removes that path.

1. Start with clean primary `main` checkouts for both `nils-cli` and
   `agent-runtime-kit`. Do not pull, fetch, or query GitHub.
2. Keep the currently installed `semantic-commit` binary and deployed hook
   policy unchanged while implementing and validating both repositories.
3. Complete all source, tests, docs, generated completions, runtime-kit
   templates/goldens, and local validation before replacing installed
   binaries.
4. Stage only the owned nils-cli paths and create its one local signed `main`
   commit using the pre-change installed
   `semantic-commit local-default` bootstrap command and an outside-repository
   receipt. This use of the old installed binary is a migration bootstrap, not
   a retained alias in the new source.
5. Stage only the owned runtime-kit paths and create its one local signed
   `main` commit through the same pre-change installed command before deploying
   the new hook policy.
6. Confirm both local commits and receipts, and confirm neither repository was
   pushed.
7. Build and smoke the new nils-cli binaries into an isolated staging prefix
   first. Preserve recoverable copies and digests of the currently installed
   binaries outside both repositories.
8. Install the complete release-default local binary set:

   ```bash
   ./scripts/install-local-release-binaries.sh
   ```

9. From the durable primary runtime-kit checkout, preview the local source
   deployment without pulling:

   ```bash
   bash scripts/sync-runtime-surfaces.sh \
     --source-root "$HOME/Project/graysurf/agent-runtime-kit" \
     --product both \
     --no-pull
   ```

10. Review the preview, then apply the same source:

    ```bash
    bash scripts/sync-runtime-surfaces.sh \
      --source-root "$HOME/Project/graysurf/agent-runtime-kit" \
      --product both \
      --no-pull \
      --apply
    ```

11. If a Hermes runtime home is present and managed on this host, repeat the
    preview/apply pair with `--product hermes`.
12. Run local doctor and fresh-process acceptance:

    ```bash
    agent-hook doctor --product codex
    agent-hook doctor --product claude
    agent-runtime doctor \
      --source-root "$HOME/Project/graysurf/agent-runtime-kit" \
      --product codex
    agent-runtime doctor \
      --source-root "$HOME/Project/graysurf/agent-runtime-kit" \
      --product claude
    ```

13. From a fresh shell/process, verify:
    - `semantic-commit --help` contains `default-branch`;
    - `semantic-commit --help` excludes `local-default`;
    - the new command's dry-run is admitted by the deployed hook;
    - the old command fails as unknown;
    - ordinary default-branch `semantic-commit commit` remains blocked;
    - normal feature-worktree commits remain allowed.
14. Retain the old binary backups and pre-deploy receipts until fresh-process
    acceptance passes. On failure, restore the prior binaries and use the
    runtime sync transaction's rollback/recovery path; do not reset or amend
    the local source commits automatically.

The implementation is not complete when tests pass but local installation or
runtime deployment remains pending.

## Risks and guardrails

- **Bootstrap lockout:** installing the new binary before committing the
  runtime-kit admission update can leave no admitted local-main commit path.
  Commit both repositories first, then install and deploy.
- **False default-branch claim:** a caller-provided branch name is not evidence
  of default identity. Resolve it independently in the CLI and hook.
- **Partial breaking rename:** any surviving active old command, receipt, forge
  option, hook token, or completion creates two conflicting contracts. Negative
  tests and immutable history are the only permitted legacy references.
- **Preview mistaken for delivery evidence:** preview output must have a
  separate schema and must never pass strict receipt adoption.
- **Unpromotable local chain:** reject an already-ahead remote-backed branch so
  the one exceptional commit has coherent provenance.
- **Cross-repository drift:** nils-cli and runtime-kit changes are coupled.
  Validate exact local heads and do not deploy a mixed old/new surface.
- **Provider leakage:** use `--no-pull` for runtime sync and do not invoke
  GitHub, `forge-cli` provider operations, workflow dispatch, release, or
  package-publish paths.
- **Irrecoverable live overwrite:** stage and smoke binaries outside the live
  prefix first, retain private backups, and rely on transactional hook
  cutover/rollback.
- **Generated-source drift:** update source templates and regenerate outputs;
  do not fix rendered `build/**` files by hand.

## Retention intent

This is a `docs/discussions/` implementation-readiness source. Keep it while
the breaking command replacement and local deployment are active.

If formal local execution tracking is needed, move this file into a dated
`docs/plans/` bundle as the discussion source, retire this original path, and
create the plan and execution-state documents there. Do not open a GitHub
tracker or PR for that promotion.

After successful local deployment, either:

- delete this transient record after its conclusions are reflected in
  canonical CLI, receipt, hook, and policy documentation; or
- retain a short canonical decision summary and remove this implementation
  handoff.

## Read first

### nils-cli

- `AGENTS.md`
- `DEVELOPMENT.md`
- `docs/runbooks/cli-completion-development-standard.md`
- `docs/specs/crate-docs-placement-policy.md`
- `crates/semantic-commit/README.md`
- `crates/semantic-commit/src/local_default.rs`
- `crates/nils-common/src/local_default_receipt.rs`
- `crates/forge-cli/README.md`
- `crates/forge-cli/src/ops/repo_push_default.rs`
- `crates/agent-hook/src/effect.rs`

### agent-runtime-kit

- `AGENTS.md`
- `DEVELOPMENT.md`
- `core/policies/git-delivery.md`
- `core/policies/files-hooks-validation.md`
- `core/hooks/README.md`
- `core/hooks/shared/block-unsafe-default-delivery.py`
- `core/hooks/shared/hook_common.py`
- `core/hooks/shared/session-coordination-guard.py`
- `tests/hooks/test_shared_hooks.py`

## Execution

- Recommended plan:
  `docs/plans/2026-07-25-semantic-commit-default-branch/semantic-commit-default-branch-plan.md`
- Recommended execution state:
  `docs/plans/2026-07-25-semantic-commit-default-branch/semantic-commit-default-branch-execution-state.md`
