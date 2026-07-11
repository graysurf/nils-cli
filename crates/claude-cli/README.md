# claude-cli

## Overview

`claude-cli` is a provider-specific Rust CLI for Claude-oriented helpers that should not live in shell glue. The surface owns
Claude Code prompt-segment rendering, usage source selection, Keychain credential lookup, cache fallback, and completion export.

## Usage

```text
Usage:
  claude-cli agent resume <SESSION_ID> [--cd <dir>]
  claude-cli prompt-segment [options]
  claude-cli prompt-segment check
  claude-cli prompt-segment status [--format text|json]
  claude-cli usage [--format text|json] [--source auto|oauth|cli|cache]
  claude-cli completion <bash|zsh>

Help:
  claude-cli help
  claude-cli agent --help
  claude-cli prompt-segment --help
```

## Scope boundary

| Job | Primary owner |
| --- | --- |
| Claude prompt-segment auth, usage source selection, cache refresh, usage rendering, completion export | `claude-cli` |
| Shell aliases, Starship module wiring, PATH/fpath registration, wrapper dispatch | zsh-kit shell glue |

`claude-cli` owns provider-specific Claude behavior. zsh-kit should keep only the small compatibility wrapper and shell integration.

## Commands

### agent

- `agent resume <SESSION_ID> [--cd <dir>]`: Resolve the session's recorded working directory from local Claude Code project history and
  launch `claude --resume <SESSION_ID>` in that directory, propagating Claude's exit status. Claude Code has no `--cd` flag and stores
  sessions per project, so the recorded directory is applied as the child process working directory. Run it from any directory. Fails
  without launching Claude (`65`) when the id is unknown or matches more than one recorded directory; pass `--cd` to override the resolved
  directory for a repository that moved.

### prompt-segment

- `prompt-segment [--ttl <duration>] [--time-format <strftime>] [--refresh] [--is-enabled]`: Render Claude 5h / weekly usage.
- `prompt-segment check`: Exit `0` when a Claude OAuth access token is available, otherwise `1`.
- `prompt-segment status [--format text|json]`: Report readiness and cache state without exposing token material.

The output mirrors the former zsh-kit `claude-prompt-segment` helper:

```text
5h:<remaining>% W:<remaining>% <weekly_reset_time>[<stale_suffix>]
```

The cache remains compatible with the former shell script:

```text
~/Library/Caches/claude-prompt-segment/usage.json
```

### usage

- `usage [--format text|json] [--source auto|oauth|cli|cache]`: Read Claude
  usage through a service-consumable contract.
- `--source auto`: Try OAuth usage refresh, then a bounded Claude CLI `/usage`
  probe, then last-good cache.
- `--source oauth`, `--source cli`, and `--source cache`: Run one source only for
  focused debugging.

JSON output uses `schema_version: "claude-cli.usage.v1"` and never includes
tokens or credential material. The result includes `provider: "claude"` and a
`windows` array containing 5h and weekly windows when available, each with
used/remaining percentages and optional reset timestamps. CLI-derived usage is
normalized back into the same cache shape used by `prompt-segment`.
When live usage is unavailable, the result may include additive `reason_code`
with one of the shared provider-neutral values: `auth_required`, `auth_expired`,
`billing_past_due`, `subscription_inactive`, `organization_disabled`,
`permission_denied`, `rate_limited`, `service_unavailable`, `timeout`, or
`unknown`. Provider response bodies and terminal error text are classified
locally and are never copied into the JSON envelope.

### completion

- `completion <bash|zsh>`: Export shell completion script to stdout.

## Environment

- `CLAUDE_PROMPT_TTL` or `CLAUDE_PROMPT_SEGMENT_TTL` overrides the cache TTL. The default is `60` seconds. `0` forces refresh.
- `CLAUDE_PROMPT_STALE_SUFFIX` or `CLAUDE_PROMPT_SEGMENT_STALE_SUFFIX` controls stale cache suffix text. The default is one leading
  space followed by `(stale)`.
- `CLAUDE_PROMPT_SEGMENT_CACHE_DIR` overrides the cache directory.
- `CLAUDE_PROMPT_SEGMENT_ENDPOINT` overrides the usage endpoint. The default is `https://api.anthropic.com/api/oauth/usage`.
- `CLAUDE_PROMPT_SEGMENT_ACCESS_TOKEN` or `CLAUDE_PROMPT_SEGMENT_CREDENTIALS_JSON` can supply credentials for automation.
- `CLAUDE_PROMPT_SEGMENT_CLAUDE_BIN` overrides the Claude CLI binary used by the
  `usage --source cli` fallback. The default is `claude`.
- `CLAUDE_PROMPT_SEGMENT_CLAUDE_TIMEOUT_SECONDS` overrides the bounded CLI usage
  probe timeout. The default is `15` seconds.
- `CLAUDE_PROMPT_SEGMENT_CLAUDE_PTY_DISABLED=1` disables the Unix PTY wrapper and
  pipes slash commands directly to the Claude binary.
- `CLAUDE_PROMPT_SEGMENT_CLAUDE_PTY_STARTUP_DELAY_MS` and
  `CLAUDE_PROMPT_SEGMENT_CLAUDE_PTY_USAGE_DELAY_MS` tune the startup and
  post-`/usage` waits for interactive Claude Code. Defaults are `4000` and
  `3000` milliseconds.
- `CLAUDE_PROMPT_SEGMENT_KEYCHAIN_DISABLED=1` disables macOS Keychain lookup.
- `CLAUDE_PROMPT_SEGMENT_KEYCHAIN_SERVICE` overrides the macOS Keychain service name. The default is `Claude Code-credentials`.
- `NO_COLOR=1` disables ANSI color.

## Dependencies

- macOS `security` is used for Keychain credential lookup unless an automation credential override is supplied.
- Nested CLI probe descendants are enumerated and terminated on timeout using
  Linux `/proc` or a time-bounded `/bin/ps` snapshot on other Unix platforms.
  Unix `script` enables the richer PTY probe path.
- No `curl`, `jq`, or Python runtime is required for prompt-segment rendering.
- `claude` is required for `agent resume`.
- `agent resume` reads local Claude Code project history under `$CLAUDE_CONFIG_DIR/projects` (default `~/.claude/projects`); the shared
  resolver lives in `nils-provider-resume`.

## Exit codes

- `0`: success, help output, or no prompt output needed.
- `1`: operational false/failed state such as `prompt-segment check` without credentials.
- `64`: usage or argument errors.
- `65`: `agent resume` could not resolve the session id (unknown or ambiguous).

## Docs

- [Docs index](docs/README.md)
