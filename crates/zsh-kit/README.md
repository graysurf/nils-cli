# zsh-kit

`zsh-kit` is the nils-cli runtime entrypoint for bootstrapping an
operator-supplied Zsh repository after an environment starts. It owns generic
clone/update/safety/bootstrap/dispatch orchestration only; Zsh-specific shell
behavior stays in the target repository hook.

## Commands

```text
zsh-kit setup --repo <url-or-path> (--dry-run | --apply) [options]
zsh-kit plugin fetch --entry <plugins.list-entry> [options]
zsh-kit plugin update [options]
zsh-kit plugin maybe-update [options]
zsh-kit plugin status [options]
zsh-kit completion <bash|zsh>
zsh-kit -V | --version
```

## `setup`

Required:

- `--repo <url-or-path>`: Git repository URL or local path.
- Exactly one of `--dry-run` or `--apply`.

Options:

- `--dest <path>`: destination directory. Defaults to `$HOME/.config/zsh`.
- `--branch <name>` or `--ref <rev>`: checkout selector.
- `--write-zshenv`: write a managed `$HOME/.zshenv` that exports `ZDOTDIR`,
  preserves `ZSH_FEATURES` when provided, and sources `$ZDOTDIR/.zshenv`.
- `--features <csv>`: feature list forwarded to the repo hook.
- `--install-tools <skip|repo>`: tool-install policy forwarded to the repo
  hook. Default: `skip`.
- `--force`: allow guarded overwrite/update paths such as mismatched
  destination remotes or replacing an existing unmanaged `.zshenv`.
- `--format <text|json>`: output format. Default: `text`.

Hook discovery order:

1. `bootstrap/zsh-kit-setup.zsh`
2. `.zsh-kit/setup.zsh`

`--dry-run` reports intended actions without mutating the filesystem or running
the hook. When `--repo` is a local path or `file://` URL, dry-run also validates
that one supported hook path exists. `--apply` clones or updates the destination,
validates the hook, optionally writes `.zshenv`, then dispatches the hook with:

```text
zsh <hook> --features <csv> --install-tools <skip|repo>
```

The command refuses HTTP(S) URLs containing userinfo so credentials are not
printed or persisted. Diagnostics redact token-shaped material before rendering.

## JSON Contract

`setup --format json` emits a single envelope:

```json
{
  "schema_version": "cli.zsh-kit.setup.v1",
  "ok": true,
  "data": {
    "repo": "https://github.com/example/zsh-config.git",
    "dest": "/home/agent/.config/zsh",
    "mode": "dry-run",
    "branch": null,
    "ref": null,
    "features": [],
    "install_tools": "skip",
    "write_zshenv": false,
    "force": false,
    "hook_path": null,
    "hook_candidates": [
      "/home/agent/.config/zsh/bootstrap/zsh-kit-setup.zsh",
      "/home/agent/.config/zsh/.zsh-kit/setup.zsh"
    ],
    "actions": [
      {
        "kind": "clone",
        "description": "clone repository into destination",
        "mutation": true,
        "path": "/home/agent/.config/zsh",
        "command": "git clone <repo> <dest>"
      }
    ],
    "changed_paths": [],
    "mutation_status": "planned"
  }
}
```

Failure envelopes use schema `cli.zsh-kit.error.v1` and stable error codes:

- `credential-bearing-repo-url`
- `home-not-set`
- `destination-conflict`
- `destination-not-git`
- `destination-repo-mismatch`
- `destination-dirty`
- `missing-setup-hook`
- `zsh-not-found`
- `git-command-failed`
- `hook-command-failed`
- `zshenv-conflict`
- `io-error`

Exit codes follow the workspace contract:

- `0`: success.
- `1`: runtime failure.
- `64`: command-line usage error.
- `65`: invalid input data.
- `69`: required resource unavailable.

## `plugin`

`zsh-kit plugin` owns the generic Git-backed plugin fetch/update/status behavior
that can run outside the current shell. The zsh-kit repository still owns shell
startup glue: reading `plugins.list`, sourcing plugin files, mutating `fpath`,
and plugin-specific hooks.

Commands:

```text
zsh-kit plugin fetch --entry <plugins.list-entry> [--plugins-dir <path>] [--dry-run] [--force]
zsh-kit plugin update [--plugins-dir <path>] [--dry-run]
zsh-kit plugin maybe-update [--plugins-dir <path>] [--timestamp-file <path>] [--interval-days <days>] [--dry-run]
zsh-kit plugin status [--timestamp-file <path>] [--interval-days <days>]
```

Defaults:

- `--plugins-dir`: `$ZSH_PLUGINS_DIR`, then `$ZDOTDIR/plugins`, then
  `$HOME/.config/zsh/plugins`.
- `--timestamp-file`: `$PLUGIN_UPDATE_FILE`, then
  `$ZSH_CACHE_DIR/plugin.timestamp`, then `$HOME/.cache/zsh/plugin.timestamp`.
- `--interval-days`: `$PLUGIN_UPDATE_INTERVAL_DAYS`, then `7`.

All plugin subcommands support `--format <text|json>`. JSON success envelopes
use schema `cli.zsh-kit.plugin.v1`; failures use
`cli.zsh-kit.plugin.error.v1`.
