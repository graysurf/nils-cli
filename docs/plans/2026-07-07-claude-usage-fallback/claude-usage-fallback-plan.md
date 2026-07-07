# Plan: Stabilize Claude usage fallback for agent-console

## Overview

Move Claude usage source selection into `nils-cli`'s `claude-cli` and keep
`sympoies-infra` as the host-local deployment/reader layer. The core change is a
new service-consumable `claude-cli usage --format json --source auto` command
that tries OAuth usage refresh, falls back to a Claude Code CLI `/usage` PTY
probe, and finally serves last-good cache when live sources are unavailable.
`sympoies-infra` then consumes that JSON instead of owning Claude-specific usage
logic.

## Read First

- Primary source:
  `docs/plans/2026-07-07-claude-usage-fallback/claude-usage-fallback-discussion-source.md`
- Source type: discussion-to-implementation-doc
- Open questions carried into execution: none; the user explicitly constrained
  this to L2 maximum and asked for issue, implementation, PR delivery, and deploy.

## Scope

- In scope:
  - Add `claude-cli usage` with JSON output and source selection.
  - Add CLI PTY `/usage` fallback with deterministic parser coverage.
  - Keep the existing shared `claude-prompt-segment/usage.json` cache compatible.
  - Update `sympoies-infra`'s `agent-console-usage` reader to consume the new JSON
    command.
  - Validate, deliver PRs, install/release as needed, restart the host reader, and
    smoke `/api/usage`.
- Out of scope:
  - Web-cookie fallback.
  - Anthropic Admin API integration.
  - Historical cost/token charts from Claude JSONL logs.
  - Any `agent-console` UI contract change unless the reader contract forces it.

## Assumptions

1. The host has a usable Claude Code login or a usable `claude` binary when PTY
   fallback is needed; if both OAuth and CLI fail, stale cache remains the only
   correct output.
2. CLI PTY parsing can target the stable labels that Claude Code currently
   exposes (`Current session`, `Current week`) while remaining tolerant of ANSI and
   line wrapping.
3. `sympoies-infra` should not gain direct Anthropic endpoint or TUI parsing code.
4. One L2 tracking issue in `sympoies/nils-cli` is enough; infra PR/deploy
   evidence can be linked from that issue.

## Sprint 1: native `claude-cli usage` source selection

**Goal**: `claude-cli` exposes a tested JSON usage command that can serve
agent-console and other machine consumers.

**Demo/Validation**:

