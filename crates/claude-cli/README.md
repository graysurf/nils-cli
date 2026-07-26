# claude-cli

## Overview

`claude-cli` is a provider-specific Rust CLI for Claude-oriented helpers that
should not live in shell glue. It owns safe one-shot Claude Code execution,
upstream-owned authentication delegation, wrapper configuration,
prompt-segment rendering, usage source selection, cache fallback, session
resume, and completion export.

## Usage

```text
claude-cli agent prompt [--runtime safe|inherited] [--model <model>] [--effort <level>] [input...]
claude-cli agent advice [--runtime safe|inherited] [--model <model>] [--effort <level>] [input...]
claude-cli agent knowledge [--runtime safe|inherited] [--model <model>] [--effort <level>] [input...]
claude-cli agent commit [--auto-stage] [--push] [--model <model>] [--effort <level>] [extra...]
claude-cli agent doctor [--format text|json]
claude-cli agent resume <SESSION_ID> [--cd <dir>]
claude-cli auth login [--claudeai|--console] [--email <email>] [--sso]
claude-cli auth status [--format text|json]
claude-cli auth logout
claude-cli config show
claude-cli config set <key> <value>
claude-cli prompt-segment [options]
claude-cli prompt-segment check
claude-cli prompt-segment status [--format text|json]
claude-cli usage [--format text|json] [--source auto|oauth|cli|cache]
claude-cli completion <bash|zsh>
```

## Scope boundary

| Job | Primary owner |
| --- | --- |
| Safe one-shot runtime, auth delegation, wrapper config, prompt/usage cache, rendering, completion | `claude-cli` |
| Credential persistence, OAuth refresh, browser login, managed policy | upstream Claude Code |
| Shell aliases, Starship wiring, PATH/fpath registration | shell integration |

The wrapper does not read, copy, export, refresh, or directly delete Claude
Code credentials.

## Agent commands

- `agent prompt [input...]`: Run a raw one-shot prompt. If input arguments are
  omitted, read the prompt from stdin.
- `agent advice [input...]`: Run the versioned
  `nils-claude-cli.agent-advice.v1` engineering-advice template.
- `agent knowledge [input...]`: Run the versioned
  `nils-claude-cli.agent-knowledge.v1` explanation template.
- `agent commit [extra...]`: Generate a bounded structured commit message from
  staged context and invoke `semantic-commit` as the only commit writer.
- `agent doctor [--format text|json]`: Check Claude capabilities, the upstream
  installation doctor, and the `git` / `semantic-commit` dependencies without
  making a model call.
- `agent resume <SESSION_ID> [--cd <dir>]`: Resolve the recorded working
  directory from local Claude project history and launch
  `claude --resume <SESSION_ID>` there. `--cd` overrides resolution.

One-shot commands default to `--runtime safe`. Before launch, the wrapper
probes `claude --help` and fails with exit `69` when a required flag is absent.
The safe profile uses:

- `--safe-mode` and `--strict-mcp-config`;
- `--no-session-persistence`;
- `--permission-mode dontAsk`, disabled slash commands, and disabled Chrome;
- a `Read,Glob,Grep` allowlist for `prompt` and `advice`;
- an empty tool allowlist for `knowledge`.

Prompt input is limited to 1 MiB and delivered to Claude over stdin so it does
not appear in child-process argv. Templates, models, and flags remain discrete
argv entries without shell interpolation. `--runtime inherited` is an explicit
escape hatch that permits upstream customizations while keeping the
command-specific tool allowlist. Session persistence remains disabled by
default and can only be enabled for inherited mode with
`CLAUDE_CLI_NO_SESSION_PERSISTENCE=false`.

Claude `--safe-mode` still permits administrator-managed policy, upstream
authentication, model selection, and built-in permissions. It is a
Claude-specific safe boundary, not a claim of Codex-equivalent isolation.

### Agent commit safety contract

`agent commit` has no inherited-runtime fallback. It always:

- reads at most 2 MiB from `semantic-commit staged-context --format bundle`;
- rejects secret-like staged content with stable `nils-scrub` pattern ids
  before Claude is launched;
