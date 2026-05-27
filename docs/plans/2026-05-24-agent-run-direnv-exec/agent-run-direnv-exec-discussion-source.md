# Agent Run Direnv Exec Implementation Handoff

- Status: open, ready for issue-backed implementation
- Date: 2026-05-24
- Source: converged discussion about making agent-executed project commands use
  the same project environment that developers rely on for local development
  and testing.
- Intended next step: open a follow-up issue, then create and execute the
  paired implementation plan.

## Execution

- Recommended plan: docs/plans/agent-run-direnv-exec/agent-run-direnv-exec-plan.md
- Recommended execution state: docs/plans/agent-run-direnv-exec/agent-run-direnv-exec-execution-state.md

## Purpose

Agents run commands from a non-interactive shell. That is close to the user's
machine environment, but it is not guaranteed to load the same interactive
shell hooks, aliases, functions, or project-specific environment activation.

For many repositories, `direnv` is the project-local source of truth for
development and test environment variables. Agent validation can become
misleading when commands bypass `.envrc` or `.env` activation. The target is a
released `nils-cli` primitive that gives agents one stable way to run project
commands through the same environment contract developers expect.

## Confirmed Facts

- Agent shell commands can run under a non-interactive shell, so interactive
  shell hooks such as `eval "$(direnv hook zsh)"` are not a reliable contract
  for command execution.
- `direnv exec DIR COMMAND ...` is the direct command form for loading the
  first `.envrc` or `.env` found for a directory and then executing a command.
- `nils-cli` already ships agent workflow primitives in
  `crates/agent-workflow-primitives`, including evidence and workflow binaries
  such as `skill-usage`, `review-evidence`, `docs-impact`, and related tools.
- `agent-runtime` is currently scoped to rendering, installing, diagnosing, and
  auditing `agent-runtime-kit` runtime surfaces. It is not the right first
  home for arbitrary project command execution.
- The workspace binary inventory is derived from Cargo metadata through
  `scripts/workspace-bins.sh`, so adding a binary target is the canonical way
  to add a user-facing CLI surface.
- This document is a transient plan-source record under `docs/plans/`, which is
  allowed by the crate docs placement policy for implementation coordination.

## Decisions

1. Add a new `agent-run` binary under `nils-agent-workflow-primitives`.
2. Treat `agent-run` as an environment-normalizing command executor, not as a
   task runner, shell replacement, or provider adapter.
3. Keep `agent-runtime` integration limited to later diagnostics, such as
   probing `agent-run doctor`; do not make `agent-runtime` own command
   execution.
4. Make `direnv` handling explicit and fail-closed:
   - default mode: `--direnv auto`;
   - direct execution when no `.envrc` or `.env` is found;
   - `direnv exec` when a project environment file is found and `direnv` is
     available;
   - failure when a project environment file exists but `direnv` is missing or
     blocked.
5. Never run `direnv allow` automatically. Trusting an `.envrc` is a user
   decision because it is arbitrary code execution.
6. Preserve child command stdout, stderr, and exit code by default.
7. Provide machine-readable status so skills can explain whether a command ran
   with project env, direct env, or a blocked/missing environment.

## Scope

In scope:

- `agent-run` binary target in `crates/agent-workflow-primitives`.
- CLI commands:
  - `agent-run exec --cwd <dir> [--direnv auto|require|off] -- <command> ...`
  - `agent-run doctor --cwd <dir> --format text|json`
  - `agent-run env --cwd <dir> --format json`
- Deterministic `.envrc` / `.env` discovery for the selected working
  directory.
- `direnv status --json` and `direnv exec` integration where available.
- Structured JSON envelopes for doctor/env output.
- Stable exit-code mapping for usage errors, environment failures, and child
  command failures.
- Completion generation and root `-V, --version` support following workspace
  conventions.
- Documentation that tells agent-facing skills to use `agent-run exec` for
  project build, test, and validation commands when available.

Out of scope:

- Automatic `direnv allow`, `direnv edit`, or any trust mutation.
- Replacing project scripts such as `scripts/check.sh`, `npm test`, or
  `uv run pytest`.
- Defining repository task aliases, package-manager autodetection, or a
  generalized task registry.
- Emulating a fully interactive login shell.
- Persisting env changes across unrelated agent tool calls.
- Provider-specific behavior for Codex, Claude, Gemini, GitHub, or GitLab.
- Browser or desktop automation.

## Requirements

- `agent-run exec --cwd . -- <command>` must execute the command with the
  selected working directory.
- `--direnv auto` must be the default.
- `--direnv off` must bypass `direnv` even when `.envrc` exists and report that
  bypass in JSON status when requested.
