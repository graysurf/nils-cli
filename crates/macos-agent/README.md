# macos-agent

`macos-agent` is a macOS-oriented CLI for agent desktop automation. It provides parseable primitives for discovery, observation, and input
actions: window/app listing, window activation, click, type, hotkey, AX (Accessibility) actions, input-source switching, screenshot, and
wait helpers.

## Quick Start

```bash
# readiness check
macos-agent preflight --format json

# list targets
macos-agent windows list --format tsv
macos-agent apps list --format json

# activate + input
macos-agent window activate --app Terminal --wait-ms 1500
macos-agent input click --x 200 --y 160
macos-agent input type --text "hello world"
macos-agent input hotkey --mods cmd,shift --key 4
macos-agent input-source switch --id abc

# ax-first interaction
macos-agent ax list --app Arc --role AXButton --title-contains "New"
macos-agent ax click --app Arc --role AXLink --title-contains "YouTube" --nth 1 --allow-coordinate-fallback
macos-agent ax type --app Arc --role AXTextField --title-contains "Search" --text "lofi" --submit --allow-keyboard-fallback
macos-agent ax attr get --app Arc --role AXTextField --name AXValue
macos-agent ax action perform --app Arc --role AXButton --title-contains "Submit" --name AXPress
macos-agent ax session start --app Arc --session-id arc-main
macos-agent ax watch start --session-id arc-main --events AXTitleChanged,AXFocusedUIElementChanged

# observation
macos-agent observe screenshot --active-window --path ./tmp/macos-agent.png

# stabilization waits
macos-agent wait app-active --app Terminal --timeout-ms 1500
macos-agent wait window-present --app Terminal --window-title-contains Inbox --timeout-ms 1500
macos-agent wait ax-present --app Arc --role AXButton --title-contains "New" --timeout-ms 2000
macos-agent wait ax-unique --app Arc --role AXTextField --title-contains "Search" --timeout-ms 2000

# gate + postcondition for mutating AX actions
macos-agent ax click --app Arc --role AXButton --title-contains "Submit" \
  --gate-app-active --gate-window-present --gate-ax-unique \
  --postcondition-focused true --wait-timeout-ms 2000 --wait-poll-ms 75

# selector-frame screenshot
macos-agent observe screenshot --active-window --role AXButton --title-contains "Play" \
  --selector-padding 12 --path ./tmp/macos-agent-selector.png

# diff-aware screenshot publish
macos-agent observe screenshot --active-window --path ./tmp/macos-agent.png \
  --if-changed --if-changed-threshold 2

# one-shot debug bundle
macos-agent debug bundle --active-window --format json
```

## Command Surface

- `preflight`
  - `macos-agent preflight [--strict] [--include-probes]`
  - JSON output includes `result.permissions` with unified fields: `screen_recording`, `accessibility`, `automation`, `ready`, `hints`.
- `windows`
  - `macos-agent windows list [--app <name>] [--window-title-contains <name>] [--on-screen-only]`
- `apps`
  - `macos-agent apps list`
- `window`
  - `macos-agent window activate (--window-id <id> | --active-window | --app <name>`
    `[--window-title-contains <name>] | --bundle-id <bundle_id>) [--wait-ms <ms>] [--reopen-on-fail]`
- `input`
  - `macos-agent input click --x <px> --y <px> [--button <left|right|middle>] [--count <n>] [--pre-wait-ms <ms>] [--post-wait-ms <ms>]`
  - `macos-agent input type --text <text> [--delay-ms <ms>] [--submit]`
  - `macos-agent input hotkey --mods <cmd,ctrl,alt,shift,fn> --key <key>`
- `input-source`
  - `macos-agent input-source current`
  - `macos-agent input-source switch --id <source_id|abc|us>`
