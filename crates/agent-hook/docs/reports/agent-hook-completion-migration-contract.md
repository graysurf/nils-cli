# agent-hook Completion Migration Contract

## Metadata

- CLI binary: `agent-hook`
- Owning crate: `crates/agent-hook`
- Contract owner: Codex
- Target PR: pending
- Status: implemented
- Last updated: 2026-08-27
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
| `workspace-recovery inspect`, `verify-handoff` | JSON-default output enum; strict request data is supplied on stdin |
| `finish-line open`, `begin`, `run`, `register`, `admit`, `observe`, `verdict`, `stop`, `status` | JSON-default output enum; request data is strict service JSON on stdin; `run` describes foreground probe/supervision; the acceptance commands describe immutable registration, pre-body admission, authenticated terminal observation, and detached verdicts; internal `quiesce` and `release` lifecycle RPCs are excluded |
| `completion` | `bash` and `zsh` shell enum |

Every completion source is clap metadata in `src/cli.rs`. The hidden paths are
`finish-line quiesce` for cancellation/failure cleanup and `finish-line
release` for authenticated disposed-session retirement. They are callable by
the DSH integration but intentionally absent from public help and completion.
No deprecated command path exists.

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
| Finish-line JSON default is represented accurately | dedicated clap output enum | help proves `[default: json]`; generated completion has no text-default annotation |
| Internal lifecycle stays non-public | hidden `finish-line quiesce` and `release` clap metadata | CLI contract proves public help and both completion scopes omit both commands |

## Validation

```bash
zsh -n completions/zsh/_agent-hook
bash -n completions/bash/agent-hook
cargo run -p nils-agent-hook -- completion zsh | rg -- "--help|--version|--"
zsh -f tests/zsh/completion.test.zsh
bash scripts/ci/completion-freshness-audit.sh --strict --bin agent-hook
```
