# Handoff: Support Codex app-server 0.145.0 in `agent-session`

<!-- implementation-readiness handoff; author: diagnostic session 2026-07-23 -->

**Status:** ready to implement. Root cause area is isolated; the exact reset
trigger is NOT yet pinned and MUST be captured by the reproduction in §5 as the
first step.
**Decision (owner):** support **0.145.0 only** — do a clean protocol cutover, do
NOT keep 0.144.x compatibility. Carry the change all the way to release +
deploy.
**Repo:** `sympoies/nils-cli`, crate `crates/agent-session` (`nils-agent-session`).
Current workspace version `1.25.9` (lockstep across all crates).

---

## 0. Next-session quickstart

1. Read §1–§4 for the full situation.
2. **Do §5 first**: reproduce the failure with codex 0.145.0 and capture the
   real error. Everything downstream depends on which failure locus §5 reveals.
3. Apply the fix in §6 (one file: `crates/agent-session/src/codex_app_server.rs`).
4. Tests §7 → build/validate §8 → local e2e §9 → PR §10 → release §11 →
   deploy + re-upgrade codex + mobile test §12.
5. Honour the constraints in §13 (0.145.0-only ⇒ **m4 must also move to
   0.145.0**).

---

## 1. Context — what broke and why we are here

- **Product:** Agent Console (browser/mobile terminal for tmux-backed Codex /
  Claude sessions on the `sympoies` host). The host runs
  `agent-console-serve.service` = `agent-session serve` (control plane HTTP +
  WebSocket PTY/attach on `127.0.0.1:8781`). Owner runbook lives in
  `serenvia/sympoies-infra/host/agent-console/README.md`.
- **Trigger:** on **2026-07-22 22:05** the codex npm global was upgraded to
  **0.145.0** (its app-server interface was substantially restructured — now has
  `codex app-server {daemon,proxy,generate-ts,generate-json-schema}` subcommands
  and a `codex remote-control {start,stop,pair}` family).
- **Symptom:** opening a Codex session from the mobile/browser console fails.
  The codex app-server child that serve launches **exits right after launch**
  (`journalctl --user -u agent-console-serve.service` shows
  `server exited unexpectedly` and
  `warning: Codex app-server control ended: … WebSocket protocol error:
  Connection reset without closing handshake`). The mobile UI shows
  `remote app server at unix:///run/user/1000/agent-session/cx-<hash>.proxy
  transport failed: WebSocket protocol error: Connection reset without closing
  handshake`.
- **Not affected:** codex **TUI** (Termius / `agent-session start` + `attach`)
  works on 0.145.0 — it does not use the app-server chain. Claude sessions work.
- **Stopgap already in place (do not undo yet):** the `sympoies` **linuxbrew**
  codex was pinned to **0.144.6** (`npm i -g @openai/codex@0.144.6` on the
  linuxbrew node prefix). Mobile Codex is currently working on 0.144.6 (verified
  end-to-end on 2026-07-23). This handoff removes the need for that pin.
- **Goal of this work:** make serve's codex app-server chain speak codex
  **0.145.0**, then re-upgrade codex and drop the pin.

## 2. Environment facts

| Thing | Value |
| --- | --- |
| Host | `sympoies` (systemd user services; NOT m4/launchd) |
| serve unit | `agent-console-serve.service`, `KillMode=process`, `AGENT_SESSION_TMUX_SCOPE=1` (non-destructive restart) |
| serve binary | `/home/linuxbrew/.linuxbrew/bin/agent-session` = nils-cli `1.25.9` |
| codex serve uses | linuxbrew npm global `/home/linuxbrew/.linuxbrew/bin/codex` → **pinned 0.144.6** (stopgap) |
| codex for testing 0.145.0 | fnm install `~/.local/share/fnm/node-versions/*/installation/bin/codex` = **0.145.0** (untouched; use this to reproduce/test) |
| codex install method | npm global (`@openai/codex`); NOT a brew formula. No auto-update cron/timer. `codex update` / `npm i -g` are the only version-change vectors. |
| nils-cli source | `/home/terry/Project/sympoies/nils-cli` |
| infra repo | `/home/terry/Project/serenvia/sympoies-infra` |

Regenerate protocol schemas for any codex version to a dir:
`codex app-server generate-json-schema --experimental --out <DIR>` (v1/v2
JSON-Schema bundle; `--experimental` is required to include experimental
methods). `generate-ts` produces TS bindings the same way.

## 3. The codex app-server chain and where the code lives