- `ax`
  - `macos-agent ax list [--session-id <id> | --app <name> | --bundle-id <bundle_id>]`
    `[--window-title-contains <text>] [--role <AXRole>] [--title-contains <text>] [--identifier-contains <text>]`
    `[--value-contains <text>] [--subrole <AXSubrole>] [--focused <bool>] [--enabled <bool>] [--max-depth <n>] [--limit <n>]`
  - `macos-agent ax click [selector flags...] [target flags...] [--match-strategy <contains|exact|prefix|suffix|regex>]`
    `[--selector-explain] [--reselect-before-click] [--allow-coordinate-fallback]`
    `[--fallback-order <ax-press,ax-confirm,frame-center,coordinate>] [--gate-app-active] [--gate-window-present]`
    `[--gate-ax-present] [--gate-ax-unique] [--wait-timeout-ms <ms>] [--wait-poll-ms <ms>]`
    `[--gate-timeout-ms <ms>] [--gate-poll-ms <ms>] [--postcondition-focused <bool>]`
    `[--postcondition-attribute <AXAttr>] [--postcondition-attribute-value <value>]`
    `[--postcondition-timeout-ms <ms>] [--postcondition-poll-ms <ms>]`
  - `macos-agent ax type [selector flags...] [target flags...] --text <text>`
    `[--match-strategy <contains|exact|prefix|suffix|regex>] [--selector-explain] [--clear-first] [--submit] [--paste]`
    `[--allow-keyboard-fallback] [--gate-app-active] [--gate-window-present] [--gate-ax-present] [--gate-ax-unique]`
    `[--wait-timeout-ms <ms>] [--wait-poll-ms <ms>] [--gate-timeout-ms <ms>] [--gate-poll-ms <ms>]`
    `[--postcondition-focused <bool>] [--postcondition-attribute <AXAttr>] [--postcondition-attribute-value <value>]`
    `[--postcondition-timeout-ms <ms>] [--postcondition-poll-ms <ms>]`
  - `macos-agent ax attr get [selector flags...] [target flags...] --name <AXAttribute>`
  - `macos-agent ax attr set [selector flags...] [target flags...] --name <AXAttribute> --value <value> [--value-type <string|number|bool|json|null>]`
  - `macos-agent ax action perform [selector flags...] [target flags...] --name <AXAction>`
  - `macos-agent ax session start [--session-id <id>] [--app <name> | --bundle-id <bundle_id>] [--window-title-contains <text>]`
  - `macos-agent ax session list`
  - `macos-agent ax session stop --session-id <id>`
  - `macos-agent ax watch start --session-id <id> [--watch-id <id>] [--events <comma-separated-AX-notifications>] [--max-buffer <n>]`
  - `macos-agent ax watch poll --watch-id <id> [--limit <n>] [--drain|--no-drain]`
  - `macos-agent ax watch stop --watch-id <id>`
- `observe`
  - `macos-agent observe screenshot (--window-id <id> | --active-window | --app <name> [--window-title-contains <name>])`
    `[--path <file>] [--image-format <png|jpg|webp>] [--if-changed] [--if-changed-baseline <path>]`
    `[--if-changed-threshold <bits>] [selector flags...] [--selector-padding <px>]`
- `debug`
  - `macos-agent debug bundle [--window-id <id> | --active-window | --app <name> [--window-title-contains <name>]] [--output-dir <path>]`
- `wait`
  - `macos-agent wait sleep --ms <ms>`
  - `macos-agent wait app-active (--app <name> | --bundle-id <bundle_id>) [--timeout-ms <ms>] [--poll-ms <ms>]`
  - `macos-agent wait window-present (--window-id <id> | --active-window | --app <name> [--window-title-contains <name>])`
    `[--timeout-ms <ms>] [--poll-ms <ms>]`
  - `macos-agent wait ax-present [selector flags...] [target flags...] [--timeout-ms <ms>] [--poll-ms <ms>]`
  - `macos-agent wait ax-unique [selector flags...] [target flags...] [--timeout-ms <ms>] [--poll-ms <ms>]`
- `scenario`
  - `macos-agent scenario run --file <scenario.json>`
- `profile`
  - `macos-agent profile validate --file <profile.json>`
  - `macos-agent profile init [--name <profile-name>] [--path <output.json>]`
- `completion`
  - `macos-agent completion <bash|zsh|fish|powershell|elvish>` (prints shell completion script to `stdout`)

## Global Flags

- `--format <text|json|tsv>`
- `--error-format <text|json>`
- `--dry-run`
- `--retries <n>`
- `--retry-delay-ms <ms>`
- `--timeout-ms <ms>`
- `--trace`
- `--trace-dir <path>`

Notes:

- `--format tsv` is only supported by `windows list` and `apps list`.
- Canonical flags: use `--window-title-contains` and `input type --submit`.
- `--dry-run` guarantees no OS automation command execution for mutating actions.
- `--error-format json` emits machine-parseable error payloads on `stderr`.
- `--trace` writes per-command trace artifacts to `AGENT_HOME/out/macos-agent-trace/`.
- `--trace-dir` overrides trace artifact output directory.
- When trace mode is enabled, `macos-agent` verifies trace directory writability before running actions.

## Output Contract

- Success:
  - Writes payload to `stdout` only.
  - `stderr` remains empty.
- Error:
  - Writes message to `stderr` only.
  - `stdout` remains empty.
  - Messages start with `error:`.

JSON envelope (`--format json`):

```json
{
  "schema_version": 1,
  "ok": true,
  "command": "input.click",
  "result": {
    "policy": {
      "dry_run": false,
      "retries": 1,
      "retry_delay_ms": 150,
      "timeout_ms": 4000
    },
    "meta": {
      "action_id": "input.click-20260101-000000-7",
      "elapsed_ms": 12
    }
  }
}
```

Preflight permission contract (`macos-agent --format json preflight`):

```json
{
  "result": {
    "permissions": {
      "screen_recording": "unknown",
      "accessibility": "ready",
      "automation": "ready",
      "ready": true,
      "hints": []
    }
  }
}
```

Mutating action commands (`window activate`, `input click`, `input type`, `input hotkey`, `ax click`, `ax type`) always include
`result.policy` in JSON output so agent-side retry and timeout policy can be parsed without guessing defaults. These action results also
include `result.meta.attempts_used` so flaky steps can be detected quickly.

Exit codes:

- `0`: success
- `1`: runtime failure
- `2`: usage error

Platform guard: `macos-agent` is macOS-only. On non-macOS hosts every subcommand short-circuits with exit code `2` and the
message `error: macos-agent is only supported on macOS`. The deterministic test mode (`AGENTS_MACOS_AGENT_TEST_MODE=1`) bypasses
this guard so CI-safe integration tests can run on Linux.

Error envelope (`--error-format json`):

```json
{
  "schema_version": 1,
  "ok": false,
  "error": {
    "category": "runtime",
    "operation": "input.click",
    "message": "input.click failed via `cliclick` (exit 2): cliclick failed",
    "hints": ["Check macOS Accessibility/Automation permissions if this action controls System Events."]
  }
}
```

## Permission Matrix

| Capability                | Required setup                                                                       | Typical failure symptom                                           | Mitigation                                                                                   |
| ------------------------- | ------------------------------------------------------------------------------------ | ----------------------------------------------------------------- | -------------------------------------------------------------------------------------------- |
| Accessibility             | Terminal host allowed in **System Settings > Privacy & Security > Accessibility**    | click/type/hotkey fail                                            | Enable the shell host app (Terminal/iTerm/etc.) and retry                                    |
| Automation (Apple Events) | Terminal host allowed in **System Settings > Privacy & Security > Automation**       | activation / System Events probe fails                            | Allow the terminal app to control System Events                                              |
| Screen Recording          | Terminal host allowed in **System Settings > Privacy & Security > Screen Recording** | observe screenshot fails                                          | Enable Screen Recording for terminal host                                                    |
| `osascript` binary        | Preinstalled on macOS; required for AppleScript backend + preflight probes           | preflight reports missing `osascript`                             | Reinstall macOS command-line tools if missing (`xcode-select --install`)                     |
| `cliclick` binary         | Installed and on `PATH`                                                              | preflight reports missing `cliclick`                              | `brew install cliclick`                                                                      |
| `hs` (Hammerspoon CLI)    | Required for Hammerspoon AX backend; install Hammerspoon and enable `hs.ipc`         | `ax attr/action/session/watch` fail with backend-unavailable hint | `brew install --cask hammerspoon`, then add `require('hs.ipc')` to `~/.hammerspoon/init.lua` |
| `im-select` binary        | Required by `input-source current` and `input-source switch`                         | `input-source` commands fail with `missing dependency im-select`  | `brew install im-select`                                                                     |

See the workspace [`BINARY_DEPENDENCIES.md`](../../BINARY_DEPENDENCIES.md) for the canonical install matrix (rows for `hs`, `cliclick`, `im-select`, and `osascript`).

## AX Backend Capability Matrix

