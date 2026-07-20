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
| `agent-session start` | `crates/agent-session/src/cli.rs` | agent/coordination/format enums, path hints, prompt and runtime flags | interactive Codex, Claude, or Hermes handoff |
| `agent-session run` | `crates/agent-session/src/cli.rs` | agent/coordination/format enums, path hints, prompt and runtime flags | one-shot Codex or Claude task |
| `agent-session list` | `crates/agent-session/src/cli.rs` | format enum | service-readable inventory |
| `agent-session command` | `crates/agent-session/src/cli.rs` | session id + format enum | prints attach commands |
| `agent-session attach` | `crates/agent-session/src/cli.rs` | session id + tmux binary path hint | local tmux attach |
| `agent-session logs` | `crates/agent-session/src/cli.rs` | session id, tail count, tmux path, format enum | tmux capture, run log, or startup diagnostic |
| `agent-session send` | `crates/agent-session/src/cli.rs` | session id, repeatable key enum, tmux path, format enum | text and named-key input |
| `agent-session glance` | `crates/agent-session/src/cli.rs` | session id, tail count, tmux path, format enum | bounded dashboard projection |
| `agent-session resume` | `crates/agent-session/src/cli.rs` | session id, tmux path, format enum | exact provider-metadata resume |
| `agent-session activity` | `crates/agent-session/src/cli.rs` | nested lifecycle commands | turn-state and provider setup group |
| `agent-session activity event` | `crates/agent-session/src/cli.rs` | session id, stdin flag, format enum | normalized metadata event ingestion |
| `agent-session activity status` | `crates/agent-session/src/cli.rs` | session id, format enum | durable turn-state read |
| `agent-session activity doctor` | `crates/agent-session/src/cli.rs` | optional provider enum, format enum | provider integration diagnostics |
| `agent-session activity setup` | `crates/agent-session/src/cli.rs` | provider enum, setup mode flags, digest and path values, format enum | preview/apply/repair/remove lifecycle setup |
| `agent-session activity hook` | `crates/agent-session/src/cli.rs` | provider/event enums and stdin flags | hidden provider hook bridge |
| `agent-session activity notify` | `crates/agent-session/src/cli.rs` | provider enum and notifier argv values | hidden provider notification bridge |
| `agent-session work-context` | `crates/agent-session/src/cli.rs` | nested advisory and enforce commands | managed coordination group |
| `agent-session work-context status` | `crates/agent-session/src/cli.rs` | format enum | self-targeting presence/context read |
| `agent-session work-context set` | `crates/agent-session/src/cli.rs` | tier, repository, path, issue/PR, plan reference, and format flags | self-targeting context replacement |
| `agent-session work-context clear` | `crates/agent-session/src/cli.rs` | format enum | self-targeting context removal |
| `agent-session work-context advise` | `crates/agent-session/src/cli.rs` | optional operation-targets path and format enum | privacy-safe overlap evaluation |
| `agent-session work-context acknowledge` | `crates/agent-session/src/cli.rs` | bounded duration and format enum | exact-warning suppression |
| `agent-session work-context claim` | `crates/agent-session/src/cli.rs` | session/file/capability paths, idempotency and revision values, format enum | authenticated claim acquisition |
| `agent-session work-context show` | `crates/agent-session/src/cli.rs` | session id, capability path, format enum | authenticated context read |
| `agent-session work-context check` | `crates/agent-session/src/cli.rs` | self/session/candidate selectors, capability path, format enum | conflict check without acquisition |
| `agent-session work-context renew` | `crates/agent-session/src/cli.rs` | claim/revision/idempotency values, capability path, format enum | claim renewal |
| `agent-session work-context release` | `crates/agent-session/src/cli.rs` | claim/revision/idempotency values, capability path, format enum | claim release |
| `agent-session work-context admit` | `crates/agent-session/src/cli.rs` | claim/revision, targets/token/capability paths, operation and idempotency values, format enum | mutation admission |
| `agent-session work-context complete` | `crates/agent-session/src/cli.rs` | lease/revision, token/capability paths, outcome and idempotency values, format enum | admitted operation completion |
| `agent-session work-context reconcile` | `crates/agent-session/src/cli.rs` | lease/revision, proof/capability paths, idempotency value, format enum | missed-completion reconciliation |
| `agent-session broker` | `crates/agent-session/src/cli.rs` | nested broker commands | broker inspection/recovery group |
| `agent-session broker status` | `crates/agent-session/src/cli.rs` | session id, optional capability path, format enum | privacy-safe broker status |
| `agent-session broker adopt` | `crates/agent-session/src/cli.rs` | session id, proof path, idempotency value, format enum | guarded broker adoption |
| `agent-session broker reconcile` | `crates/agent-session/src/cli.rs` | session id, proof path, operation/revision/attestation/idempotency values, format enum | guarded broker reconciliation |
| `agent-session broker stop` | `crates/agent-session/src/cli.rs` | internal broker identity and capability paths | hidden lifecycle helper |
| `agent-session broker heartbeat` | `crates/agent-session/src/cli.rs` | internal broker identity and capability paths | hidden heartbeat sidecar |
| `agent-session message` | `crates/agent-session/src/cli.rs` | nested mailbox commands | private mailbox group |
| `agent-session message send` | `crates/agent-session/src/cli.rs` | sender/recipient ids, body/capability paths, reply/expiry/idempotency values, format enum | bounded private send |
| `agent-session message inbox` | `crates/agent-session/src/cli.rs` | session id, capability path, state/cursor/limit values, format enum | mailbox metadata page |
| `agent-session message show` | `crates/agent-session/src/cli.rs` | session/message ids, capability path, format enum | authenticated body read |
| `agent-session message ack` | `crates/agent-session/src/cli.rs` | session/message/revision/idempotency values, capability path, format enum | message acknowledgement |
| `agent-session message reply` | `crates/agent-session/src/cli.rs` | session/message/revision/idempotency values, body/capability paths, format enum | bounded reply |
| `agent-session message wait` | `crates/agent-session/src/cli.rs` | session/message/revision/timeout values, capability path, format enum | bounded revision wait |
| `agent-session serve` | `crates/agent-session/src/cli.rs` | bind/token/machine options and tmux path hint | HTTP/WebSocket control plane |
| `agent-session codex-app-server-proxy` | `crates/agent-session/src/cli.rs` | session id and Unix socket paths | hidden managed Codex bridge |
| `agent-session delete` | `crates/agent-session/src/cli.rs` | session id, tmux path, format enum | verified runtime and state deletion |
| `agent-session completion` | `crates/agent-session/src/cli.rs` | shell enum | `bash` and `zsh` |