Everything is in one file: **`crates/agent-session/src/codex_app_server.rs`**
(~9,295 lines). Protocol messages are **inline `serde_json::json!` literals**
(no vendored crate, no generated types, no JSON schema in-tree). A protocol fix
edits those literals in this one file.

Launch chain — `launch_script()` (approx. `:552-708`), a POSIX script handed to
`tmux … sh -c`:

1. `"$agent" app-server --listen "unix://$socket"` (`:590`) — the codex
   app-server; its stderr is teed to `$startup_diagnostic_pipe`.
2. `"$proxy_bin" --state-dir "$state_dir" codex-app-server-proxy --id
   "$session_id" --upstream "$socket" --listen "$proxy"` (`:657`) —
   agent-session's own WS↔WS bridge (`$proxy_bin` = `current_runtime_helper()`,
   i.e. the agent-session binary; subcommand `codex-app-server-proxy`).
3. `"$agent" -c check_for_update_on_startup=false --remote "unix://$proxy"
   "$@"` (`:688`) — the codex TUI client connecting to the proxy. Startup
   stages are written to `$startup_dir/.startup-stage`; on early exit the script
   records `.startup-failure` (`app-server-start-failed`, `proxy-start-failed`,
   `provider-client-exited`, `runtime-helper-unavailable`, …) — see
   `startup_failure_details` in `lib.rs` (approx. `:911-984`).

Serve side: the reconcile loop spawns `run_control(socket)` per session and, on
exit, logs `warning: Codex app-server control ended: {err}` — `serve.rs`
(approx. `:4772-4798`).

Control WS + handshake — `run_control` (approx. `:2018-2046`):

```text
let stream = connect_socket(&socket).await?;               // UnixStream::connect
let (mut websocket, _) = tokio_tungstenite::client_async("ws://localhost", stream) // :2029
send_json(&mut websocket, initialize_request(request_id)).await?;
receive_response_with_timeout(… CONTROL_RESPONSE_TIMEOUT).map_err(|e| format!("initialize failed: {e}"))?;
send_json(&mut websocket, initialized_notification()).await?;
```

Proxy — `run_proxy_session` (approx. `:3428-3472`): accepts the codex `--remote`
client as a WS server (`accept_async_with_config`) and dials upstream to the
app-server as a WS client (`client_async_with_config("ws://localhost", …)`);
frame/message caps `MAX_PROXY_MESSAGE_BYTES` 16 MiB / `MAX_PROXY_FRAME_BYTES`
4 MiB.

Inline builders (approx. `:1519-1596`) — **current code, note the missing
`"jsonrpc"` envelope field**:

```rust
fn initialize_request(id) -> json!({ "id": id, "method": "initialize", "params": {
    "clientInfo": { "name": "agent-session", "title": "agent-session", "version": env!("CARGO_PKG_VERSION") },
    "capabilities": { "experimentalApi": true, "requestAttestation": false } } })
fn initialized_notification() -> json!({ "method": "initialized" })
fn loaded_threads_request(id) -> json!({ "id": id, "method": "thread/loaded/list", "params": {} })
fn resume_thread_request(id, thread_id, cwd) -> json!({ "id": id, "method": "thread/resume", "params": { "threadId", "cwd" } })
fn rate_limits_request(id) -> json!({ "id": id, "method": "account/rateLimits/read" })
fn external_auth_login_request(...) -> "account/login/start" { "type":"chatgptAuthTokens", "accessToken", "chatgptAccountId", "chatgptPlanType" }
fn external_auth_refresh_response(...) -> { "id", "result": { "accessToken", "chatgptAccountId", "chatgptPlanType" } }
fn continuation_request(id, thread_id, message) -> "turn/start" { "threadId", "input":[{ "type":"text","text","text_elements":[] }] }
```

Inbound handling also references `account/chatgptAuthTokens/refresh`,
`mcpServer/elicitation/request`, `serverRequest/resolved`, `thread/start`
(approx. `:2948-3110`).

Response parsing is **loose** (`serde_json::Value.get(...)`, e.g.
`loaded_thread_ids` does `result.get("data")?.as_array()?`). There are **no
`#[serde(deny_unknown_fields)]` structs** for app-server responses → additive
response fields do NOT break parsing.

Version gate — `codex_app_server.rs:60-61`:

```rust
const MINIMUM_APP_SERVER_VERSION: (u64, u64, u64) = (0, 144, 1);
const AUDITED_EXACT_ATTENTION_VERSIONS: &[(u64,u64,u64)] = &[(0,144,1),(0,144,3)];
```