| Backend preference    | `ax list/click/type`                                                                              | `ax attr/action/session/watch` | Notes                                                                                                         |
| --------------------- | ------------------------------------------------------------------------------------------------- | ------------------------------ | ------------------------------------------------------------------------------------------------------------- |
| `auto` (default)      | Hammerspoon `hs` first; falls back to AppleScript (JXA) only when the Hammerspoon path is missing | Hammerspoon-only               | Best default for resilience; fallback does not apply to extended AX commands and surfaces a "fallback" hint   |
| `hammerspoon` / `hs`  | Hammerspoon-only (no JXA fallback)                                                                | Supported                      | Full AX surface; requires the `hs` CLI on `PATH` and `require('hs.ipc')` enabled in `~/.hammerspoon/init.lua` |
| `applescript` / `jxa` | AppleScript (JXA via `osascript`) only                                                            | Not supported directly         | Extended AX commands still require Hammerspoon; selecting this preference does not avoid the dependency       |

Preflight emits an `ax_backend_capabilities` row so operators can verify backend mode and fallback expectations before failures.
The row carries the `AX backend preference=<auto|hammerspoon|applescript>` message; when the preference is anything other than
`hammerspoon`, the row also adds a hint pointing operators back to `AGENTS_MACOS_AGENT_AX_BACKEND` and
`preflight --include-probes`.

## Reliability Boundaries and Practices

Desktop UI automation is inherently brittle due to animation timing, focus drift, and app responsiveness. Use these defaults for better
stability:

- Always activate context before input:
  - `window activate ... --wait-ms 1000`
- Add small waits around click chains:
  - `input click ... --pre-wait-ms 100 --post-wait-ms 100`
- Enable retries for transient failures:
  - `--retries 2 --retry-delay-ms 150`
- Keep timeouts explicit for slow apps:
  - `--timeout-ms 5000`
- Use `wait app-active` / `wait window-present` before mutating actions.
- Prefer `ax click/type` first, then opt in to fallback flags when app AX trees are unstable.
- AX backend selection defaults to `auto` (Hammerspoon `hs` CLI first, AppleScript JXA fallback for `ax list/click/type`).
  - Override with `AGENTS_MACOS_AGENT_AX_BACKEND=hammerspoon|applescript|auto`.
  - Aliases `hs` and `jxa` are also accepted (mapped to `hammerspoon` and `applescript` respectively).
  - Extended AX commands (`attr`, `action`, `session`, `watch`) always require Hammerspoon; the `applescript` and `auto`
    paths do not provide a JXA fallback for them.
  - In deterministic test mode (`AGENTS_MACOS_AGENT_TEST_MODE=1`) without an explicit override, the resolver picks
    `applescript` to keep the JXA stub deterministic.

## Command Decision Matrix (AX/Input/Wait/Fallback/Backend)

Use this matrix to pick commands consistently. Start from the decision row, then use the mapped troubleshooting row on failure.

| Decision ID | When                                                               | Command choice (`ax`/`input`/`wait`)                                                                                    | Fallback policy                                             | Backend policy                                                                          | Troubleshooting row |
| ----------- | ------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------- | --------------------------------------------------------------------------------------- | ------------------- |
| `D1`        | Target element is discoverable in AX tree                          | `ax list` -> `ax click` / `ax type`; gate with `wait app-active` and (if needed) `wait window-present`                  | Keep fallback flags off first                               | `auto` default is preferred; see `AX Backend Capability Matrix`                         | `T3`, `T5`          |
| `D2`        | AX selector exists but can be unstable across reruns               | Same as `D1`, plus `--allow-coordinate-fallback` or `--allow-keyboard-fallback`; keep wait gates explicit               | Opt in per command (`ax click/type` only)                   | Keep `auto` so `ax click/type` can fall back to JXA when Hammerspoon is unavailable     | `T4`, `T5`          |
| `D3`        | AX path is unavailable for the target app                          | `window activate` + `input click` / `input type` / `input hotkey`; use `wait app-active/window-present` before mutation | No AX fallback path; use coordinate/keyboard input directly | Backend-independent for pure `input` flow                                               | `T1`, `T2`          |
| `D4`        | Need extended AX operations (`attr`, `action`, `session`, `watch`) | Use `ax attr/action/session/watch` commands; add wait gate before mutating action                                       | No fallback support for extended AX commands                | Requires Hammerspoon runtime support (see `AX Backend Capability Matrix`)               | `T5`                |
| `D5`        | Text entry depends on deterministic keyboard layout                | `input-source current` -> `input-source switch --id <id>` -> `ax type` or `input type`                                  | Prefer paste/submit flow when IME variance is high          | Backend-independent for `input-source`; AX typing still follows `D1`/`D2` backend rules | `T6`                |

