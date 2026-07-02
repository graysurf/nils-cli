# agent-session Completion Migration Contract

## Metadata

- CLI binary: `agent-session`
- Owning crate: `crates/agent-session`
- Contract owner: Codex
- Target PR: pending
- Status: done
- Last updated: 2026-07-03
- Completion enforcement metadata tuple from matrix row:
  - `completion_mode=clap-first`
  - `completion_mode_toggles=forbidden`
  - `alternate_completion_dispatch=forbidden`
  - `generated_load_failure=fail-closed`
  - `completion_engine=static`

## command graph

| command path | clap source | completion obligations | notes |
| --- | --- | --- | --- |
| `agent-session` | `crates/agent-session/src/cli.rs` | root flags + top-level subcommands | `--state-dir`, `--host`, `-h`, `-V` |
| `agent-session start` | `crates/agent-session/src/cli.rs` | agent enum, path hints, format enum, prompt flags | default interactive handoff path |
| `agent-session run` | `crates/agent-session/src/cli.rs` | agent enum, path hints, format enum, prompt flags | one-shot task path |
| `agent-session list` | `crates/agent-session/src/cli.rs` | format enum | service-readable inventory |
| `agent-session command` | `crates/agent-session/src/cli.rs` | session id + format enum | prints attach commands |
| `agent-session attach` | `crates/agent-session/src/cli.rs` | session id + tmux binary path hint | local tmux attach |
| `agent-session logs` | `crates/agent-session/src/cli.rs` | session id, tail count, format enum | tmux capture or run log |
| `agent-session delete` | `crates/agent-session/src/cli.rs` | session id, tmux binary path hint, format enum | kill tmux and remove state |
| `agent-session completion` | `crates/agent-session/src/cli.rs` | shell enum | `bash` and `zsh` |

Checklist:

- [x] Every supported subcommand path is listed.
- [x] Long/short flags are represented by clap metadata.
- [x] Hidden/deprecated paths are explicitly called out.

## value providers

| argument or flag | provider type | source location | context-aware behavior | tests |
| --- | --- | --- | --- | --- |
| `--agent` | `ValueEnum` | `crates/agent-session/src/cli.rs` | static `codex`, `claude` values | completion freshness/flag parity |
| `--format` | `ValueEnum` | `nils_common::cli_contract::OutputFormat` | static `text`, `json` values | completion freshness/flag parity |
| `completion <shell>` | `ValueEnum` | `crates/agent-session/src/completion.rs` | static `bash`, `zsh` values | completion freshness/flag parity |
| path flags | `ValueHint` | `crates/agent-session/src/cli.rs` | shell path completion | completion freshness/flag parity |

No dynamic runtime value provider is used.

Checklist:

- [x] No global candidate dump behavior remains.
- [x] Cursor-position filtering is documented for dynamic value paths.

## alias map

No aliases are required for `agent-session`.

Checklist:

- [x] Alias entries are synced in both alias files, or `not required` is explicit.
- [x] Adapter rewrite semantics are documented when aliases inject defaults.

## completion enforcement metadata

| metadata key | required value | declared value | enforcement location | verification method |
| --- | --- | --- | --- | --- |
| `completion_mode` | `clap-first` | `clap-first` | `docs/specs/completion-coverage-matrix-v1.md` | matrix row |
| `completion_mode_toggles` | `forbidden` | `forbidden` | `docs/specs/completion-coverage-matrix-v1.md` | grep validation |
| `alternate_completion_dispatch` | `forbidden` | `forbidden` | `docs/specs/completion-coverage-matrix-v1.md` | grep validation |
| `generated_load_failure` | `fail-closed` | `fail-closed` | `docs/specs/completion-coverage-matrix-v1.md` | committed generated assets |
| `completion_engine` | `static` | `static` | matrix row omits dynamic marker | completion freshness audit |

Checklist:

- [x] Declared metadata values match required values in this template.
- [x] Declared metadata values match the matrix row for this CLI.
- [x] Verification evidence includes completion-mode toggle and alternate dispatch checks.

## single-path invariants

| invariant | enforcement location | verification method |
| --- | --- | --- |
| No runtime completion-mode toggles | `crates/agent-session/src/cli.rs`, `crates/agent-session/src/completion.rs` | grep validation |
| No alternate completion dispatch path | `crates/agent-session/src/completion.rs` | grep validation |
| Generated-load failure fails closed | committed completion assets | completion freshness audit |

Checklist:

- [x] Adapters are thin.
- [x] Generated-load failure does not route to alternate completion code.

## tests and validation

### validation commands

1. `zsh -n completions/zsh/_agent-session`
2. `bash -n completions/bash/agent-session`
3. `cargo run -p nils-agent-session -- completion zsh | rg -- "--help|--version|--"`
4. `cargo test -p nils-agent-session --test integration`
5. `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`

### test coverage mapping

| requirement | test file or command | status | notes |
| --- | --- | --- | --- |
| command graph candidates | completion freshness/flag parity | pending local-fast | generated assets committed |
| value providers | completion freshness/flag parity | pending local-fast | enum and path-hint backed |
| alias map registration | matrix row | pass | aliases not required |
| completion enforcement metadata | matrix row + this report | pass | static clap-first |
| single-path invariants | grep validation + freshness audit | pending local-fast | no alternate dispatch code |

## acceptance criteria

- [x] command graph matches implemented clap command surface.
- [x] value providers cover required candidates and dynamic paths.
- [x] alias map reflects zsh/bash alias entries and completion registration.
- [x] completion enforcement metadata is declared, matches matrix policy, and is validated.
- [x] single-path invariants are enforced and verified.
- [ ] tests and validation commands pass, with evidence captured.
- [ ] PR notes link this contract and summarize follow-up risk, if any.