- sends that bundle and at most 64 KiB of optional guidance over stdin;
- runs Claude in a temporary working directory with safe mode, strict MCP,
  no session persistence, disabled slash commands/Chrome, and an empty tool
  list;
- validates Claude output against JSON Schema and local Conventional Commit
  constraints;
- rechecks both `HEAD` and the staged tree after model generation;
- invokes `semantic-commit commit --expect-head ... --automation` only when
  those checks still match;
- verifies the created commit has the captured parent and tree before reporting
  success or allowing a push.

`--auto-stage` explicitly runs `git add -A` before the snapshot. `--push`
requires an attached branch with a configured upstream, captures its effective
push endpoint before the model call, and revalidates that endpoint before
pushing only the verified commit through an explicit non-force refspec. The
push command uses the captured endpoint rather than resolving the mutable
remote alias again and pins exact-match command-scope URL rewrites so inherited
`insteadOf` / `pushInsteadOf` chains cannot retarget it. A model, schema, drift,
or commit failure leaves the index staged when no writer-side mutation is
observed. If `semantic-commit` times out or fails after changing repository
state, the wrapper preserves that state, skips push, and requires inspection
instead of claiming the index is still staged. A post-commit integrity or push
failure preserves and reports the local commit but never pushes an unverified
object.

`agent doctor` bounds and discards upstream stdout/stderr, verifies the exact
`semantic-commit staged-context` and `commit` help surfaces, and never copies
settings paths or other private diagnostic text into its JSON contract. A
successful diagnosis emits `claude-cli.agent.doctor.v1`; `ok: true` means the
checks completed, while `result.ready` and exit `0`/`1` carry readiness.
`commit_profile` covers the fixed safe commit profile;
`configured_commit_profile` also requires `--model` or `--effort` when those
wrapper overrides are configured.

## Authentication commands

- `auth login`: Delegate to `claude auth login`, including `--claudeai`,
  `--console`, `--email`, and `--sso`.
- `auth status [--format text|json]`: Call `claude auth status --json`, retain
  only public status classifications, and preserve upstream exit `0`/`1`
  authenticated meaning.
- `auth logout`: Delegate to `claude auth logout`.

Auth status drops email, organization identity, token, credential path, and
unknown upstream fields.

## Configuration commands

- `config show`: Print the effective wrapper model, effort, runtime, and
  no-session-persistence values. Invalid configured values fail with exit `64`
  instead of being silently replaced by defaults.
- `config set <key> <value>`: Validate a supported key and emit one safely
  quoted POSIX-shell `export`.

Supported keys are `model`, `effort`, `agent-runtime`, and
`no-session-persistence`. These commands never modify Claude settings or
credential files.

## Prompt segment

- `prompt-segment [--no-5h] [--ttl <duration>] [--time-format <strftime>]
  [--show-timezone] [--refresh]`: Render Claude usage.
- `prompt-segment check`: Exit `0` when a Claude OAuth token is available,
  otherwise `1`.
- `prompt-segment status [--format text|json]`: Report cache readiness without
  exposing token material.

Output:

```text
5h:<remaining>% W:<remaining>% <weekly_reset_time>[<stale_suffix>]
```

Without `--refresh`, stale or missing cache starts a coalesced detached refresh
after any eligible cached line is printed. The prompt path does not wait for
the network request. `--refresh` is the explicit blocking operation.
`--no-5h` hides the five-hour window. `--show-timezone` adds the local UTC
offset to the default time format; explicit `--time-format` takes precedence.

Cached values are display-eligible while cache mtime is less than 600 seconds
old. A timestamp up to five seconds in the future is tolerated. Expired files
are retained but contribute no prompt or usage windows.

## Usage command

- `usage [--format text|json] [--source auto|oauth|cli|cache]`: Read Claude
  usage through a service-consumable contract.
- `auto`: Try OAuth, then a bounded native Claude `/usage` probe, then cache.
- `oauth`, `cli`, and `cache`: Select one source for focused diagnostics.

JSON uses `claude-cli.usage.v1`. It includes provider, source, stale state,
normalized windows, and an optional provider-neutral `reason_code`. Provider
responses, terminal errors, and credentials are classified locally and never
forwarded.

## Completion