This AX-first + fallback policy avoids brittle coordinate-only flows while keeping a reliable escape hatch.

## Debug Bundle Triage Flow

Copy-paste triage flow to collect deterministic artifacts after a flaky or failed run:

```bash
OUT="${AGENT_HOME:-$HOME/.agents}/out/macos-agent-debug-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$OUT"

# 1) capture debug bundle + artifact index
macos-agent debug bundle --active-window --output-dir "$OUT" --format json > "$OUT/debug-bundle.json"

# 2) inspect artifact index and partial failures
jq '.result.artifact_index_path, .result.partial_failure, (.result.artifacts[] | {id, ok, path, error})' \
  "$OUT/debug-bundle.json"

# 3) optional selector-frame screenshot for visual targeting proof
macos-agent observe screenshot --active-window --role AXButton --title-contains "Play" \
  --selector-padding 12 --path "$OUT/selector-frame.png" --format json > "$OUT/selector-frame.json"
```

Artifact index notes:

- `result.artifact_index_path` points to the canonical artifact index JSON.
- `result.partial_failure=true` means some artifacts failed but bundle capture still completed.
- Each artifact entry records `id`, `ok`, `path`, and `error` for fast triage routing.

## Deterministic Test Mode

Set `AGENTS_MACOS_AGENT_TEST_MODE=1` to run with deterministic fixtures and without controlling the real desktop. This mode is used by
CI-safe integration tests.

## Opt-in Real macOS E2E Checks

`crates/macos-agent/tests/e2e_real_macos.rs` contains real-desktop checks for:

- TCC signal quality in `preflight` (Accessibility/Automation statuses + hints)
- focus drift detection path for activation + `wait app-active`

`crates/macos-agent/tests/e2e_real_apps.rs` contains app workflow checks for:

- Finder activation + window presence + navigation hotkeys + screenshot evidence
- Arc YouTube flow (open home, click 3 videos, play/pause, comment checkpoint)
- Spotify flow (UI track click, play/pause toggles, player-state probe)
- Cross-app Arc↔Spotify focus recovery and matrix artifact index output

These checks are disabled by default and require explicit opt-in:

```bash
MACOS_AGENT_REAL_E2E=1 cargo test -p nils-macos-agent --test e2e_real_macos
MACOS_AGENT_REAL_E2E=1 MACOS_AGENT_REAL_E2E_MUTATING=1 MACOS_AGENT_REAL_E2E_APP=Finder \
  cargo test -p nils-macos-agent --test e2e_real_macos
MACOS_AGENT_REAL_E2E=1 MACOS_AGENT_REAL_E2E_MUTATING=1 MACOS_AGENT_REAL_E2E_APPS=finder \
  cargo test -p nils-macos-agent --test e2e_real_apps -- finder_navigation_and_state_checks --nocapture
MACOS_AGENT_REAL_E2E=1 MACOS_AGENT_REAL_E2E_MUTATING=1 MACOS_AGENT_REAL_E2E_APPS=arc,spotify,finder \
  MACOS_AGENT_REAL_E2E_PROFILE=default-1440p \
  cargo test -p nils-macos-agent --test e2e_real_apps -- matrix_runner_supports_app_subset_selection_real --nocapture
```

Real-app E2E environment variables:

- `MACOS_AGENT_REAL_E2E=1`: enable real desktop tests.
- `MACOS_AGENT_REAL_E2E_MUTATING=1`: allow mutating desktop actions (click/type/hotkey).
- `MACOS_AGENT_REAL_E2E_APPS=arc,spotify,finder`: select app subset in deterministic order.
  - Unsupported app names are treated as configuration errors (fail fast).
- `MACOS_AGENT_REAL_E2E_PROFILE=default-1440p`: choose coordinate profile fixture.
- `MACOS_AGENT_REAL_E2E_INPUT_SOURCE=com.apple.keylayout.ABC` (or `abc`): optional; if set, tests switch to the target input source once via
  `im-select` before text-entry flows.