`PROTOCOL_VERSION = "v2"` (`:39`) is only agent-session's own metadata tag stored
in the session record — it is NOT sent to codex.
Capability probe `app_server_probe` (approx. `:235-271`) runs
`agent-bin app-server --help` and advertises transport only if the help text
contains both `"--listen <URL>"` and `"unix://"` — 0.145.0 still prints both, so
this gate passes.

## 4. Root cause — confirmed vs excluded vs hypotheses

**Confirmed:**

- The break is entirely on the serve→codex-app-server chain; codex TUI is fine.
- codex 0.145.0 `app-server --listen unix://<sock>` **starts fine standalone,
  creates the socket, and completes the WebSocket upgrade** (a raw
  `curl --unix-socket … -H 'Upgrade: websocket' …` returns
  `HTTP/1.1 101 Switching Protocols` + `sec-websocket-accept`). So the transport
  is still a WebSocket and the server does not die merely from listening.
- The app-server child exits only under serve's full session context (the
  standalone listener does not).

**Excluded (ruled out):**

- Version gate — 0.145.0 ≥ `(0,144,1)` passes; and it is correctly handled as
  hook attention authority (0.145.0 ∉ `AUDITED_EXACT_ATTENTION_VERSIONS`, which
  is the intended fallback; there are already tests feeding `codex-cli 0.145.0`).
- `initialize` params schema — **`v1/InitializeParams.json` is byte-identical
  between 0.144.6 and 0.145.0**; it requires only `clientInfo`, does NOT require
  `protocolVersion`. agent-session's `initialize_request` params are valid.
- Transport switch — still WebSocket (see 101 above).
- Runtime dir — `private_runtime_dir()` / `allocate_socket_path()` are
  agent-session-owned and version-independent (socket under
  `$XDG_RUNTIME_DIR/agent-session/cx-<hash>.sock`, ≤100 bytes). Not the cause
  unless 0.145.0 refuses a caller-supplied `unix://` path (it does not — proven
  by the standalone listen test).
- Message-shape drift on thread/turn/account methods — the schema deltas are
  **additive/optional** (see appendix) and parsing is loose, so they do not
  cause a connect-time reset.

**Top hypotheses to confirm in §5 (ordered by likelihood):**

1. **JSON-RPC envelope tightening.** agent-session's request builders omit the
   `"jsonrpc": "2.0"` field (see §3). If codex 0.145.0's app-server now enforces
   JSON-RPC 2.0 envelope validation and hangs up on a request without
   `jsonrpc:"2.0"`, the socket closes with no WS close frame → exactly
   "Connection reset without closing handshake". **Small fix** (add the field to
   every builder). Highest-probability + cheapest — check first.