`completion <bash|zsh>` exports clap-generated shell completion to stdout.

## Environment

- `CLAUDE_CLI_BIN`: Claude executable for agent/auth; default `claude`.
- `CLAUDE_CLI_MODEL`, `CLAUDE_CLI_EFFORT`: one-shot defaults.
- `CLAUDE_CLI_AGENT_RUNTIME`: `safe` (default) or `inherited`.
- `CLAUDE_CLI_NO_SESSION_PERSISTENCE`: default `true`; safe mode always
  disables persistence.
- One-shot capability probes and auth-status delegation bound captured output
  while the child is running. They terminate the child process group after
  five and three seconds, respectively.
- Commit generation caps captured Claude output at 1 MiB with a five-minute
  deadline. `agent doctor` caps upstream diagnostic output at 256 KiB with a
  15-second deadline.
- Auto-stage has a 60-second deadline. Commit creation and push each have a
  120-second deadline. All bounded subprocess caps are aggregate across stdout
  and stderr, and deadline/limit failure terminates the child process group.
- `CLAUDE_PROMPT_TTL`, `CLAUDE_PROMPT_SEGMENT_TTL`: cache TTL; default `60`
  seconds. `0` forces blocking refresh.
- `CLAUDE_PROMPT_STALE_SUFFIX`,
  `CLAUDE_PROMPT_SEGMENT_STALE_SUFFIX`: stale suffix.
- `CLAUDE_PROMPT_SEGMENT_CACHE_DIR`: cache-directory override.
- `CLAUDE_PROMPT_SEGMENT_ENDPOINT`: usage-endpoint override.
- `CLAUDE_PROMPT_SEGMENT_REFRESH_MIN_SECONDS`: detached refresh cooldown;
  default `60`.
- `CLAUDE_PROMPT_SEGMENT_EXE`: detached self-refresh executable override.
- `CLAUDE_PROMPT_SEGMENT_ZSH_ESCAPE_ENABLED=1`: double percent characters.
- `CLAUDE_PROMPT_SEGMENT_ACCESS_TOKEN`,
  `CLAUDE_PROMPT_SEGMENT_CREDENTIALS_JSON`: automation credentials.
- `CLAUDE_PROMPT_SEGMENT_CLAUDE_BIN`: native CLI usage-probe executable.
- `CLAUDE_PROMPT_SEGMENT_CLAUDE_TIMEOUT_SECONDS`: bounded CLI usage timeout;
  default `15`.
- `CLAUDE_PROMPT_SEGMENT_CLAUDE_PTY_DISABLED=1`: disable Unix PTY probing.
- `CLAUDE_PROMPT_SEGMENT_CLAUDE_PTY_STARTUP_DELAY_MS`,
  `CLAUDE_PROMPT_SEGMENT_CLAUDE_PTY_USAGE_DELAY_MS`: PTY timing controls.
- `CLAUDE_PROMPT_SEGMENT_KEYCHAIN_DISABLED=1`: disable macOS Keychain lookup.
- `CLAUDE_PROMPT_SEGMENT_KEYCHAIN_SERVICE`: Keychain service override.
- `NO_COLOR=1`: disable ANSI color.

## Dependencies

- `claude` is required for agent, auth, resume, and the optional CLI usage
  fallback.
- `git` and `semantic-commit` are required for `agent commit`; doctor reports
  both dependencies without modifying a repository.
- macOS `security` is used for prompt-segment Keychain lookup unless an
  automation credential override is supplied.
- Unix `script` enables the richer native CLI usage-probe path.
- Resume reads `$CLAUDE_CONFIG_DIR/projects` (default `~/.claude/projects`)
  through `nils-provider-resume`.

## Exit codes

- `0`: success, help, or no prompt output needed.
- `1`: operational false/failed state, including unauthenticated status.
- `64`: usage or argument error.
- `65`: invalid input data, invalid structured commit output, or unresolved
  resume session.
- `69`: required executable or Claude capability unavailable.

## Docs

- [Docs index](docs/README.md)
- [JSON contracts](docs/specs/claude-cli-json-contract-v1.md)
- [Usage consumer runbook](docs/runbooks/usage-consumer.md)
