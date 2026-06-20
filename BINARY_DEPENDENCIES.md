# External Tooling Dependencies

This document defines the external binaries and script-level tools used by the `nils-cli` workspace,
and provides recommended installation commands for Homebrew (macOS) and Linuxbrew (Linux).

## Scope and Intent

- Focus: runtime dependencies invoked by workspace CLIs, plus development/test tooling used by repo workflows.
- Source of truth: crate READMEs, runtime process invocations (`Command::new(...)`), and repository scripts.
- Goal: make environment setup predictable for contributors and CI-like local validation.

## 1. Runtime Dependencies (Core)

These tools are required for common command paths. Each row is anchored to at least one
`Command::new(...)` (or equivalent `shared_process` / `ProcessRequest`) call site in `crates/*/src`.

| Tool | Used By | Requirement Level | Install (brew/linuxbrew) |
|---|---|---|---|
| `git` | `git-scope`, `git-cli`, `git-summary`, `git-lock`, `repo-retro`, `semantic-commit` (via `git-scope`), `codex-cli`, `gemini-cli`, `opencode-cli`, `fzf-cli git-*`, `zsh-kit setup`, `zsh-kit plugin *`, `heuristic-inbox deliver` | Required | `brew install git` |
| `fzf` | `fzf-cli` interactive commands | Required (for `fzf-cli`) | `brew install fzf` |
| `grpcurl` | `api-grpc` unary backend (via `api-testing-core::grpc::runner`); overridable with `GRPCURL_BIN` | Required (for `api-grpc call` / suite gRPC cases) | `brew install grpcurl` |
| `ffmpeg` | `screen-record` on Linux (X11 + Wayland portal capture, audio mux) | Required on Linux | `brew install ffmpeg` |
| `codex` | `codex-cli auth login` and `codex-cli agent *` flows | Required for `codex-cli` runtime | Install from official Codex distribution |
| `ssh` | `codex-cli auth remote pull` remote token-authority transport | Required for `codex-cli auth remote pull` | Usually preinstalled (`brew install openssh`) |
| `gemini` | `gemini-cli auth login` flow | Required for `gemini-cli` login | Install from official Gemini CLI distribution |
| `opencode` | `opencode-cli agent *` flows | Required for `opencode-cli` runtime | Install from official OpenCode distribution |
| `curl` | `gemini-cli` auth refresh + rate-limit client | Required for `gemini-cli` auth flows | Usually preinstalled (`brew install curl`) |
| `security` | `claude-cli prompt-segment` Keychain credential lookup | Required on macOS unless `CLAUDE_PROMPT_SEGMENT_ACCESS_TOKEN` / `CLAUDE_PROMPT_SEGMENT_CREDENTIALS_JSON` is supplied | Preinstalled on macOS |
| `docker` | `docker-tools container *`, `docker-tools run *`, and Docker Compose v2 resolution for `docker-tools compose down` | Required for `docker-tools` Docker-backed commands | `brew install docker` |
| `osascript` | `macos-agent` AppleScript backend, preflight checks | Required on macOS for `macos-agent` | Preinstalled on macOS |
| `gh` | `git-cli open *` GitHub helpers, `plan-issue` GitHub I/O, `forge-cli` GitHub backend | Required for GitHub-facing flows | `brew install gh` |
| `glab` | `forge-cli` GitLab backend, including `glab api` for MR checks/wait/merge and inbox reads | Required for GitLab-facing `forge-cli` flows | `brew install glab` |
| `direnv` | `agent-run exec` project environment activation for applicable `.envrc` / `.env` files | Required when a project env file applies and `--direnv` is not `off` | `brew install direnv` |

### 1.1 `image-processing` runtime policy

- `convert --in` and `svg-validate`:
  - Rust-backed (`image` decode/encode for raster inputs, `usvg`/`resvg` for SVG inputs).
  - No external runtime binary requirement.

### 1.2 `api-websocket` transport runtime dependency policy

- `api-websocket` uses an in-process Rust transport (`tungstenite`) via `api-testing-core`.
- No external adapter binary (for example `websocat`) is required for `api-websocket call` or suite websocket cases.

## 2. Runtime Dependencies (Optional / Degradation Paths)

These tools enable richer behavior. Missing tools typically trigger fallback behavior or reduced UX.
Each row is anchored to at least one `Command::new(...)` / `find_in_path` / `cmd_exists` call site
in `crates/*/src`.

