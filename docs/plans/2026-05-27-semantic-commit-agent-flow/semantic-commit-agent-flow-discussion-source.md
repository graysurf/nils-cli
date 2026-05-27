# Discussion Source: `semantic-commit` Agent Commit Flow

## Source Summary

User intent: make `semantic-commit` the complete agent-facing commit
surface for common commit creation and commit-editing flows, so agents do
not need to fall back to direct `git commit` for amend, structured
message construction, trailers, JSON result capture, or fixup/squash
commits.

## Current State

- `semantic-commit staged-context` emits staged change context in bundle,
  JSON, or patch form.
- `semantic-commit commit` validates Semantic Commit messages and commits
  staged changes.
- Existing `commit` options include inline or file message input,
  `--message-out`, summary modes, `--repo`, `--max-header-width`,
  automation mode, `--validate-only`, `--dry-run`, `--auto-fix`,
  `--no-progress`, and `--quiet`.
- The current commit execution path runs `git commit -F <message>` and
  does not support amend, message-only amend, fixup, squash, trailers,
  signoff, structured JSON output, or structured message assembly.

## Target Outcome

Agents can use `semantic-commit` for the full set of commit operations
that are part of normal agent-owned development workflows:

- create a Semantic Commit from staged changes
- amend the previous commit, either with a new validated message or with
  `--no-edit`
- edit only the previous commit message
- produce machine-readable commit results after create or amend
- add standard trailers and signoff without hand-editing the message
- build a validated message from structured CLI fields
- create fixup and squash commits for review cleanup workflows

## Explicit Non-Goals

- Do not turn `semantic-commit` into a generic `git` wrapper.
- Do not add raw passthrough options such as `--git-arg`.
- Do not add a generic `--no-verify` bypass.
- Do not add push, force-push, rebase, or PR delivery behavior.
- Do not add implicit staging or hidden `git add -A` behavior.

## Design Notes

- Keep `commit` as the Semantic Commit surface. Add amend, JSON output,
  guard flags, trailers, and structured message fields there.
- Add fixup and squash as separate subcommands because their generated
  commit subjects intentionally use `fixup!` / `squash!` prefixes and
  should not pass through Semantic Commit header validation.
- Preserve the existing staged-only boundary by default. Any empty commit
  or message-only amend behavior must be explicit.
- Prefer structured output that agents can parse directly instead of
  requiring follow-up `git rev-parse`, `git show`, or ad hoc parsing.

## Open Questions

None for initial implementation. Names may be adjusted during execution
when a clearer CLI contract emerges from tests and help text.

## Execution

- Recommended plan:
  docs/plans/semantic-commit-agent-flow/semantic-commit-agent-flow-plan.md
- Recommended execution state:
  docs/plans/semantic-commit-agent-flow/semantic-commit-agent-flow-execution-state.md
