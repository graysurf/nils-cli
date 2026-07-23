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
  agent           prompt | advice | knowledge | commit | resume | run | doctor
  auth            login | use | save | remove | refresh | auto-refresh | status | current | sync | remote pull
  diag            rate-limits
  config          show | set
  prompt-segment  check | status | (render options)
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

- `prompt [--runtime isolated|inherited] [--ephemeral] [PROMPT...]`: Run a raw prompt through `codex exec`.
- `advice [--runtime isolated|inherited] [--ephemeral] [QUESTION...]`: Request actionable engineering advice.
- `knowledge [--runtime isolated|inherited] [--ephemeral] [CONCEPT...]`: Request a concept explanation.
- `commit [--runtime isolated|inherited] [--ephemeral] [-p|--push] [-a|--auto-stage] [EXTRA...]`: Run the semantic-commit workflow.
- `doctor [--format text|json]`: Probe isolated-runtime flags, features,
  temporary-home/auth bridging, instruction isolation, and hook isolation
  without an API request.
- `resume <SESSION_ID> [--cd <dir>]`: Resolve the session's recorded working directory from local Codex history and launch
  `codex resume <SESSION_ID> --cd <cwd> --no-alt-screen` there, propagating Codex's exit status. Run it from any directory. Fails without
  launching Codex (`65`) when the id is unknown or matches more than one recorded directory; pass `--cd` to override the resolved directory
  for a repository that moved.
- `run --capsule <dir> [--allow-host-access] [--format text|json]`: Validate
  and run a private Execution Capsule through a Codex supervisor.
  Workspace capsules retain `workspace-write`; host capsules require the
  operator to pass `--allow-host-access` and use `danger-full-access`. Host
  receipts are `supervisor-trusted`, not tamper-resistant against a malicious
  same-UID process.

Agent flag notes:

- `--runtime isolated` is the default for one-shot commands. It creates a
  temporary `CODEX_HOME` containing only an auth symlink, ignores user/project
  rules, disables hooks/plugins/apps/memory/goals/subagents, and always runs
  ephemerally without the dangerous bypass flag.
- `--runtime inherited` explicitly restores the historical full-home executor and
  its `CODEX_ALLOW_DANGEROUS_ENABLED=true` gate. `agent resume` is always inherited.
- `--ephemeral` is a compatibility no-op in isolated mode and keeps its existing
  forwarding behavior in inherited mode.
- Isolated `agent commit` exposes only staged-context to the model, accepts
  strict structured message fields, detects HEAD/index drift, and delegates the
  commit exclusively to `semantic-commit`. The inherited path retains its
  compatibility fallback.
- `resume --cd <dir>`: Bypass automatic cwd resolution and resume in `<dir>` (must be an existing directory).
- `agent run` is separate from the isolated/inherited one-shot prompt modes. It
  deliberately retains the current Codex home, project instructions, config,
  and hooks; it never passes `--ignore-user-config`, `--ignore-rules`, or
  `--dangerously-bypass-approvals-and-sandbox`.
- The capsule's `run.sh` remains directly runnable with
  `bash /absolute/capsule/run.sh`. The supervised route independently checks
  the manifest, script digest, optional Git preconditions, exact script
  execution, post-supervision integrity, and declared validation commands,
  then writes owner-only schema/JSONL/final/receipt artifacts.
  See the [Execution Capsule v1 specification](docs/specs/execution-capsule-v1.md).

### auth

- `login [--api-key|--device-code]`: Login via ChatGPT browser flow (`chatgpt-browser`, default), ChatGPT device-code flow
  (`chatgpt-device-code`), or API key flow (`api-key`). `--api-key` and `--device-code` are mutually exclusive (`64` on invalid usage).
- `use <name|name.json|email>`: Switch to a secret by name/name.json or email.
- `save [--yes] <secret|secret.json>`: Save active `CODEX_AUTH_FILE` into `CODEX_SECRET_DIR`. Secret files are normalized to `.json`; if
  target exists, interactive mode prompts for overwrite, while non-interactive and JSON mode require `--yes` to overwrite.
