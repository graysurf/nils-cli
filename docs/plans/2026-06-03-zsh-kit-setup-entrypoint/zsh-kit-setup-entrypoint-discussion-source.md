# zsh-kit Setup Entrypoint Implementation Handoff

- Status: decisions settled; ready for plan tracking.
- Date: 2026-06-03
- Source: operator discussion about keeping personal Zsh setup out of the
  public `agent-runtime-kit` image while still making container/runtime shell
  setup repeatable through a nils-cli entrypoint.
- Intended next step: open an L2 plan-tracking issue from this bundle. This is
  a source artifact, not the implementation itself.

## Execution

- Recommended plan: docs/plans/2026-06-03-zsh-kit-setup-entrypoint/zsh-kit-setup-entrypoint-plan.md
- Recommended execution state: docs/plans/2026-06-03-zsh-kit-setup-entrypoint/zsh-kit-setup-entrypoint-execution-state.md
- Status: decisions settled; plan generation is the next step.
- Next-task source: this document

## Problem

The `agent-runtime-kit` Docker image is a public, shared runtime image. Baking a
personal `~/.config/zsh` tree or a private shell repository into that image
would couple a generic agent runtime to one operator's machine-local shell
preferences, private scripts, credentials, and update cadence.

At the same time, the operator and agents do real work from a Zsh environment
after the image starts. The missing layer is a stable command that can bootstrap
that shell environment at runtime, using an operator-supplied repository URL and
runtime credentials, without moving Zsh-specific behavior into the Dockerfile or
into nils-cli itself.

## Decisions

- Implement a nils-cli binary named `zsh-kit` as the stable runtime entrypoint.
- Keep Zsh-specific setup behavior in the Zsh repository. nils-cli should clone
  or update the repository, validate expected files, optionally write the
  `ZDOTDIR` bootstrap, and dispatch to a repo-owned setup hook.
- Keep `agent-runtime-kit` image changes minimal: install public prerequisites
  such as `zsh`, but do not bake `~/.config/zsh`, private scripts, tokens, or
  operator-specific config.
- Support private repository access through runtime environment and mounted
  auth state only, for example `GH_TOKEN`, SSH agent forwarding, or an already
  authenticated `gh`. Do not persist tokens by default.
- Make the command safe to rehearse with `--dry-run` and explicit to mutate
  with `--apply`.
- Make tool installation optional and conservative. In containers, the default
  should not require root or package-manager access.

## Scope

- In scope:
  - Add a publishable nils-cli crate and binary for `zsh-kit`.
  - Define `zsh-kit setup` with stable text and JSON output contracts.
  - Implement clone/update/inspect/dispatch behavior for an
    operator-supplied Zsh repository URL.
  - Support explicit flags for destination, ref selection, Zsh bootstrap,
    feature selection, tool-install policy, dry-run/apply, force, and JSON
    output.
  - Add clap-first completions and release/package integration for the new
    binary.
  - Add a repo-owned setup hook in the Zsh repository that owns shell behavior.
  - After nils-cli release, update `agent-runtime-kit` to include the new
    nils-cli binary and only the minimal public Docker prerequisite `zsh`.
- Out of scope:
  - Baking personal shell config or private repositories into
    `agent-runtime-kit` images.
  - Moving Zsh function/alias/plugin logic into nils-cli.
  - Managing secrets or writing token material into config files.
  - Making package-manager installs mandatory inside containers.
  - Replacing the Zsh repository's existing bootstrap architecture.

## Requirements

1. `zsh-kit setup --repo <url> --apply` clones the repository when the
   destination is absent and updates it safely when the destination already
   exists.
2. `zsh-kit setup --dry-run` reports intended filesystem, git, bootstrap, and
   dispatch actions without mutating local state.