- `MACOS_AGENT_REAL_E2E_STEP_TIMEOUT_MS=15000`: optional per-step timeout guard for real-app helper commands.
- `MACOS_AGENT_REAL_E2E_ITERATIONS=5`: optional short-loop repetition count for matrix runs.

Input-method notes for reliability:

- Arc YouTube navigation uses address-bar focus + clipboard paste + `Return` (not per-key character typing), then verifies the active URL
  contains `youtube.com` and is not a Google search URL.
- Spotify search input uses clipboard paste (`Cmd+A` + `Cmd+V`) and then `Return`, avoiding IME-dependent character typing.
- If you want deterministic keyboard layout, install `im-select` (`brew install im-select`) and set `MACOS_AGENT_REAL_E2E_INPUT_SOURCE=abc`.
- You can verify/switch layout directly with:
  - `macos-agent --format json input-source current`
  - `macos-agent --format json input-source switch --id abc`

Real-app artifact notes:

- Every real-app scenario writes `steps.jsonl` and `step-summary.json` under its artifact directory.
- `artifact-index.json` includes per-scenario `step_ledger_path`, `failing_step_id`, and `last_successful_step_id`.
- Real-app checks are manual/local validation flows and should not be included in default CI jobs.

## Immediate Feedback Loop

### Workflow 1: readiness then action probe

```bash
macos-agent --format json preflight --include-probes
macos-agent --format json window activate --app Terminal --wait-ms 1200 --retries 1
macos-agent --format json wait app-active --app Terminal --timeout-ms 1500
```

### Workflow 2: machine-parseable failure triage

```bash
macos-agent --error-format json --trace input click --x 200 --y 160
# Read latest trace in AGENT_HOME/out/macos-agent-trace/
```

### Workflow 3: iterate with scenario file + profile checks

```bash
macos-agent profile validate --file crates/macos-agent/tests/fixtures/real_e2e_profile_default_1440p.json
macos-agent --format json scenario run --file crates/macos-agent/tests/fixtures/scenario-basic.json
macos-agent profile init --name local-1440p --path "$AGENT_HOME/out/local-profile.json"
```

## Troubleshooting matrix

Use the `Decision ID` from `Command Decision Matrix` to choose the row quickly.

| ID   | Symptom                                                          | Next command                                                                             | What to inspect                                                                                           | Decision row     |
| ---- | ---------------------------------------------------------------- | ---------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------- | ---------------- |
| `T1` | `not authorized` or Apple Events failures                        | `macos-agent --format json preflight --include-probes`                                   | `error.hints`, Automation/Accessibility rows                                                              | `D3`             |
| `T2` | Flaky click/input behavior                                       | `macos-agent --trace --error-format json input click ...`                                | latest trace JSON (`attempts_used`, timeout/retry policy)                                                 | `D3`             |
| `T3` | AX selector no match / ambiguous match                           | `macos-agent --format json ax list --app <name> --role <AXRole> --title-contains <text>` | node candidates (`node_id`, `role`, `title`, `identifier`) and refine selector / `--nth`                  | `D1`             |
| `T4` | AX press/type fails but coordinate/keyboard path should continue | rerun with `ax click --allow-coordinate-fallback` or `ax type --allow-keyboard-fallback` | whether `used_coordinate_fallback` / `used_keyboard_fallback` is true in JSON result                      | `D2`             |
| `T5` | Hammerspoon AX backend unavailable                               | `hs -t 1 -q -c 'return \"ok\"'`                                                          | ensure Hammerspoon is running and `require('hs.ipc')` is enabled, or keep backend `auto` for JXA fallback | `D1`, `D2`, `D4` |
| `T6` | Input source mismatch before typing                              | `macos-agent --format json input-source current` then `... switch --id abc`              | current source id and switch result (`switched=true`)                                                     | `D5`             |
| `T7` | Trace enabled but command does not start                         | `macos-agent --trace --trace-dir <path> --error-format json preflight`                   | `trace.write` error and writable-path hint                                                                | `D3`             |
| `T8` | Real-app scenario failed mid-flow                                | run target `e2e_real_apps` command with `--nocapture`                                    | `steps.jsonl`, `step-summary.json`, `artifact-index.json`                                                 | `D1`, `D2`, `D3` |
| `T9` | Profile coordinate drift                                         | `macos-agent profile validate --file <profile.json>`                                     | key-path validation errors and bounds issues                                                              | `D3`             |

## Docs

- [Docs index](docs/README.md)
