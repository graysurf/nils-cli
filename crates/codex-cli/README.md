# codex-cli

## Overview

codex-cli is a provider-specific Rust CLI for OpenAI/Codex workflows: Codex execution wrappers, auth/secret management, Codex diagnostics,
config output, and prompt-segment rendering. Runtime wiring is owned by `codex-cli` adapters with shared `nils-common::provider_runtime` helpers
for common primitives.

## Usage

```text
Usage:
  codex-cli <group> <command> [args]
  codex-cli prompt-segment [options]
  codex-cli completion <shell>

Groups:
  agent           prompt | advice | knowledge | commit
  auth            login | use | save | remove | refresh | auto-refresh | current | sync
  diag            rate-limits
  config          show | set
  prompt-segment  (options)
  completion      bash | zsh

Help:
  codex-cli help
  codex-cli <group> help
```

## Scope boundary

| Job                                                                                              | Primary owner                                          |
| ------------------------------------------------------------------------------------------------ | ------------------------------------------------------ |
| Shared provider runtime helpers (`auth/path/config/exec/error`)                                  | `nils-common::provider_runtime` + `codex-cli` adapters |
| OpenAI/Codex auth, Codex prompt wrappers, Codex rate-limit diagnostics, prompt-segment rendering | `codex-cli`                                            |
| Unsupported commands/groups                                                                      | clap usage error (`64`)                                |

- `codex-cli` owns only provider-specific OpenAI/Codex operations (`agent`, `auth`, `diag rate-limits`, `config`, `prompt-segment`, `completion`).
- Existing `codex-cli` commands stay stable for provider-specific workflows.
- Unknown groups/subcommands are deterministic usage errors (`64`).

## Commands

### agent

- `prompt [--ephemeral] [PROMPT...]`: Run a raw prompt through `codex exec`.
- `advice [--ephemeral] [QUESTION...]`: Request actionable engineering advice.
- `knowledge [--ephemeral] [CONCEPT...]`: Request a concept explanation.
- `commit [--ephemeral] [-p|--push] [-a|--auto-stage] [EXTRA...]`: Run the semantic-commit workflow.

Agent flag notes:

- `--ephemeral`: Forward `codex exec --ephemeral` so Codex does not persist session files for that run.

### auth

- `login [--api-key|--device-code]`: Login via ChatGPT browser flow (`chatgpt-browser`, default), ChatGPT device-code flow
  (`chatgpt-device-code`), or API key flow (`api-key`). `--api-key` and `--device-code` are mutually exclusive (`64` on invalid usage).
- `use <name|name.json|email>`: Switch to a secret by name/name.json or email.
- `save [--yes] <secret|secret.json>`: Save active `CODEX_AUTH_FILE` into `CODEX_SECRET_DIR`. Secret files are normalized to `.json`; if
  target exists, interactive mode prompts for overwrite, while non-interactive and JSON mode require `--yes` to overwrite.
- `remove [--yes] <secret|secret.json>`: Remove a secret file from `CODEX_SECRET_DIR`. Secret names are normalized to `.json`; interactive
  mode prompts for confirmation, while non-interactive and JSON mode require `--yes`.
- `refresh [secret.json]`: Refresh OAuth tokens.
- `auto-refresh`: Refresh stale tokens across auth + secrets.
- `current`: Show which secret matches `CODEX_AUTH_FILE`.
- `sync`: Sync `CODEX_AUTH_FILE` back into matching secrets.

Auth examples:

- `codex-cli auth login`: ChatGPT browser login.
- `codex-cli auth login --device-code`: ChatGPT device-code login.
- `codex-cli auth login --api-key`: OpenAI API key login.
- `codex-cli auth save team-alpha`: Save to `team-alpha.json` and prompt before overwrite when applicable.
- `codex-cli auth save --yes team-alpha.json`: Force overwrite without prompt.
- `codex-cli auth remove --yes team-alpha`: Remove `team-alpha.json`.

### diag

- `rate-limits [options] [secret.json]`: Rate-limit diagnostics. Options: `-c/--clear-cache`, `-d/--debug`, `--cached`,
  `--no-refresh-auth`, `--format <text|json>`, `--json`, `--one-line`, `--all`, `--async`, `--watch`, `--jobs <n>`.