3. The command supports at least these flags:
   - `--repo <url>`
   - `--dest <path>` with default `$HOME/.config/zsh`
   - `--branch <name>` or `--ref <rev>`
   - `--write-zshenv`
   - `--features <csv>`
   - `--install-tools skip|repo`
   - `--dry-run` and `--apply`
   - `--force`
   - `--format text|json`
4. The command detects the repo-owned setup hook, dispatches to it, and passes
   feature/tool-install choices in a stable way.
5. The command refuses unsafe situations clearly, including dirty destination
   repositories, destination path conflicts, missing repo hooks, and embedded
   credentials in URLs that would be printed or persisted.
6. JSON mode returns a versioned envelope with actions, changed paths, selected
   repo/ref, hook path, mutation status, and stable error codes.
7. Completions are generated and committed for zsh and bash according to the
   workspace completion standard.
8. Release packaging includes the `zsh-kit` binary and completion assets.

## Acceptance Criteria

1. `cargo run -q -p nils-zsh-kit -- setup --repo <fixture-url> --dry-run`
   produces a complete no-mutation plan.
2. `zsh-kit setup --apply` works against a local fixture repository with a
   repo-owned setup hook and leaves an auditable result in text and JSON modes.
3. Dirty or mismatched destination states refuse with stable, tested error
   codes.
4. Completion coverage matrix, generated completion assets, workspace bin
   inventory, release publish order, and crate README are all updated.
5. The Zsh repository hook can be validated independently by its own check
   scripts.
6. The `agent-runtime-kit` image contains `zsh` and the released `zsh-kit`
   binary, but still does not contain the operator's Zsh repo contents.
7. A container smoke can run `zsh-kit setup --dry-run` without private auth and
   can document the authenticated `--apply` path.

## Validation Plan

- nils-cli:
  - `cargo fmt -p nils-zsh-kit -- --check`
  - `cargo test -p nils-zsh-kit`
  - `zsh -n completions/zsh/_zsh-kit`
  - `bash -n completions/bash/zsh-kit`
  - `bash scripts/workspace-bins.sh --release-default | rg '^zsh-kit$'`
  - `scripts/publish-crates.sh --dry-run --crate nils-zsh-kit`
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
- Zsh repository:
  - repo-owned syntax, unit, and smoke checks, expected to include
    `./tools/check.zsh` and `./tools/check.zsh --smoke` if those remain the
    current entrypoints.
- agent-runtime-kit:
  - Docker build smoke proving `zsh`, `zsh-kit`, and existing core CLIs are
    available.
  - `zsh-kit setup --dry-run` inside the image.
  - `bash scripts/ci/all.sh`
  - `bash tests/hooks/run.sh`

## Risks And Guardrails

- **Risk**: leaking private repository names, tokens, or local paths into public
  nils-cli output or tracker records.
  **Guardrail**: keep examples generic in public docs; redact or reject
  credential-bearing URLs in diagnostics.
- **Risk**: nils-cli becomes responsible for Zsh behavior and drifts from the
  real shell repository.
  **Guardrail**: nils-cli only orchestrates clone/update/bootstrap/dispatch;
  the Zsh repository owns the setup hook and shell semantics.
- **Risk**: container setup requires root, Homebrew, apt, or interactive
  package installation.
  **Guardrail**: default tool install policy is `skip`; repo-owned installs are
  explicit.
- **Risk**: writing `~/.zshenv` damages an existing operator shell setup.
  **Guardrail**: make `--write-zshenv` explicit, preview it in dry-run, create
  backups or refuse without `--force`, and test conflict handling.

## Read-First References

- `AGENTS.md`
- `BINARY_DEPENDENCIES.md`
- `docs/runbooks/new-cli-crate-development-standard.md`
- `docs/runbooks/cli-completion-development-standard.md`
- `docs/specs/crate-docs-placement-policy.md`
- `docs/specs/completion-coverage-matrix-v1.md`
- `scripts/workspace-bins.sh`
- `release/crates-io-publish-order.txt`