2. **`codex --remote` client ↔ agent-session proxy negotiation.** The user-facing
   error is emitted by the codex 0.145.0 `--remote` client talking to
   `unix://…proxy` (agent-session's `codex-app-server-proxy`). 0.145.0's client
   may now require a WS subprotocol, an auth token (`--remote-auth-token-env`
   exists), or a connection-level handshake the old proxy does not answer →
   proxy or client resets. Fix would live in `run_proxy_session` / the
   `--remote` invocation flags.
3. **A required `initialized`/capability step or a first server→client request**
   (e.g. attestation) that 0.145.0 now expects and old code answers differently.

§5 must capture the actual stderr/close reason to decide which of these it is.
Do not start editing before §5 identifies the locus.

## 5. STEP 1 — reproduce and pin the exact error (do this first)

Use codex **0.145.0** (the fnm binary) without disturbing production serve
(which stays on 0.144.6). Two approaches; prefer (A).

**(A) Throwaway serve — faithful, full session context.**

- Start a second serve on a free loopback port with a scratch state dir and the
  0.145.0 codex first on PATH and a self-chosen token:

  ```sh
  FNM_DIR=$(dirname "$(ls ~/.local/share/fnm/node-versions/*/installation/bin/codex | head -1)")
  SD=$(mktemp -d); TOK=testtoken
  PATH="$FNM_DIR:$PATH" AGENT_SESSION_TOKEN=$TOK \
    agent-session --state-dir "$SD" serve --bind 127.0.0.1:8799 --machine testrepro &
  ```

- Create + attach a codex session the way the browser does (POST create, then WS
  attach) against `127.0.0.1:8799` with `Authorization: Bearer testtoken`.
  Inspect serve's HTTP surface first (`GET /` routes are token-free on loopback);
  the create/attach contract is documented in
  `crates/agent-session/docs/specs/serve-api-v1.md`.
- When the codex session fails, read the per-session diagnostics that
  `launch_script` writes:
  `"$SD/sessions/<id>/.startup-diagnostic.log"` (real codex app-server stderr),
  `.startup-failure` (the failure code), `.runtime-exit-status`,
  `.startup-stage`. These are the ground-truth error. (Note: the mobile app
  deletes failed sessions, so post-mortem files from the original incident are
  gone — you must re-capture.)

**(B) Manual chain — quick, but needs a real session record.**
Running the three commands from §3 by hand reproduces most of it. Caveat learned
during diagnosis: `agent-session codex-app-server-proxy --id <x>` fails with
`session load failed: session-not-found` unless `<x>` is a real session id in
the given `--state-dir`, and `codex --remote` needs a PTY (`stdin is not a
terminal` otherwise). So create a real session first (or drive via (A)).

**Cheapest first probe for hypothesis #1:** open the app-server WS yourself and
send `initialize` **without** then **with** `"jsonrpc":"2.0"`, and watch whether
the 0.145.0 app-server stays up or resets. A tiny client is easiest in Rust
(a throwaway `#[tokio::test]` or example using `tokio-tungstenite` +
`UnixStream`, mirroring `run_control`), since `websocat`/`wscat` are not
installed and `curl` cannot send post-upgrade frames. Compare against 0.144.6 to
confirm the behavioural difference.

Deliverable of §5: a one-line statement of the exact reset cause + which
builder/flag/handshake must change.

## 6. The fix (0.145.0-only cutover)

Single file: `crates/agent-session/src/codex_app_server.rs`.

- Apply whatever §5 pins. If it is hypothesis #1, add `"jsonrpc": "2.0"` to every
  request/response builder (`initialize_request`, `initialized_notification`
  [a notification: `{"jsonrpc":"2.0","method":"initialized"}`],
  `loaded_threads_request`, `resume_thread_request`, `rate_limits_request`,
  `external_auth_login_request`, `external_auth_refresh_response`,
  `continuation_request`, and the inbound response builders near `:2948-3110`).
- Bump the version floor: `MINIMUM_APP_SERVER_VERSION = (0, 145, 0)` (`:60`).
  This is the explicit 0.145.0-only cutover — older codex is then rejected
  cleanly with `codex-version-too-old` instead of silently breaking.
- Only touch `app_server_probe` / the `--help` substrings (`:235-271`) if §5
  shows the 0.145.0 help wording changed (it did not in observation — still
  prints `--listen <URL>` and `unix://`).
- Message shapes (thread/turn/account) are additive + parsed loosely; touch them
  only if §5 shows a specific required-field change actually breaks a call.

Keep the change minimal and scoped to what §5 proved. **If §5 reveals the break
is a large multi-method protocol change (not a small envelope/handshake fix),
STOP and report to the owner before a large rewrite** (owner asked to be
consulted if scope balloons).

## 7. Tests

Follow the repo's test-first-evidence norm: add/adjust a failing test that
encodes the 0.145.0 expectation before the production edit.

- In-file `#[cfg(test)]` suite in `codex_app_server.rs` (approx. `:4280-9290`),
  including live proxy round-trip tests — extend these to assert the new
  envelope/handshake and the `(0,145,0)` floor.
- `crates/agent-session/tests/integration/cli.rs` — version-gate fixtures feed
  `codex-cli 0.145.0` (approx. `:2282-2284`); update expectations for the new
  minimum (and add a rejected-`0.144.x` case).
- `crates/agent-session/src/serve.rs` tests around the control loop.

## 8. Build & validation (finish-line gate)

```sh
cargo build
bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast     # finish-line gate; writes .cache/agent-validation/project-dev.ok
# full parity when needed:
NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh
```

`AGENT_DOCS.toml` (`[[validation]]`) is the authority; the `--local-fast` gate
must pass (or an explicit waiver) before declaring the project-dev task done.
Workspace version lockstep is CI-enforced (`scripts/ci/workspace-version-lockstep.sh --strict`).

## 9. Local end-to-end verification (with codex 0.145.0)

```sh
scripts/install-local-release-binaries.sh --bin agent-session   # installs to ~/.local/nils-cli/bin
```

Then repeat the §5(A) throwaway serve using the freshly built agent-session with
codex 0.145.0 on PATH; confirm the codex session's app-server chain stays
connected and a `turn/start` round-trips (no `server exited unexpectedly`, no WS
reset in the diagnostics).

## 10. Delivery — PR (cross-repo mechanics)

Work happens in `sympoies/nils-cli`, which is not the usual cwd. Use the managed
tooling (direct `git commit` / `git worktree` / `gh pr create` are hook-blocked):

- `git-cli worktree` to create a managed non-default branch.
- `semantic-commit` (pass a literal `--repo` for the nils-cli path when cwd is
  elsewhere).
- `forge-cli` to open the PR. `main` is unprotected; `git push -u` before
  `forge-cli` where needed, and watch checks with
  `gh pr checks --watch` — required green checks: `test`, `test_macos`,
  `coverage`.
- Repo policy: keep content English; PR body per `forge-cli` format; do not add
  a Claude co-author trailer.

## 11. Release (owner-gated)

Use the **`private-release-nils-cli`** skill (routes to the canonical
sympoies-infra release-and-deploy; preserves the two-stage consent boundary). It
bumps the workspace version from `1.25.9`, tags `v*` (which triggers
`.github/workflows/release.yml`, requiring green `test` / `test_macos` /
`coverage`), and publishes. **Confirm with the owner before releasing.**

## 12. Deploy to sympoies + re-upgrade codex + mobile test

1. Deploy the new agent-session to the host serve. Either the trusted
   `deploy-agent-console.yml` (infra deploy runs the installer first), or
   manually in `serenvia/sympoies-infra`:
   `git pull --ff-only && scripts/install-agent-console.sh --render` — this
   restarts `agent-console-serve.service` via the non-destructive
   `KillMode=process` path (live tmux panes survive). **Confirm with the owner
   before deploying.**
2. Drop the stopgap: re-upgrade the linuxbrew codex to 0.145.0:
   `/home/linuxbrew/.linuxbrew/bin/npm i -g @openai/codex@0.145.0`.
3. Owner opens a **new** Codex session in mobile Agent Console → must reach the
   `>_ OpenAI Codex` prompt and accept input; verify serve journal has no
   `server exited unexpectedly` / app-server reset.
4. Update `serenvia/sympoies-infra` runbook to remove any temporary
   "pin codex ≤0.144.6" note.

## 13. Decisions & constraints

- **0.145.0-only** (owner decision): clean cutover, no 0.144.x compat. Hence the
  `MINIMUM_APP_SERVER_VERSION` bump to `(0,145,0)`.
- **m4 spoke must also move to 0.145.0.** m4 (macOS launchd + `tailscale serve`
  spoke, see infra runbook) currently runs codex 0.144.6. Once agent-session is
  0.145.0-only, m4's codex must be upgraded to 0.145.0 (and m4's agent-session
  updated to the released build) or its Codex sessions will break in the
  opposite direction. Sequence the m4 upgrade with the deploy.