- `--cached` reads cache only. Freshness is controlled by `CODEX_RATE_LIMITS_CACHE_TTL` (default `3m`); stale cache is rejected unless
  `CODEX_RATE_LIMITS_CACHE_ALLOW_STALE=true`.
- `--watch` refreshes output every 60 seconds until interrupted and requires `--async`.

### config

- `show`: Print effective configuration values.
- `set <key> <value>`: Emit a shell snippet for the current shell.

### prompt-segment

- `prompt-segment [--no-5h] [--ttl <duration>] [--time-format <strftime>] [--show-timezone] [--refresh] [--is-enabled]`: Render or refresh
  the prompt segment. Default reset time uses local time without timezone; `--show-timezone` adds the local offset.

### completion

- `completion <bash|zsh>`: Print the shell completion script for the requested shell to stdout.

## JSON contract (service consumers)

- Human-readable text is the default output mode.
- Machine-readable JSON mode is explicit: use `--format json` (preferred) or `--json` where supported for compatibility.
- Contract spec: `docs/specs/codex-cli-diag-rate-limits-and-auth-json-contract-v1.md`
- Consumer runbook: `docs/runbooks/json-consumers.md`
- Covered surfaces: `diag rate-limits` (single/all/async) and `auth login|use|save|remove|refresh|auto-refresh|current|sync`.

## Environment

- `CODEX_ALLOW_DANGEROUS_ENABLED`: gate for `agent` commands (default: `false`).
- `CODEX_CLI_MODEL`: `codex exec` default model (default: `gpt-5.1-codex-mini`).
- `CODEX_CLI_REASONING`: `codex exec` default reasoning level (default: `medium`).
- `CODEX_CLI_EPHEMERAL_ENABLED`: append `--ephemeral` to `codex exec` for agent commands (default: `false`).
- `CODEX_SECRET_DIR`: secret directory path (default: `~/.config/codex_secrets`).
- `CODEX_AUTH_FILE`: active auth file path (default: `~/.agents/auth.json`).
- `CODEX_SECRET_CACHE_DIR`: secret timestamp cache directory. If unset, resolver order is:
  `ZSH_CACHE_DIR/codex/secrets` -> `ZDOTDIR/cache/codex/secrets` -> `~/.config/zsh/cache/codex/secrets`.
- `CODEX_RATE_LIMITS_CACHE_TTL`: `diag rate-limits --cached` TTL (default: `3m`; supports `s|m|h|d|w` suffixes or raw seconds).
- `CODEX_RATE_LIMITS_CACHE_ALLOW_STALE`: allow stale cache in `--cached` mode (default: `false`).
- `CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED`: default `diag rate-limits` to `--all` when no target is provided (default: `false`).
- `CODEX_PROMPT_SEGMENT_ENABLED`: enable prompt-segment output (default: `false`; set `true` to enable).
- `CODEX_PROMPT_SEGMENT_TTL`: prompt-segment cache TTL override (default: `3m`; supports `s|m|h|d|w` suffixes or raw seconds).
- `CODEX_AUTO_REFRESH_ENABLED`: enable `auth auto-refresh` behavior where applicable (default: `false`).
- `CODEX_AUTO_REFRESH_MIN_DAYS`: `auth auto-refresh` minimum token age threshold (default: `5`).

## Dependencies

- `codex` is required for `agent` commands.
- `git` is required for `agent commit`.
- `semantic-commit` and `git-scope` are optional for `agent commit` (fallbacks apply).

## Exit codes

- `0`: success and help output.
- `64`: usage or argument errors.
- `1`: operational errors.

## Contract sign-off checklist

- [ ] `cargo test -p nils-codex-cli --test main_entrypoint --test dispatch`
- [ ] `rg -n "codex-cli\\.diag\\.rate-limits\\.v1|codex-cli\\.auth\\.v1" crates/codex-cli/docs/specs/codex-cli-diag-rate-limits-and-auth-json-contract-v1.md`
- [ ] `NILS_WRAPPER_MODE=debug ./wrappers/codex-cli unknown-group` exits `64` with clap usage error output.

## Docs

- [Docs index](docs/README.md)
- [Cross-lane parity contract](../../docs/specs/codex-gemini-cli-parity-contract-v1.md)
- [JSON consumers runbook](docs/runbooks/json-consumers.md)
