# agent-scope-lock

## Overview

`agent-scope-lock` creates and validates deterministic edit-scope locks for agent workflows. It is designed for skills that need a
mechanical boundary around which repo paths an agent may change.

The default lock file is stored outside tracked files through:

```bash
git rev-parse --git-path agent-scope-lock.json
```

Pass `--lock-file PATH` to use an explicit file for tests or advanced workflows.

## Package vs binary name

| Field        | Value                   |
| ------------ | ----------------------- |
| Package name | `nils-agent-scope-lock` |
| Binary name  | `agent-scope-lock`      |

## Usage

```text
Create and validate deterministic agent edit-scope locks.

Usage: agent-scope-lock <COMMAND>

Commands:
  create      Create a lock with allowed repo-relative path prefixes
  read        Read the current lock
  validate    Validate changed paths against the current lock
  clear       Remove the current lock if present
  completion  Print shell completion script

Options:
  -h, --help     Print help
  -V, --version  Print version
```

Examples:

```bash
agent-scope-lock create --path crates/agent-scope-lock --path Cargo.toml --owner T3
agent-scope-lock read
agent-scope-lock validate --changes all
agent-scope-lock validate --format json
agent-scope-lock clear
```

## Commands

- `create --path PATH [--path PATH ...] [--owner OWNER] [--note NOTE] [--force] [--lock-file PATH] [--format text|json]`:
  create a lock file with repo-relative allowed prefixes. Existing locks are not overwritten unless `--force` is passed.
- `read [--lock-file PATH] [--format text|json]`: print the current lock.
- `validate [--changes all|staged|unstaged] [--lock-file PATH] [--format text|json]`: compare changed git paths against the lock. `all`
  includes staged, unstaged, and untracked paths.
- `clear [--lock-file PATH] [--format text|json]`: remove the lock idempotently.
- `completion <bash|zsh>`: print clap-generated shell completions.

## Output contract

Human-readable text is the default. JSON is opt-in with `--format json` on command subcommands.

JSON output uses versioned envelopes:

```json
{
  "schema_version": "cli.agent-scope-lock.validate.v1",
  "command": "agent-scope-lock validate",
  "ok": true,
  "result": {
    "lock_file": "/repo/.git/agent-scope-lock.json",
    "mode": "all",
    "allowed_paths": ["crates/agent-scope-lock"],
    "changed_paths": ["crates/agent-scope-lock/src/lib.rs"],
    "violations": []
  }
}
```

Failure envelopes use `ok=false` with stable `error.code`, `error.message`, and optional `error.details`.

Exit codes:

- `0`: success
- `1`: runtime failure or scope violation
- `64`: usage/configuration error

## Secret-safety boundary

`validate` inspects changed path names from git only. It does not read changed file contents, so secret-like files can appear as path
violations without expanding their contents into stdout, stderr, or JSON.

## Docs

- [Docs index](docs/README.md)
- [CLI service JSON contract guideline](../../docs/specs/cli-service-json-contract-guideline-v1.md)
- [New CLI crate development standard](../../docs/runbooks/new-cli-crate-development-standard.md)