- `cargo test -p nils-claude-cli`
- `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
- Manual smoke with fake endpoint and fake `claude` binary in tests.

### Task 1.1: Add failing coverage for auto source fallback

- **Location**:
  - `crates/claude-cli/tests/integration.rs`
- **Description**: Add an integration test where OAuth refresh fails, a fake
  `claude` binary returns a `/usage` panel, and `claude-cli usage --format json
  --source auto` emits 5h/weekly windows with `source: "cli"`.
- **Dependencies**:
  - none
- **Complexity**: 3
- **Acceptance criteria**:
  - Test fails on current code because `usage` is not implemented.
- **Validation**:
  - `cargo test -p nils-claude-cli prompt_segment`

### Task 1.2: Implement JSON usage command and cache contract

- **Location**:
  - `crates/claude-cli/src/cli.rs`
  - `crates/claude-cli/src/main.rs`
  - `crates/claude-cli/src/prompt_segment/mod.rs`
  - `crates/claude-cli/src/prompt_segment/cache.rs`
  - `crates/claude-cli/src/prompt_segment/client.rs`
  - `crates/claude-cli/src/prompt_segment/render.rs`
  - `crates/claude-cli/src/prompt_segment/usage.rs`
  - `crates/claude-cli/README.md`
- **Description**: Add `usage --format json --source auto|oauth|cli|cache` with a
  versioned, secret-free envelope. Reuse existing auth, client, render parser, and
  cache helpers where practical.
- **Dependencies**:
  - Task 1.1
- **Complexity**: 5
- **Acceptance criteria**:
  - JSON schema is stable and documented.
  - Token values never appear in stdout, stderr, or JSON metadata.
  - Existing `prompt-segment` output remains compatible.
- **Validation**:
  - `cargo test -p nils-claude-cli`

### Task 1.3: Implement CLI `/usage` fallback parser

- **Location**:
  - `crates/claude-cli/src/prompt_segment/mod.rs`
  - `crates/claude-cli/src/prompt_segment/cache.rs`
  - `crates/claude-cli/src/prompt_segment/render.rs`
  - `crates/claude-cli/src/prompt_segment/usage.rs`
  - `crates/claude-cli/tests/integration.rs`
- **Description**: Add a bounded subprocess/PTY-style probe that runs `claude`,
  sends `/usage`, parses ANSI-stripped usage windows, and writes the shared cache
  only after successful parsing.
- **Dependencies**:
  - Task 1.2
- **Complexity**: 6
- **Acceptance criteria**:
  - Fake `claude` integration test drives the fallback without network.
  - Parser accepts used/left/remaining wording and reset timestamps when present.
  - Missing CLI, timeout, or unparseable output degrades to cache/stale output.
- **Validation**:
  - `cargo test -p nils-claude-cli`

## Sprint 2: infra reader consumes `claude-cli usage`

**Goal**: agent-console's host reader delegates Claude source selection to
`claude-cli` and keeps emitting the existing `/api/usage` shape.

**Demo/Validation**:

- `python3 scripts/test-agent-console-usage.py`
- `bash -n scripts/*.sh host/agent-console/bin/run-agent-console-usage
  host/agent-console/bin/agent_console_usage.py`
- `make config STACK=agent-console`

### Task 2.1: Switch the usage reader to the JSON command

- **Location**:
  - `graysurf/sympoies-infra:host/agent-console/bin/agent_console_usage.py`
  - `graysurf/sympoies-infra:scripts/test-agent-console-usage.py`
- **Description**: Invoke `claude-cli usage --format json --source auto` with the
  existing token/cache environment, map returned windows to provider windows, and
  keep existing stale-note behavior for command failure.
- **Dependencies**:
  - Task 1.3
- **Complexity**: 4
- **Acceptance criteria**:
  - Python tests cover success, stale, command missing, invalid JSON, and no secret
    emission.
- **Validation**:
  - `python3 scripts/test-agent-console-usage.py`

### Task 2.2: Update runbook docs and deployment notes

- **Location**:
  - `graysurf/sympoies-infra:host/agent-console/README.md`
  - `graysurf/sympoies-infra:docs/devlog/2026-07.md`
- **Description**: Document that Claude source selection lives in `claude-cli`
  and the reader is only a loopback adapter.
- **Dependencies**:
  - Task 2.1
- **Complexity**: 2
- **Acceptance criteria**:
  - Docs describe the new ownership boundary and deployment smoke.
- **Validation**:
  - `bash -n scripts/*.sh`

## Sprint 3: delivery and deploy

**Goal**: Merge the required PRs, install the new `claude-cli` on sympoies,
restart/deploy the agent-console usage path, and leave an explicit verification
URL/command for user acceptance.

**Demo/Validation**:

- `claude-cli usage --format json --source auto`
- `systemctl --user restart agent-console-usage.service`
- `curl -s http://127.0.0.1:8793/usage | jq '.providers[] | select(.provider=="claude")'`
- `scripts/smoke-agent-console.sh`

### Task 3.1: Deliver nils-cli PR and install/release local binary

- **Location**:
  - `sympoies/nils-cli`
- **Description**: Deliver the `claude-cli` PR, then install or release the
  resulting `claude-cli` binary to the host path used by sympoies.
- **Dependencies**:
  - Task 1.3
- **Complexity**: 4
- **Acceptance criteria**:
  - PR merged or explicitly blocked with issue evidence.
  - `~/.local/nils-cli/bin/claude-cli` on sympoies includes the `usage` command.
- **Validation**:
  - `claude-cli usage --format json --source auto`

### Task 3.2: Deliver sympoies-infra PR and deploy

- **Location**:
  - `graysurf/sympoies-infra`
- **Description**: Deliver the infra PR, reinstall/restart the host reader, and run
  the agent-console smoke path.
- **Dependencies**:
  - Task 2.2
  - Task 3.1
- **Complexity**: 4
- **Acceptance criteria**:
  - Infra PR merged.
  - `agent-console-usage.service` is restarted and healthy.
  - `/api/usage` returns Claude usage windows or a clear stale note.
- **Validation**:
  - `make deploy STACK=agent-console`
  - `scripts/smoke-agent-console.sh`
