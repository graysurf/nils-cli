# semantic-commit

## Overview

`semantic-commit` is the agent-facing commit helper for staged-change
workflows. It validates Semantic Commit messages, creates and amends commits,
emits staged context for message generation, and supports machine-readable
commit results for automation.

The CLI is intentionally not a generic `git` wrapper. It owns commit-message
validation and commit mutation. Callers still own staging decisions, task
scope, branch policy, push/PR delivery, and any higher-level workflow checks.

## Commands

```text
Usage: semantic-commit [COMMAND]

Commands:
  staged-context  Print staged change context for commit message generation
  commit          Commit staged changes with a prepared commit message
  local-default   Create one governed signed commit on the primary local default branch
  fixup           Create a fixup! commit for staged changes
  squash          Create a squash! commit for staged changes
  completion      Export shell completion script
  help            Display help message
```

Top-level options:

- `-h`, `--help` - print help
- `-V`, `--version` - print version

Use `semantic-commit <command> --help` for command-specific help.

## `staged-context`

Print staged change context for commit message generation.

```text
semantic-commit staged-context [--format <bundle|json|patch>] [--repo <path>]
```

Options:

- `--format <bundle|json|patch>` - output format; default is `bundle`
- `--json` - equivalent to `--format json`
- `--repo <path>` - run git commands against a repository path

Formats:

- `bundle`: prints `commit-context.json` and `staged.patch` sections.
- `json`: prints only the JSON context payload.
- `patch`: prints only `git diff --cached`.

## `commit`

Create a new commit or amend `HEAD` using a validated Semantic Commit message.

```text
semantic-commit commit [--message <text>|--message-file <path>] [options]
```

Message sources are mutually exclusive:

- `-m`, `--message <text>` - inline commit message
- `-F`, `--message-file <path>` - read commit message from a file
- stdin - used when no message flag is supplied and stdin is non-TTY
- structured fields - `--type`, optional `--scope`, `--subject`, and
  repeatable `--body-bullet`

Commit operation options:

- `--amend` - amend `HEAD` instead of creating a new commit
- `--no-edit` - with `--amend`, reuse and validate the `HEAD` message
- `--message-only` - with `--amend`, update only the `HEAD` message and
  require no staged changes
- `--allow-empty` - allow a commit operation without staged changes
- `--dry-run` - validate message and staged-state checks without committing
- `--validate-only` - validate only the message; does not require a git repo

Output and recovery options:

- `--format <text|json>` - output mode; default is `text`
- `--json` - equivalent to `--format json`
- `--message-out <path>` - write the resolved message before mutation
- `--summary <git-scope|git-show|none>` - summary mode; default is
  `git-scope` with fallback to `git-show`
- `--no-summary` - equivalent to `--summary none`
- `--no-progress` - disable the progress spinner
- `--quiet` - suppress progress and text summary output

Safety options:

- `--repo <path>` - run git commands against a repository path
- `--require-clean` - require no unstaged or untracked changes
- `--no-unstaged` - alias for `--require-clean`
- `--expect-head <rev>` - require `HEAD` to match a revision before mutation
- `--automation`, `--non-interactive` - disallow stdin message fallback

Message-construction options:

- `--auto-fix` - normalize body wrapping, bullet capitalization, header
  type/scope case, and the blank separator before validation
- `--max-header-width <N>` - override the active header width; default is
  `100`, with `SEMANTIC_COMMIT_HEADER_WIDTH` as an environment default
- `--signoff` - pass `--signoff` to `git commit`
- `--trailer <token: value>` - add a git trailer; repeatable
- `--type <type>` - structured message type
- `--scope <scope>` - structured message scope
- `--subject <subject>` - structured message subject
- `--body-bullet <text>` - structured message body bullet; repeatable

## `local-default`

Create exactly one signed, local-only commit in the primary checkout. The
mutating form requires `--expect-head <full-sha>`, `--expected-branch <name>`,
and a new `--receipt-out <outside-repository-path>`. If any remote is
configured, it also requires `--remote-mode local-only`. The command never
contacts a remote.

