# gemini-cli

## Overview

gemini-cli is a provider-specific Rust CLI for Gemini workflows: Gemini execution wrappers, auth/secret management, diagnostics, config
output, prompt-segment rendering, and completion export. Runtime wiring is owned by `gemini-cli` adapters with shared
`nils-common::provider_runtime` helpers for common primitives.

## Usage

```text
Usage:
  gemini-cli <group> <command> [args]
  gemini-cli prompt-segment [options]
  gemini-cli completion <bash|zsh>

Groups:
  agent      prompt | advice | knowledge | commit
  auth       login | use | save | remove | refresh | auto-refresh | current | sync
  diag       rate-limits
  config     show | set
  prompt-segment   (options)
  completion bash | zsh

Help:
  gemini-cli help
  gemini-cli <group> help
```

## Scope boundary

| Job                                                                                                  | Primary owner                                           |
| ---------------------------------------------------------------------------------------------------- | ------------------------------------------------------- |
| Shared provider runtime helpers (`auth/path/config/exec/error`)                                      | `nils-common::provider_runtime` + `gemini-cli` adapters |
| Gemini auth, Gemini prompt wrappers, Gemini diagnostics, prompt-segment rendering, completion export | `gemini-cli`                                            |
| Unsupported commands/groups                                                                          | clap usage error (`64`)                                 |

- `gemini-cli` owns only provider-specific Gemini operations (`agent`, `auth`, `diag rate-limits`, `config`, `prompt-segment`, `completion`).
- Existing `gemini-cli` commands stay stable for provider-specific workflows.
- Unknown groups/subcommands are deterministic usage errors (`64`).

## Commands

### agent

- `prompt [PROMPT...]`: Run a raw prompt through `gemini --prompt-interactive`.
- `advice [QUESTION...]`: Request actionable engineering advice.
- `knowledge [CONCEPT...]`: Request a concept explanation.
- `commit [-p|--push] [-a|--auto-stage] [EXTRA...]`: Run the semantic-commit workflow.

### auth

- `login [--api-key|--device-code]`: Login via Gemini browser flow (`gemini-browser`, default), Gemini device-code flow
  (`gemini-device-code`), or API key flow (`api-key`). `--api-key` and `--device-code` are mutually exclusive (`64` on invalid usage).
- `use <name|email>`: Switch to a secret by name or email.
- `save [--yes] <secret.json>`: Save active `GEMINI_AUTH_FILE` into `GEMINI_SECRET_DIR` with an explicit file name. If target exists,
  interactive mode prompts for overwrite; non-interactive and JSON mode require `--yes` to overwrite.
- `remove [--yes] <secret.json>`: Remove a secret file from `GEMINI_SECRET_DIR`. Interactive mode prompts for confirmation; non-interactive
  and JSON mode require `--yes`.
- `refresh [secret.json]`: Refresh OAuth tokens.
- `auto-refresh`: Refresh stale tokens across auth + secrets.
- `current`: Show which secret matches `GEMINI_AUTH_FILE`.
- `sync`: Sync `GEMINI_AUTH_FILE` back into matching secrets.

Auth examples:

- `gemini-cli auth login`: Gemini browser login.
- `gemini-cli auth login --device-code`: Gemini device-code login.
- `gemini-cli auth login --api-key`: Gemini API key login.
- `gemini-cli auth save team-alpha.json`: Save and prompt before overwrite when applicable.
- `gemini-cli auth save --yes team-alpha.json`: Force overwrite without prompt.
- `gemini-cli auth remove --yes team-alpha.json`: Remove a saved secret file.

### diag

- `rate-limits [options] [secret.json]`: Rate-limit diagnostics. Options: `-c/--clear-cache`, `-d/--debug`, `--cached`, `--no-refresh-auth`,
  `--json`, `--format json`, `--one-line`, `--all`, `--async`, `--jobs <n>`.

### config

- `show`: Print effective configuration values.
- `set <key> <value>`: Emit a shell snippet for the current shell.

### prompt-segment

- `prompt-segment [--no-5h] [--ttl <duration>] [--time-format <strftime>] [--show-timezone] [--refresh] [--is-enabled]`: Render or refresh
  the prompt segment. Default reset time uses local time without timezone; `--show-timezone` adds the local offset.

### completion

- `completion <bash|zsh>`: Export shell completion script to stdout.

## JSON contract (service consumers)

- Human-readable text is the default output mode.
- Machine-readable JSON mode is explicit: use `--format json` (preferred) or `--json` where supported for compatibility.
- Contract spec: `docs/specs/gemini-cli-diag-auth-json-contract-v1.md`
- Consumer runbook: `docs/runbooks/json-consumers.md`
- Covered surfaces: `diag rate-limits` (single/all/async) and `auth login|use|save|remove|refresh|auto-refresh|current|sync`.

## Environment

- `GEMINI_ALLOW_DANGEROUS_ENABLED=true` is required for `agent` commands.
- `GEMINI_CLI_MODEL` and `GEMINI_CLI_REASONING` set `gemini` execution defaults.
- `GEMINI_SECRET_DIR` controls the secret directory path. When unset, it defaults to `~/.gemini/secrets`.
- `GEMINI_AUTH_FILE` controls the active auth file path. When unset, it defaults to `~/.gemini/oauth_creds.json`.
- `GEMINI_SECRET_CACHE_DIR` controls secret cache timestamps.
- `GEMINI_PROMPT_SEGMENT_ENABLED=true` enables prompt-segment output.
- `GEMINI_PROMPT_SEGMENT_TTL` overrides the prompt-segment cache TTL.
- `GEMINI_PROMPT_SEGMENT_REFRESH_MIN_SECONDS` (default `30`) throttles background-refresh attempts; see
  parity contract for invariants.
- `GEMINI_PROMPT_SEGMENT_LOCK_STALE_SECONDS` (default `90`) controls stale-lock recovery for
  background refresh.
- `GEMINI_RATE_LIMITS_DEFAULT_ALL_ENABLED=true` makes `diag rate-limits` default to `--all` when no
  positional secret is supplied.
- `GEMINI_AUTO_REFRESH_ENABLED` and `GEMINI_AUTO_REFRESH_MIN_DAYS` configure auth auto-refresh behavior.

## Dependencies

- `gemini` is required for `agent` commands and interactive OAuth login flows.
- `git` is required for `agent commit`.
- `semantic-commit` and `git-scope` are optional for `agent commit` (fallbacks apply).

## Exit codes

- `0`: success and help output.
- `64`: usage or argument errors.
- `1`: operational errors.

## Contract sign-off checklist

- [ ] `cargo test -p nils-gemini-cli --test main_entrypoint --test dispatch`
- [ ] `rg -n "gemini-cli\\.diag\\.rate-limits\\.v1|gemini-cli\\.auth\\.v1" crates/gemini-cli/docs/specs/gemini-cli-diag-auth-json-contract-v1.md`
- [ ] `NILS_WRAPPER_MODE=debug ./wrappers/gemini-cli unknown-group` exits `64` with clap usage error output.

## Docs

- [Docs index](docs/README.md)
- [Cross-lane parity contract](../../docs/specs/codex-gemini-cli-parity-contract-v1.md)
- [JSON consumers runbook](docs/runbooks/json-consumers.md)