- `--direnv require` must fail when no `.envrc` or `.env` applies to the
  selected directory.
- If `.envrc` or `.env` applies and `direnv` is unavailable, `agent-run` must
  fail before running the child command.
- If `direnv` reports the environment file is blocked or not allowed,
  `agent-run` must fail before running the child command and print the path
  that needs user review.
- `agent-run` must pass command arguments as argv, not by shell-string
  concatenation.
- Child command stdout and stderr must be streamed without wrapping by default.
- Child exit status must be preserved when the child process starts and exits
  normally.
- Wrapper-level errors must use stable exit codes and structured JSON error
  envelopes where `--format json` is supported.
- The implementation must not hard-code user-local paths.

## Acceptance Criteria

- `cargo run -p nils-agent-workflow-primitives --bin agent-run -- --help`
  shows `exec`, `doctor`, `env`, `completion`, and root `-V, --version`.
- `agent-run exec --cwd <repo-without-envrc> -- <command>` executes directly
  and returns the child exit code.
- `agent-run exec --cwd <repo-with-allowed-envrc> -- <command>` runs through
  `direnv exec`.
- `agent-run exec --cwd <repo-with-blocked-envrc> -- <command>` fails without
  running the child command and reports the blocked file.
- `agent-run exec --direnv require --cwd <repo-without-envrc> -- <command>`
  fails before running the child command.
- `agent-run exec --direnv off --cwd <repo-with-envrc> -- <command>` runs the
  child command directly and records that `direnv` was bypassed.
- `agent-run doctor --cwd <dir> --format json` emits a stable
  `agent-run.doctor.v1` envelope with cwd, direnv availability, env-file state,
  selected mode, and decision.
- `agent-run env --cwd <dir> --format json` emits enough information for an
  agent to state whether project env is active, absent, blocked, or bypassed.
- `bash scripts/workspace-bins.sh` includes `agent-run`.
- Generated bash and zsh completions are syntactically valid.
- Docs-only and full workspace gates remain green for the touched surface.

## Validation Plan

- `cargo test -p nils-agent-workflow-primitives agent_run`
- `cargo test -p nils-agent-workflow-primitives --test integration agent_run`
- `cargo run -p nils-agent-workflow-primitives --bin agent-run -- --help`
- `cargo run -p nils-agent-workflow-primitives --bin agent-run -- doctor --cwd . --format json`
- `cargo run -p nils-agent-workflow-primitives --bin agent-run -- exec --cwd . -- sh -c 'pwd'`
- `bash scripts/workspace-bins.sh | grep '^agent-run$'`
- `zsh -n completions/zsh/_agent-run`
- `bash -n completions/bash/agent-run`
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only`
- `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage`

## Risks And Guardrails

- Do not trade environment parity for hidden trust mutation. A blocked `.envrc`
  must stop execution until the user explicitly allows it outside `agent-run`.
- Do not hide environment decisions. Agents need to know whether validation ran
  through `direnv`, direct env, or an intentional bypass.
- Do not turn v1 into a repo task runner. The first release should normalize
  execution environment only.
- Do not require `direnv` for repositories that do not declare project env
  files.
- Do not make JSON output depend on localized `direnv` text when a more stable
  signal is available.
- Avoid shell injection hazards by preserving argv boundaries.
- Keep provider-specific agent behavior in skills or provider CLIs, not in
  `agent-run`.

## Resolved V1 Decisions

- `agent-run env --format json` reports status, paths, selected mode, and the
  final execution decision only. It does not include an environment diff in v1.
  If consumers later need diff visibility, add an explicit opt-in such as
  `--include-env-diff` after defining the redaction contract.
- `agent-run exec` does not print a one-line stderr preface on successful normal
  execution. Wrapper output is reserved for warnings and errors; normal
  environment decisions are exposed through `agent-run doctor` and
  `agent-run env`.
- `agent-runtime doctor --check-project` does not probe `agent-run` in the first
  implementation PR. Defer that integration until `agent-run` is released and
  at least one agent-facing skill has adopted it.

## Read-First References

- `README.md`
- `DEVELOPMENT.md`
- `BINARY_DEPENDENCIES.md`
- `scripts/workspace-bins.sh`
- `crates/agent-workflow-primitives/Cargo.toml`
- `crates/agent-workflow-primitives/src/skill_usage.rs`
- `crates/agent-runtime-cli/src/lib.rs`
- `docs/specs/crate-docs-placement-policy.md`

## Retention Intent

This source doc is execution coordination. It can be removed with the sibling
plan bundle after implementation is complete and durable CLI contract details
have been promoted into crate docs, runbooks, or the workspace README as
needed.
