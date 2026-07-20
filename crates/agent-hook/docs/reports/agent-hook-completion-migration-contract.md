# agent-hook Completion Migration Contract

## Metadata

- CLI binary: `agent-hook`
- Owning crate: `crates/agent-hook`
- Contract owner: Codex
- Target PR: pending
- Status: implemented
- Last updated: 2026-07-20
- Enforcement: `completion_mode=clap-first`;
  `completion_mode_toggles=forbidden`;
  `alternate_completion_dispatch=forbidden`;
  `generated_load_failure=fail-closed`; `completion_engine=static`

## Command graph

| Command | Completion obligations |
| --- | --- |
| `agent-hook` | global config/policy/state paths, `-h`, `-V`, subcommands |
| `dispatch` | product and output enums, event/capability paths, boolean modes |
| `validate`, `inventory` | output enum |
| `doctor` | product enum, all-provider mode, output enum |
| `setup` | product enum, exclusive action flags, digest, output enum |
| `recovery challenge` | product/scope enums, exact binding values, output path |
| `recovery authorize` | challenge/output paths and reviewed digest |
| `recovery consume` | capability path and exact binding values |
| `recovery status`, `recovery revoke` | public capability selector and output enum |
| `completion` | `bash` and `zsh` shell enum |

Every completion source is clap metadata in `src/cli.rs`; no hidden or
deprecated command path exists.

## Value providers and aliases

Static `ValueEnum` candidates cover product, scope, format, and shell values.
`ValueHint` supplies file/directory completion for paths. There are no dynamic
runtime candidates and no aliases for `agent-hook`.

## Enforcement and single-path invariants

| Invariant | Enforcement | Verification |
| --- | --- | --- |
| Clap-first static generation | `src/completion.rs` | completion freshness audit |
| No runtime completion-mode toggle | `src/cli.rs`, `src/completion.rs` | grep audit |
| No alternate dispatcher | `src/completion.rs` | grep audit |
| Generated-load failure is closed | committed shell assets | completion tests |

## Validation

```bash
zsh -n completions/zsh/_agent-hook
bash -n completions/bash/agent-hook
cargo run -p nils-agent-hook -- completion zsh | rg -- "--help|--version|--"
zsh -f tests/zsh/completion.test.zsh
bash scripts/ci/completion-freshness-audit.sh --strict --bin agent-hook
```