It requires staged changes with no unstaged or untracked paths, an attached
matching branch, no in-progress Git operation, and an aligned cached upstream
or no upstream. It verifies the created signature and parent/tree/clean-state
postconditions, then atomically writes a
`cli.semantic-commit.local-default.v1` receipt. A post-commit receipt failure
is reported as partial success and never triggers an automatic reset.

Examples:

```bash
semantic-commit staged-context

semantic-commit commit \
  --message "feat(cli): add commit result json"

semantic-commit commit \
  --amend \
  --no-edit \
  --expect-head HEAD \
  --require-clean

semantic-commit commit \
  --amend \
  --message-only \
  --message "fix(cli): clarify amend help"

semantic-commit commit \
  --type feat \
  --scope semantic-commit \
  --subject "support amend flow" \
  --body-bullet "Add no-edit amend support." \
  --body-bullet "Return JSON commit metadata." \
  --json

semantic-commit commit \
  --message "fix(cli): link tracker" \
  --trailer "Refs: #573" \
  --signoff
```

## `fixup` and `squash`

Create review-cleanup commits without bypassing the `semantic-commit` staged
checks and machine-readable output contract.

```text
semantic-commit fixup --target <rev> [options]
semantic-commit squash --target <rev> [options]
```

These subcommands call git's cleanup-commit modes and intentionally do not
validate the generated `fixup!` / `squash!` subject as a Semantic Commit
header.

Options:

- `--target <rev>` - target commit revision; required
- `--dry-run` - validate target and staged checks without committing
- `--format <text|json>`, `--json` - output mode
- `--summary <git-scope|git-show|none>`, `--no-summary` - text summary
- `--allow-empty` - allow a cleanup commit without staged changes
- `--require-clean`, `--no-unstaged` - require no unstaged or untracked
  changes
- `--expect-head <rev>` - require `HEAD` to match a revision before mutation
- `--repo <path>` - run git commands against a repository path
- `--no-progress`, `--quiet` - progress and summary controls

Examples:

```bash
semantic-commit fixup --target HEAD~1
semantic-commit squash --target abc123 --json --dry-run
```

## JSON Output

`semantic-commit commit --json`, `semantic-commit fixup --json`, and
`semantic-commit squash --json` emit a single JSON record on success:

```json
{
  "schema_version": "cli.semantic-commit.commit.v1",
  "ok": true,
  "operation": "commit",
  "validate_only": false,
  "dry_run": false,
  "commit": {
    "sha": "012345...",
    "subject": "feat(cli): add commit result json"
  },
  "target": null,
  "staged": {
    "file_count": 1,
    "files": [
      {
        "status": "M",
        "path": "crates/semantic-commit/src/commit.rs",
        "old_path": null
      }
    ]
  }
}
```

For `fixup` and `squash`, the record also includes the resolved target commit
and generated subject.

## Commit Message Validation

Semantic Commit validation applies to `semantic-commit commit` message input:

- Header format is `type(scope): subject` or `type: subject`.
- Header type must be lowercase and start with an ASCII lowercase letter.
- Scope, when present, may contain lowercase letters, digits, `.`, `_`, and
  `-`.
- Header length must not exceed the active header width.
- Body lines must be bullet lines that start with `-` plus a space followed by an
  uppercase ASCII letter, or continuation lines that start with two spaces.
- Body and trailer lines must be at most `100` characters.
- Git trailers may appear after the header or after a blank separator
  following bullet body lines.
- Blocked message rules reject known unwanted agent attribution text, including
  `Co-Authored-By: Claude ...` trailers from either message input or
  `--trailer`.

`fixup` and `squash` do not use this header validation because git generates
subjects prefixed with `fixup!` or `squash!`.

## Exit Codes

- `0`: success and help output.
- `1`: usage errors or operational errors.
- `2`: no staged changes.
- `3`: commit message missing or empty.
- `4`: commit message validation failed.
- `5`: required dependency missing, such as `git`.

## Dependencies

- `git` is required.
- `git-scope` is optional. When unavailable, text summaries fall back to
  `git show -1`.

## Non-Goals

`semantic-commit` does not push, force-push, rebase, create PRs, choose branch
names, or stage files implicitly. Those decisions belong to the surrounding
agent workflow.

## Docs

- [Docs index](docs/README.md)