- **Full scope to production** (owner decision): implement → test → PR → release
  → deploy → mobile verify, with owner confirmation before release and before
  deploy.

## 14. Appendix — evidence captured 2026-07-23

- `InitializeParams` v1 schema: identical 0.144.6 ↔ 0.145.0 (only `clientInfo`
  required; no `protocolVersion`).
- Schema deltas on methods agent-session uses are **additive/optional**, e.g.:
  - `ThreadStartParams` / `TurnStartParams`: + `runtimeWorkspaceRoots`
    (nullable, "Omitted defaults to `cwd`").
  - `ThreadStart/TurnStart` response content: + `inputAudio`/`audio` content-item
    variants; + web-search `results` (default null); + `canAcceptDirectInput`
    (nullable); **removed** nullable `templateId`.
  - `ThreadResumeResponse`: + `itemsBackwardsCursor` (opaque pagination cursor).
  - New v2 methods added in 0.145.0 (not used by agent-session): Apps*,
    Environment*, `ThreadSearchOccurrences*`, `RawResponseCompletedNotification`.
- Standalone `codex app-server --listen unix://<sock>` on 0.145.0: socket
  created, process stays up, WS upgrade returns HTTP 101.
- Diagnostic-file paths written by `launch_script` (for §5):
  `$STATE/sessions/<id>/{.startup-stage,.startup-failure,.startup-diagnostic.log,.runtime-exit-status,.provider-stderr.pipe}`.

_All line numbers are approximate anchors against `agent-session` 1.25.9 —
re-grep before editing (search for `initialize_request`,
`MINIMUM_APP_SERVER_VERSION`, `launch_script`, `run_control`,
`run_proxy_session`)._
