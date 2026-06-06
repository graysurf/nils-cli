# nils-opencode-cli

`opencode-cli` is the native OpenCode agent-helper CLI migrated out of zsh-kit.

## Commands

```bash
opencode-cli agent prompt [prompt...]
opencode-cli agent advice [question...]
opencode-cli agent knowledge [concept...]
opencode-cli agent commit [-p|--push] [-a|--auto-stage] [extra...]
opencode-cli completion bash
opencode-cli completion zsh
```

The zsh-kit `opencode-tools`, `oc`, `opencode-advice`,
`opencode-knowledge`, and `opencode-commit-with-scope` functions remain shell
compatibility wrappers around this binary.

## Environment

- `OPENCODE_CLI_MODEL`: forwarded to `opencode run -m`.
- `OPENCODE_CLI_VARIANT`: forwarded to `opencode run --variant`.
- `ZDOTDIR`: used to find prompt templates under `$ZDOTDIR/prompts`.
- `ZSH_SCRIPT_DIR`: fallback root used to find `prompts/` next to zsh-kit
  runtime scripts.

## Exit Codes

- `0`: success.
- `1`: runtime error.
- `64`: command-line usage error.
