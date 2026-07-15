# nils-secrets

`secrets` pulls and pushes a repo's `.env` from a central [SOPS](https://github.com/getsops/sops)
store, from anywhere. It is a thin wrapper over `sops` and `git`: run it from
inside a cloned repo and it maps the repo's `origin` remote to a store entry,
decrypts that entry into `./.env`, or encrypts `./.env` back into the store.

This crate ports the `serenvia/secrets` bash script into the workspace.

## Commands

```bash
secrets pull [name]    # decrypt the store entry -> ./.env (mode 600)
secrets add [file]     # encrypt ./.env (or <file>) -> store, commit, push
secrets list           # list every store entry name
secrets which [name]   # print the store path this repo maps to
secrets edit [name]    # open the store entry in sops for editing
secrets completion bash
secrets completion zsh
```

`[name]` overrides the auto-detected repo: a bare name resolves against
`repos/` then `stacks/`; or pass an explicit `stacks/<x>` / `repos/<o>/<r>`.

## Output modes

- Default: human-readable text on stdout; warnings/errors on stderr.
- `--format json`: a single versioned envelope
  (`schema_version` / `ok` / `data` | `error`) per the
  [CLI Service JSON Contract Guideline](../../docs/specs/cli-service-json-contract-guideline-v1.md).
  Envelope `schema_version` values are `cli.secrets.<command>.v1`.

### No-secret-leak guarantee

stdout and the JSON envelope carry only **metadata** — store paths, store entry
**names**, booleans, and counts. Decrypted secret **values** are written
directly to `./.env` (mode `600`) and are never echoed to stdout or placed in
the JSON envelope. `add` encrypts into a hidden mode-600 temporary output beside
the final entry, asks SOPS to decrypt and MAC-verify the complete temporary
document, and only then atomically renames it over the tracked target. The
sibling location guarantees a same-filesystem rename and supports both primary
checkouts and linked worktrees whose `.git` is a pointer file. The target never
contains plaintext; encryption failure, invalid output, SIGINT, or SIGTERM that
wins the atomic install commit point leaves any prior ciphertext unchanged and
removes the temporary output. Once installation wins that commit point, handled
signals are deferred until `git add`, commit, and push complete so they cannot
strand an incomplete Git transaction. This contract is
exercised by hermetic tests in `crates/secrets/tests/integration.rs` that use a
secret canary string and assert it never appears on stdout/stderr.

## Exit codes

| Code | Meaning                                                       |
| ---- | ------------------------------------------------------------- |
| `0`  | success                                                       |
| `1`  | runtime error                                                 |
| `64` | command-line usage error                                      |
| `65` | no store entry for the requested target / missing source file |
| `69` | the store, `sops`, or `git` is unavailable                    |

## Environment

- `SECRETS_REPO`: override the store path. The shared Serenvia environment sets
  it to `$HOME/Project/serenvia/secrets`; the CLI uses that same path as its
  fallback when the variable is unset.

## Dependencies

Requires `git` and `sops` on `PATH`. Decryption uses this host's age key
(`~/.config/sops/age/keys.txt`) or a configured GPG key, per the store's
`.sops.yaml`.
