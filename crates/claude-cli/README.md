# claude-cli

## Overview

`claude-cli` is a provider-specific Rust CLI for Claude-oriented helpers that should not live in shell glue. The initial surface owns
Claude Code prompt-segment rendering, including Keychain credential lookup, usage refresh, cache fallback, and completion export.

## Usage

```text
Usage:
  claude-cli prompt-segment [options]
  claude-cli prompt-segment check
  claude-cli prompt-segment status [--format text|json]
  claude-cli completion <bash|zsh>

Help:
  claude-cli help
  claude-cli prompt-segment --help
```

## Scope boundary

| Job | Primary owner |
| --- | --- |
| Claude prompt-segment auth, cache refresh, usage rendering, completion export | `claude-cli` |
| Shell aliases, Starship module wiring, PATH/fpath registration, wrapper dispatch | zsh-kit shell glue |

`claude-cli` owns provider-specific Claude behavior. zsh-kit should keep only the small compatibility wrapper and shell integration.

## Commands

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

### completion

- `completion <bash|zsh>`: Export shell completion script to stdout.

## Environment

- `CLAUDE_PROMPT_TTL` or `CLAUDE_PROMPT_SEGMENT_TTL` overrides the cache TTL. The default is `60` seconds. `0` forces refresh.
- `CLAUDE_PROMPT_STALE_SUFFIX` or `CLAUDE_PROMPT_SEGMENT_STALE_SUFFIX` controls stale cache suffix text. The default is one leading
  space followed by `(stale)`.
- `CLAUDE_PROMPT_SEGMENT_CACHE_DIR` overrides the cache directory.
- `CLAUDE_PROMPT_SEGMENT_ENDPOINT` overrides the usage endpoint. The default is `https://api.anthropic.com/api/oauth/usage`.
- `CLAUDE_PROMPT_SEGMENT_ACCESS_TOKEN` or `CLAUDE_PROMPT_SEGMENT_CREDENTIALS_JSON` can supply credentials for automation.
- `CLAUDE_PROMPT_SEGMENT_KEYCHAIN_DISABLED=1` disables macOS Keychain lookup.
- `CLAUDE_PROMPT_SEGMENT_KEYCHAIN_SERVICE` overrides the macOS Keychain service name. The default is `Claude Code-credentials`.
- `NO_COLOR=1` disables ANSI color.

## Dependencies

- macOS `security` is used for Keychain credential lookup unless an automation credential override is supplied.
- No `curl`, `jq`, or Python runtime is required for prompt-segment rendering.

## Exit codes

- `0`: success, help output, or no prompt output needed.
- `1`: operational false/failed state such as `prompt-segment check` without credentials.
- `64`: usage or argument errors.

## Docs

- [Docs index](docs/README.md)