Checklist:

- [x] Every supported subcommand path is listed.
- [x] Long/short flags are represented by clap metadata.
- [x] Hidden internal paths are explicitly called out; there are no deprecated paths.

## value providers

| argument or flag | provider type | source location | context-aware behavior | tests |
| --- | --- | --- | --- | --- |
| `--agent` | `ValueEnum` | `crates/agent-session/src/cli.rs` | static `codex`, `claude` values | completion freshness/flag parity |
| `--coordination-mode` | `ValueEnum` | `crates/agent-session/src/cli.rs` | static `advisory`, `enforce`, `off` values | completion freshness/flag parity |
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
4. `zsh -f tests/zsh/completion.test.zsh`
5. `cargo test -p nils-agent-session --test integration`
6. `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`

### test coverage mapping

| requirement | test file or command | status | notes |
| --- | --- | --- | --- |
| command graph candidates | completion freshness/flag parity | pass | generated assets committed |
| value providers | completion freshness/flag parity | pass | enum and path-hint backed |
| alias map registration | matrix row | pass | aliases not required |
| completion enforcement metadata | matrix row + this report | pass | static clap-first |
| single-path invariants | grep validation + freshness audit | pass | no alternate dispatch code |

## acceptance criteria

- [x] command graph matches implemented clap command surface.
- [x] value providers cover required candidates and dynamic paths.
- [x] alias map reflects zsh/bash alias entries and completion registration.
- [x] completion enforcement metadata is declared, matches matrix policy, and is validated.
- [x] single-path invariants are enforced and verified.
- [x] tests and validation commands pass, with evidence captured.
- [x] PR notes link this contract and summarize follow-up risk, if any.