- `remove [--yes] <secret|secret.json>`: Remove a secret file from `CODEX_SECRET_DIR`. Secret names are normalized to `.json`; interactive
  mode prompts for confirmation, while non-interactive and JSON mode require `--yes`.
- `refresh [secret.json]`: Refresh OAuth tokens. With `CODEX_AUTH_REMOTE_SSH` set, the default active-auth refresh delegates to the
  remote token authority and imports access-only auth. When the active auth matches a stored secret, that matched secret name is preferred
  over `CODEX_AUTH_REMOTE_NAME`; explicit `secret.json` targets still use local `refresh_token`.
- `auto-refresh`: Refresh stale tokens across auth + secrets. In remote-authority mode it refreshes only the active auth file through remote
  access-only sync and does not overwrite local secret files.
- `status`: Report active auth readiness without exposing token or API-key material.
- `current`: Show which secret matches `CODEX_AUTH_FILE`.
- `sync`: Sync `CODEX_AUTH_FILE` back into matching secrets.
- `remote pull --ssh <host> --name <secret> --access-only --write-active [--refresh]`: Pull access-only auth from a remote token authority
  over SSH and write it to `CODEX_AUTH_FILE`. The local file never receives `refresh_token`; the remote authority remains the only
  refresh-token writer. `--refresh` explicitly asks the remote authority to refresh the named secret before export; without it, pull only
  exports the authority's current access/id/account fields.

Auth examples:

- `codex-cli auth login`: ChatGPT browser login.
- `codex-cli auth login --device-code`: ChatGPT device-code login.
- `codex-cli auth login --api-key`: OpenAI API key login.
- `codex-cli auth save team-alpha`: Save to `team-alpha.json` and prompt before overwrite when applicable.
- `codex-cli auth save --yes team-alpha.json`: Force overwrite without prompt.
- `codex-cli auth remove --yes team-alpha`: Remove `team-alpha.json`.
- `codex-cli auth status --format json`: Check active auth readiness for automation.
- Import auth-host's current access-only `team` auth into the active local auth file:
  `codex-cli auth remote pull --ssh auth-host --name team --access-only --write-active`
- Configure a replica to delegate default active-auth refresh to `g14`:
  `eval "$(codex-cli config set remote-ssh g14)" && eval "$(codex-cli config set remote-name team)"`

### diag

- `rate-limits [options] [secret.json]`: Rate-limit diagnostics. Options: `-c/--clear-cache`, `-d/--debug`, `--cached`,
  `--no-refresh-auth`, `--format <text|json>`, `--json`, `--one-line`, `--all`, `--async`, `--watch`, `--jobs <n>`.
- `--cached` reads cache only. Freshness is controlled by `CODEX_RATE_LIMITS_CACHE_TTL` (default `3m`); stale cache is rejected unless
  `CODEX_RATE_LIMITS_CACHE_ALLOW_STALE=true`.
- Cached quota values have a fixed display ceiling independent of the freshness
  TTL: values are eligible through 599 seconds and omitted at 600 seconds.
  `CODEX_RATE_LIMITS_CACHE_ALLOW_STALE=true` never bypasses this ceiling. A
  `fetched_at` timestamp up to 5 seconds in the future is tolerated for clock
  skew; a timestamp further ahead fails closed. Cache/auth files are retained,
  and prompt refresh/retry continues without rendering expired percentages.
- `--watch` refreshes output every 60 seconds until interrupted and requires `--async`.

### config

- `show`: Print effective configuration values.
- `set <key> <value>`: Emit a shell snippet for the current shell.

### prompt-segment

- `prompt-segment [--no-5h] [--ttl <duration>] [--time-format <strftime>] [--show-timezone] [--refresh]`: Render or refresh the prompt
  segment. Default reset time uses local time without timezone; `--show-timezone` adds the local offset.