| Tool | Behavior Impact | Install (brew/linuxbrew) |
|---|---|---|
| `file` | MIME-based binary detection in `git-scope` and `git-cli commit context` | Usually preinstalled |
| `lsof` | Preferred backend for `fzf-cli port` (fallback: `netstat`); required for `fzf-cli kill-port` | `brew install lsof` |
| `netstat` | Fallback backend for `fzf-cli port` when `lsof` is missing | Usually preinstalled |
| `bat` | Syntax-highlighted previews in `fzf-cli file` / `directory` (invoked via fzf preview shell) | `brew install bat` |
| `vi` | Default editor for `fzf-cli` open / `git-commit` flows (override via `FZF_FILE_OPEN_WITH`) | Usually preinstalled |
| `code` | VS Code open mode for `fzf-cli` (`--vscode`), `git-commit --vscode`, and `open-changed-files` | macOS: `brew install --cask visual-studio-code` |
| `pbcopy` / `wl-copy` / `xclip` / `xsel` | Clipboard integration via `nils-common::clipboard` (used by `git-cli commit context`, `fzf-cli` block preview) | Linux: `brew install wl-clipboard xclip xsel` |
| `cwebp` | WebP encode path for `screen-record` macOS WebP screenshot fallback | `brew install webp` |
| `pactl` | Linux audio source discovery for `screen-record --audio ...` | `brew install pulseaudio` |
| `xdg-desktop-portal` + backend + PipeWire | Wayland portal capture path (`screen-record --portal`) | Prefer distro packages |
| `open` | macOS `open` invocation for `screen-record` permission prompts | Preinstalled on macOS |
| `hs` (Hammerspoon CLI) | Preferred AX backend path for `macos-agent ax *` (fallback to JXA when unavailable) | `brew install --cask hammerspoon` |
| `cliclick` | Probed by `macos-agent` preflight as an alternate input backend | `brew install cliclick` |
| `im-select` | Required by `macos-agent input-source *` and macOS real E2E keyboard/input-source setup | `brew install im-select` |
| `openvpn` | Optional `forge-cli inbox --gitlab-vpn-check openvpn` readiness dependency probe; `forge-cli` never starts or stops VPN | `brew install openvpn` |
| `glab` `mr note create --resolvable` | `forge-cli pr review` on GitLab probes `glab mr note create --help` and picks the most capable note form: with `--resolvable` it posts a non-resolvable status note; with `create` but no `--resolvable` it drops only that flag; with no `create` subcommand it uses the bare `glab mr note <id>` form. Only the first avoids registering on the `pr merge` thread gate | `brew upgrade glab` |
| `docker-compose` | Fallback backend for `docker-tools compose down` when Docker Compose v2 is unavailable | `brew install docker-compose` |

## 3. Development and Validation Toolchain

| Tool | Purpose | Recommended Install |
|---|---|---|
| Rust toolchain (`cargo`, `rustc`, `rustfmt`, `clippy`) | Build/lint/test pipeline | `brew install rustup-init && rustup-init -y && rustup component add rustfmt clippy` |
| `cargo-nextest` | CI-style test execution | `cargo install cargo-nextest --locked` |
| `cargo-llvm-cov` | Coverage workflows | `cargo install cargo-llvm-cov --locked` |
| `zsh` | Required for `tests/zsh/completion.test.zsh` | `brew install zsh` |
| `python3` | `scripts/crates-io-status.sh`, `scripts/publish-crates.sh` | `brew install python` |
| `bash`, `awk`, `sed` | CI helper scripts in `scripts/ci/` | Typically preinstalled |
| `rg` (ripgrep) | Required by docs/CI audit scripts (for example `scripts/ci/docs-hygiene-audit.sh`) | `brew install ripgrep` |
| `bash-completion` | Bash completion loading (optional) | `brew install bash-completion` |
| `gh` | PR/release operations in GitHub-driven workflows | `brew install gh` |

## 4. Repository-Local Script Entry Points

These are repository scripts (not third-party packages):

- Install workspace binaries:
  - `./scripts/install-local-release-binaries.sh`
- Run default local changed-scope checks:
  - `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
- Run full CI parity checks when needed locally:
  - `NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh`
- Supporting utilities:
  - `scripts/generate-third-party-artifacts.sh`
  - `scripts/workspace-bins.sh`
  - `scripts/ci/docs-placement-audit.sh`
  - `scripts/ci/docs-hygiene-audit.sh`
  - `scripts/ci/coverage-summary.sh`
  - `scripts/ci/coverage-badge.sh`

## 4.1 Adapter Opt-Outs

`plan-issue` exposes one runtime opt-out env var consumed at command time.

| Env var | Effect | Intended consumer |
|---|---|---|
| `PLAN_ISSUE_SKIP_INIT_SNAPSHOT` | When set non-empty, `start-plan` and `start-sprint` skip both the existence check on `<AGENT_HOME>/prompts/plan-issue-delivery-{main,subagent}-init.md` and the matching `.snapshot.md` copy into the per-issue / per-sprint workspace. The result payload sets `init_snapshot_skipped: true` for auditability; every other artifact (plan snapshot, dispatch records, prompt manifest, etc.) is unaffected. | Runtime adapters (such as the Claude Code `plan-issue` plugin) that ship their own role/protocol prompts inline with their adapter agents and do not need the canonical init-prompt snapshots in sprint workspaces. |

The codex and opencode adapters must not set this var; they continue to rely on the canonical init prompts. The env var defaults to unset, so the binary's behaviour is unchanged for every other caller.

## 5. `agent-docs` integration for `project-dev`

Register this file as a required `project-dev` document by declaring a
`[[document]]` entry in the project `AGENT_DOCS.toml` catalog:

```toml
[[document]]
context  = "project-dev"
scope    = "project"
path     = "BINARY_DEPENDENCIES.md"
required = true
notes    = "External runtime tools required by the repo"
```

`agent-docs init --print` emits an annotated stub with this schema. Verify
resolution includes this document:

```bash
cargo run -p agent-docs -- preflight --intent project-dev --format json \
  | rg "BINARY_DEPENDENCIES\\.md"
```

## 6. Recommended Install Profiles

### 6.1 Base contributor profile

```bash
brew install git gh glab fzf webp ffmpeg bat zsh python bash-completion rustup-init im-select
```

### 6.2 Linux extra profile (audio/clipboard/network ergonomics)

```bash
brew install lsof wl-clipboard xclip xsel pulseaudio
```

## 7. Linuxbrew Bootstrap (if `brew` is not installed)

```bash
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
```

After installation, initialize Linuxbrew in shell startup (example):

```bash
eval "$(/home/linuxbrew/.linuxbrew/bin/brew shellenv)"
```

## 8. Quick Environment Verification

```bash
for c in git gh glab fzf grpcurl file ffmpeg bat im-select curl; do
  if command -v "$c" >/dev/null 2>&1; then
    echo "[OK]   $c -> $(command -v "$c")"
  else
    echo "[MISS] $c"
  fi
done
```