- `prompt-segment check`: Exit `0` only when prompt-segment output is enabled and the active auth file has ChatGPT/OAuth credentials usable
  by the prompt segment. This command is intended for Starship `when` gates.
- `prompt-segment status [--format text|json]`: Report prompt-segment readiness, cache state, and the reason it would or would not render.

### completion

- `completion <bash|zsh>`: Print the shell completion script for the requested shell to stdout.

## JSON contract (service consumers)

- Human-readable text is the default output mode.
- Machine-readable JSON mode is explicit: use `--format json` (preferred) or `--json` where supported for compatibility.
- Contract spec: `docs/specs/codex-cli-diag-rate-limits-and-auth-json-contract-v1.md`
- Consumer runbook: `docs/runbooks/json-consumers.md`
- Covered surfaces: `agent run`, `diag rate-limits` (single/all/async),
  `auth login|use|save|remove|refresh|auto-refresh|status|current|sync|remote pull`, and `prompt-segment status`.

## Environment

- `CODEX_CLI_AGENT_RUNTIME`: `isolated` (default) or explicit `inherited` for one-shot agent commands.
- `CODEX_ALLOW_DANGEROUS_ENABLED`: gate for inherited `agent` commands (default: `false`).
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
- `CODEX_PROMPT_SEGMENT_ZSH_ESCAPE_ENABLED`: escape `%` as `%%` for zsh prompt expansion when a Starship adapter embeds output in `PROMPT`
  (default: `false`; set `true` only for zsh prompt adapters).
- `CODEX_AUTO_REFRESH_ENABLED`: enable token refresh behavior for `auth auto-refresh`, `diag rate-limits`, and prompt-segment
  retry-on-401 paths
  (default: `false`; leave unset/false on multi-machine setups unless one machine intentionally owns refresh).
- `CODEX_AUTO_REFRESH_MIN_DAYS`: `auth auto-refresh` minimum token age threshold (default: `5`).
- `CODEX_AUTH_REMOTE_SSH`: SSH host alias for a remote Codex token authority. When paired with `CODEX_AUTH_REMOTE_NAME`, default
  `auth refresh` delegates to remote access-only sync instead of reading local `refresh_token`.
- `CODEX_AUTH_REMOTE_NAME`: remote authority secret name used by delegated active-auth refresh.
- `CODEX_AUTH_REMOTE_REFRESH`: when truthy, delegated remote refresh asks the authority to refresh before exporting. Default is unset/false;
  leave false when the authority's own timer owns freshness.

## Dependencies

- `codex` is required for `agent` commands.
- `git` is required for `agent commit`.
- `semantic-commit` is required for isolated `agent commit`; the inherited
  compatibility path retains the old fallback. `git-scope` remains optional.
- `ssh` is required for `auth remote pull`.
- `agent resume` reads local Codex session history under `$CODEX_HOME/sessions` (default `~/.codex/sessions`); the shared resolver lives in
  `nils-provider-resume`.

## Exit codes

- `0`: success and help output.
- `64`: usage or argument errors.
- `65`: invalid input data, including an invalid capsule, missing host-access
  acknowledgement, or an `agent resume` id that cannot be resolved.
- `1`: operational errors.

## Contract sign-off checklist

- [ ] `cargo test -p nils-codex-cli --test main_entrypoint --test dispatch`
- [ ] `rg -n "codex-cli\\.diag\\.rate-limits\\.v1|codex-cli\\.auth\\.v1" crates/codex-cli/docs/specs/codex-cli-diag-rate-limits-and-auth-json-contract-v1.md`
- [ ] `NILS_WRAPPER_MODE=debug ./wrappers/codex-cli unknown-group` exits `64` with clap usage error output.

## Docs

- [Docs index](docs/README.md)
- [Execution Capsule v1](docs/specs/execution-capsule-v1.md)
- [Cross-lane parity contract](../../docs/specs/codex-gemini-cli-parity-contract-v1.md)
- [JSON consumers runbook](docs/runbooks/json-consumers.md)
