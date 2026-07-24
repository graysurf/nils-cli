# Codex CLI `agent run` MCP Policy Implementation Handoff

- **Status:** Decided; implementation-ready; no unresolved design questions
- **Date:** 2026-07-25
- **Source:** Operator failure report, local Codex 0.145.0 capability probes,
  the unreleased Execution Capsule prototype, and the current `nils-cli`
  isolated-agent runtime
- **Ownership:** `nils-codex-cli`
- **Implementation tier:** L2
- **Intended next step:** Recover or rebase the Execution Capsule prototype
  onto current `nils-cli` main, then implement this MCP policy before release
  and runtime-kit adoption
- **Current delivery constraint:** This handoff is committed to a local branch.
  GitHub issue and PR creation are intentionally deferred while the provider
  path is unavailable.
- **Retention:** Transient implementation source. Remove it after the released
  behavior is represented by the owning Execution Capsule specification,
  crate runbook, and acceptance tests.

## Purpose

Make `codex-cli agent run` deterministic and usable when the operator's normal
Codex home contains unavailable, expired, or intermittently failing MCP
servers.

An Execution Capsule supervisor normally needs repository instructions,
lifecycle hooks, command rules, authentication, shell tooling, and the
capsule's exact helper commands. It does not normally need arbitrary external
MCP tools. Loading the full inherited MCP catalog adds startup latency,
credential refreshes, external authority, and failure modes unrelated to the
prepared operation.

The target behavior is:

1. MCP is disabled by default for `agent run`.
2. Home and project instructions, lifecycle hooks, rules, Git hooks, signing,
   sandboxing, and capsule integrity checks remain active.
3. An operator can explicitly select the existing fully inherited Codex runtime
   when MCP or another full-home capability is genuinely required.
4. The runner reports startup progress and fails clearly instead of appearing
   hung before the first Codex JSONL event.

This is not a proposal to change ordinary `codex`, the interactive Codex app,
or the existing isolated behavior of `agent prompt`, `agent advice`,
`agent knowledge`, or `agent commit`.

## Evidence Index

- **[U1]** The operator ran a host Execution Capsule through
  `codex-cli agent run`; Codex logged an `atlassian-rovo` OAuth refresh failure
  with `unauthorized_client` and produced no visible runner progress after the
  initial stdin message.
- **[U2]** The operator decided that `agent run` normally should not require
  MCP, while preserving an explicit path for capsules that do.
- **[F1]** `crates/codex-cli/src/runtime/isolated.rs` implements a private
  temporary `CODEX_HOME`, file-backed auth symlink, capability probes,
  control-environment removal, and cleanup checks for existing one-shot agent
  commands.
- **[F2]** `crates/codex-cli/src/runtime/agent_mode.rs` defines the current
  `isolated|inherited` runtime choice for prompt-like commands; `agent run`
  must not reuse the current isolated mode unchanged because that mode removes
  instructions and hooks.
- **[F3]** The unreleased Execution Capsule prototype adds
  `crates/codex-cli/src/agent/capsule.rs`,
  `crates/codex-cli/docs/specs/execution-capsule-v1.md`, receipt/error schemas,
  CLI wiring, and integration tests. Its supervisor launches `codex` with the
  real inherited `CODEX_HOME`, pipes JSONL stdout, inherits stderr, and has no
  MCP policy or first-event timeout.
- **[F4]** The prototype's Execution Capsule v1 contract intentionally
  preserves active instructions, configuration, hooks, rules, signing, and
  sandbox behavior. The MCP change must refine that boundary rather than
  silently replace it with the existing fully isolated prompt runtime.
- **[A1]** Local Codex 0.145.0 exposes `-c/--config`, `--profile`,
  `--ignore-user-config`, `--ignore-rules`, and feature disables, but no
  dedicated `--no-mcp` or MCP allowlist flag on `codex exec`.
- **[A2]** A local `-c 'mcp_servers={}'` probe did not clear inherited MCP
  entries. A direct server can be disabled with
  `mcp_servers.<name>.enabled=false`, while a plugin-provided server requires
  the separate plugin configuration namespace. Enumerating only
  `mcp_servers.*` therefore cannot guarantee a no-MCP runtime.
- **[A3]** The current `nils-cli` main at `6af1224` does not yet contain the
  Execution Capsule prototype. The prototype was inspected from the private
  runtime-kit acceptance source snapshot based on workspace version `1.25.9`;
  it is evidence, not an authoritative Git commit.
- **[W1]** The official Codex configuration documentation defines per-server
  `enabled`, `required`, startup timeout, profiles, one-off dotted overrides,
  project-scoped MCP configuration, and plugin-provided MCP configuration:
  <https://learn.chatgpt.com/docs/config-file/config-reference>.
- **[W2]** The official Codex MCP documentation describes direct
  `[mcp_servers.<name>]` entries and plugin-provided server control under
  `[plugins."<plugin-id>".mcp_servers.<name>]`:
  <https://learn.chatgpt.com/docs/extend/mcp>.

## Confirmed Facts

### Current repository baseline

- Current `nils-cli` main already owns the reusable low-level primitives for a
  temporary Codex home and auth bridge. [F1]
- Current `nils-cli` main does not own `agent run`, `capsule-exec`, the
  Execution Capsule specification, or its schemas. Those must be recovered or
  rebased before this policy can be implemented against authoritative source.
  [A3]
- The prototype runs Codex as the AI supervisor; the parent wrapper validates
  the capsule, generates private helpers, captures evidence, validates the
  structured final report, and publishes a receipt. [F3]
- The prototype does not pass the dangerous approval/sandbox bypass, does not
  ignore rules, and intentionally uses `workspace-write` or
  `danger-full-access` according to the capsule access class. [F3][F4]

### Current Codex MCP behavior

- Direct MCP servers can be configured globally or in a trusted project's
  `.codex/config.toml`. [W1][W2]
- Plugins can contribute MCP servers independently of direct
  `[mcp_servers.*]` entries. Their control namespace includes both plugin ID
  and MCP server name. [W2]
- `required = true` makes an enabled server startup failure fatal. The default
  non-required behavior does not guarantee that OAuth refresh, initialization,
  or startup delay is absent. [W1]
- `--ignore-user-config` removes the entire user `config.toml`, not just MCP.
  It is too broad when used alone because the Execution Capsule contract must
  retain the governance subset of the active runtime. [W1][F4]
- A profile overlays the base user configuration; it does not erase the base
  MCP tables. [W1][A2]
- An empty table supplied as a CLI overlay does not reliably replace merged
  MCP configuration. [A2]

### Failure characteristics

- The observed failure came from an external OAuth credential refresh that was
  unrelated to the prepared script. [U1]
- The current prototype inherits Codex stderr and waits while a bounded stdout
  reader drains JSONL. It does not display a supervisor phase, a first-event
  deadline, or a heartbeat. A slow startup and a dead startup are therefore
  indistinguishable to the operator. [F3]
- Automatically parsing an MCP name from stderr and retrying with changed
  configuration would silently alter the tool surface and would depend on
  unstable prose. That is not an acceptable machine contract. [I1]

## Decisions

### D1. Add an `agent run`-specific MCP mode

Add:

```text
--mcp-mode disabled|inherited
```

The default is `disabled`.

`inherited` is accepted only as an explicit CLI flag on the current
invocation. Do not add an environment variable or config default that can
silently broaden the supervisor's external tool surface.

The initial implementation intentionally has two modes:

| Mode | Codex home | MCP behavior | Intended use |
| --- | --- | --- | --- |
| `disabled` | Private supervisor projection | No user/plugin/app MCP | Normal capsule execution |
| `inherited` | Real active Codex home | Existing full-home behavior | Explicit compatibility escape hatch |

Do not add an allowlist mode in this implementation. A safe allowlist would
need either a first-class Codex capability or a new capsule schema that can
declare and validate direct and plugin server identities without copying or
logging credential material. That belongs in a later, separately reviewed
change.

### D2. Keep Execution Capsule v1 unchanged

`--mcp-mode` is a supervisor launch policy, not authority embedded in
`run.sh`. Keep `execution-capsule.v1` and its manifest fields unchanged.

Reasons:

- The v1 parser rejects unknown fields.
- Adding a manifest field would change the portable capsule schema and require
  an unnecessary v2 before a safe allowlist exists.
- The direct `bash <capsule>/run.sh` route never uses MCP.
- The effective mode can be recorded additively in the supervised receipt.

If a future implementation introduces a server allowlist or makes MCP part of
the capsule's declared authority, that change must define
`execution-capsule.v2` rather than extending v1 ambiguously.

### D3. Create a governance-preserving supervisor runtime

Do not run the existing `isolated` prompt runtime unchanged. It deliberately
removes home/project instructions, hooks, plugins, and rules, which conflicts
with the Execution Capsule contract. [F1][F2][F4]

Create a separate internal runtime, named `CapsuleSupervisorHome` in this
handoff, with these properties:

1. Resolve and refresh active Codex authentication using the same source-home
   and remote-auth order as `runtime::isolated`.
2. Create a unique private temporary `CODEX_HOME` with mode `0700`.
3. Bridge file-backed authentication with an `auth.json` symlink. Never copy
   credential bytes.
4. Preserve the active home `AGENTS.md` by copying bounded instruction bytes
   into an owner-only child-home file when present. Do not symlink a mutable
   governance source into a host-access supervisor.
5. Preserve provider hook ingress:
   - project only the top-level `hooks` table and `features.hooks` setting from
     the real `config.toml` into a private generated `config.toml`;
   - copy a bounded, validated `hooks.json` into the private home when present;
   - retain hook trust state needed by the projected hook table;
   - never serialize projected hook configuration into logs or receipts.
6. Keep the real shell `HOME`, `PATH`, Git, SSH, GPG, and signing environment.
7. Preserve project `AGENTS.md`, Git hooks, repository rules, and the
   capsule's existing sandbox and helper-attestation boundary.
8. Run ephemerally so the supervisor does not leave resumable session state.
9. Remove child-control environment variables using the existing isolated
   runtime helper, except for values explicitly required by the capsule
   supervisor.
10. Clean the temporary home after Codex exits and retain the existing
    replacement warning if the child substitutes the auth symlink.

The generated config is a governance projection, not a filtered copy of the
entire user config. It must not include:

- `mcp_servers`;
- plugin, marketplace, app, connector, skill, memory, goal, or subagent
  registrations;
- notification integrations;
- static HTTP headers or other MCP credentials;
- unrelated user-facing UI configuration.

Refactor the reusable private-home, auth-bridge, permissions, cleanup, and
control-environment code out of `runtime/isolated.rs` rather than maintaining
two security-sensitive implementations.

The projected governance contract covers provider-level hooks registered
directly in `config.toml` or `hooks.json`. Plugin-bundled hooks are not loaded
in `disabled` mode because loading their owning plugin can also reintroduce an
MCP server. A capsule that intentionally depends on a plugin-bundled hook must
use the explicit `inherited` mode.

### D4. Disable plugin and app capability loading explicitly

The `disabled` launch must use Codex capability probes and explicit feature
disables for at least:

```text
plugins
remote_plugin
apps
workspace_dependencies
```

Keep hooks enabled.

The implementation must fail closed before an API request when the installed
Codex cannot enforce the required supervisor feature set. Do not silently fall
back to the inherited runtime.

Direct user MCP is absent because the projected `config.toml` contains no
`mcp_servers` table. Plugin/app MCP is absent because plugin/app discovery is
disabled and no plugin/app registrations are projected.

For project-local `.codex/config.toml`, perform a preflight before starting
Codex:

- discover applicable project config paths using the same repository/cwd
  boundary accepted by the runner;
- parse them as TOML;
- reject `disabled` mode with a stable preflight error if a project config
  declares `mcp_servers`, plugin MCP controls, plugin loading, or app
  connectors that the supervisor cannot prove disabled;
- never log the rejected table values.

The first implementation may become more permissive only after a deterministic
probe proves that the explicit CLI feature disables and config projection
prevent every applicable project MCP from starting. Until that proof exists,
fail closed.

### D5. Preserve exact inherited compatibility

`--mcp-mode inherited` uses the real `CODEX_HOME` and the prototype's current
Codex launch shape.

It must:

- preserve active user/project config, MCP, plugins, apps, skills,
  instructions, hooks, and rules;
- preserve existing host-access acknowledgement;
- print a concise stderr notice before launch:
  `codex-cli agent run: MCP mode inherited; external tools may initialize`;
- record `mcp_mode: "inherited"` in the receipt;
- never retry automatically with a different mode;
- never suppress OAuth or MCP startup errors.

This path is an explicit operator choice, not a fallback from a failed
`disabled` preflight.

### D6. Add startup observability and a first-event deadline

The parent must keep JSON stdout machine-clean and write progress only to
stderr.

Before spawning Codex, text and JSON modes both emit a stderr phase line:

```text
codex-cli agent run: starting supervisor (mcp=disabled)
```

Refactor stdout capture so the parent can observe the first complete JSONL
event while continuing to retain the bounded evidence stream:

1. Drain Codex stdout on a dedicated bounded reader.
2. Signal the parent when the first newline-terminated event is received.
3. Poll child status without blocking the progress loop.
4. If no first event arrives within a fixed internal deadline, terminate the
   Codex child process group, reap it, and return
   `codex-supervisor-startup-timeout`.
5. After the first event, emit a low-frequency stderr heartbeat only when no
   other operator-visible progress has occurred.
6. Do not impose a total model-execution timeout in this change.

Use a 60-second first-event deadline initially. Keep it an internal constant,
covered by deterministic tests with a test-only override. Do not add another
public flag until real evidence shows operators need to tune it.

The timeout error is retryable only after the operator changes runtime
conditions or explicitly selects `--mcp-mode inherited`. It must include
typed, bounded recovery guidance and no captured stderr dump.

### D7. Do not infer policy from MCP stderr

Do not:

- scrape server names from OAuth error prose;
- enumerate `codex mcp list` and synthesize one override per visible server;
- use `mcp_servers={}` as a no-MCP guarantee;
- inject a PATH shim around `codex`;
- mutate the real `config.toml`;
- run `codex mcp logout`, `remove`, or `login`;
- retry automatically after disabling a failing server.

These approaches are incomplete for plugin/project configuration, create
time-of-check/time-of-use gaps, or mutate state outside the capsule contract.
[A2][W2]

### D8. Extend receipts additively

Add the following required fields to the receipt result and checked v1 JSON
schema:

```json
{
  "mcp_mode": "disabled",
  "supervisor_runtime": "governance-projected"
}
```

Allowed values:

- `mcp_mode`: `disabled|inherited`
- `supervisor_runtime`: `governance-projected|inherited`

The receipt must not include:

- configured MCP server names;
- MCP URLs;
- OAuth status or errors;
- config paths outside the already documented capsule artifacts;
- hook command bodies;
- raw child stderr.

Preflight failures that happen before receipt creation continue to use the
Execution Capsule error envelope. Post-preflight startup timeout and Codex
failure continue to produce a detailed receipt when the existing receipt
contract can do so safely.

## CLI Contract

Expected help:

```text
--mcp-mode <mode>
    MCP policy for the Codex supervisor
    [default: disabled]
    [possible values: disabled, inherited]
```

Examples:

```sh
# Normal deterministic supervisor
codex-cli agent run --capsule /private/capsule

# Explicit full-home compatibility when an MCP-backed diagnosis is required
codex-cli agent run \
  --capsule /private/capsule \
  --mcp-mode inherited

# Host access remains a separate acknowledgement
codex-cli agent run \
  --capsule /private/capsule \
  --allow-host-access \
  --mcp-mode inherited
```

An invalid mode is a clap usage error with exit `64`.

Do not add a short flag. Regenerate zsh and bash completion assets with the new
enum candidates.

## Error Contract

New or touched failures must use the repository's automation-facing error
contract: stable code, deterministic exit, typed retryability, next action,
bounded recovery, and secret-free text/JSON rendering.

| Code | Phase | Exit | Retryable | Required next action |
| --- | --- | --- | --- | --- |
| `capsule-supervisor-unsupported` | Preflight | `65` | No | Upgrade Codex or explicitly choose inherited mode |
| `capsule-supervisor-home-failed` | Preflight | `65` | Usually | Repair local runtime/temp permissions |
| `capsule-supervisor-config-invalid` | Preflight | `65` | No | Repair malformed hook/project configuration |
| `capsule-project-mcp-undeclared` | Preflight | `65` | No | Remove project MCP for this run or explicitly choose inherited mode |
| `codex-supervisor-startup-timeout` | Runtime | `1` | Conditional | Inspect runtime health; retry or explicitly choose inherited mode |

The exact existing Execution Capsule envelope names and recovery object shape
remain authoritative when the prototype is recovered. If those names differ
after rebasing, preserve their current released/prototype contract and map
these semantics onto it rather than creating a second error family.

## Scope

### In scope

- Recovering/rebasing the unreleased Execution Capsule prototype onto current
  main as the prerequisite implementation baseline.
- `--mcp-mode disabled|inherited` on `agent run`.
- Default governance-projected supervisor home.
- Explicit inherited compatibility mode.
- Shared private-home/auth bridge refactor.
- Hook and instruction preservation probes.
- Project config fail-closed checks.
- Plugin/app capability disables.
- First-event progress, heartbeat, timeout, child cleanup, and evidence
  preservation.
- Additive receipt/schema fields.
- Text/JSON error contracts.
- CLI help, generated completions, crate docs, and deterministic tests.
- Runtime-kit adoption and executable acceptance after a released version
  exists.

### Non-scope

- Per-server MCP allowlists.
- Manifest schema v2.
- Automatically repairing, logging in, logging out, or removing MCP servers.
- Changing ordinary Codex config or the operator's stored OAuth state.
- Making `agent prompt`, `advice`, `knowledge`, or `commit` load governance
  hooks.
- Changing `agent resume`.
- Removing the explicit inherited compatibility path.
- Changing capsule access classes, helper attestations, allowed paths, Git
  preconditions, validation commands, or evidence trust.
- Total model execution timeout.
- GitHub issue, PR, or release delivery in the documentation-only handoff.

## Implementation Boundaries

### Baseline recovery

Before MCP production edits:

1. Reconstruct the Execution Capsule prototype against current main.
2. Preserve its manifest v1 parser, private file/inode checks, helper
   attestations, stdout evidence capture, final report validation, and receipt
   schemas.
3. Run its existing integration tests unchanged and obtain a green baseline.
4. Treat the private acceptance source snapshot as comparison evidence only.
   Do not copy build artifacts, absolute paths, or private runtime state into
   the repository.

If prototype recovery changes the capsule contract, update the owning spec
first and re-evaluate this handoff before MCP implementation.

### Suggested source ownership

The eventual implementation is expected to touch:

- `crates/codex-cli/src/cli.rs`
  - add `CapsuleMcpMode`;
  - add `--mcp-mode` to `AgentCommand::Run`.
- `crates/codex-cli/src/main.rs`
  - pass the selected mode into capsule `RunOptions`.
- `crates/codex-cli/src/agent/capsule.rs`
  - select the supervisor runtime;
  - manage first-event progress/timeout;
  - record mode/runtime in the receipt;
  - preserve the existing helper/evidence lifecycle.
- `crates/codex-cli/src/runtime/isolated.rs`
  - move reusable private-home/auth/control-environment primitives out.
- `crates/codex-cli/src/runtime/supervisor.rs` or an equivalent focused module
  - build and validate the governance projection;
  - probe required Codex capabilities;
  - validate applicable project config.
- `crates/codex-cli/src/runtime/child_home.rs` or an equivalent private module
  - own secure temp directory, auth symlink, permissions, and cleanup shared
    by isolated and supervisor modes.
- `crates/codex-cli/tests/integration/execution_capsule.rs`
  - add mode, projection, timeout, receipt, and leakage tests.
- `crates/codex-cli/tests/integration/agent_isolation.rs`
  - prove the shared refactor does not change existing isolated behavior.
- `crates/codex-cli/docs/specs/execution-capsule-v1.md`
  - document launch policy and additive receipt fields.
- `crates/codex-cli/docs/specs/execution-capsule-receipt-v1.schema.json`
  - require and constrain the additive fields.
- `crates/codex-cli/docs/runbooks/json-consumers.md`
  - document consumer branching on the new fields/error codes.
- `crates/codex-cli/README.md` and `crates/codex-cli/docs/README.md`
  - update user routing.
- `completions/zsh/_codex-cli` and `completions/bash/codex-cli`
  - regenerate enum completions.

Do not put config projection or security-sensitive child-home logic in
`main.rs` or shell completion adapters.

### Configuration projection rules

Use a generic TOML parser and construct a fresh output value. Do not perform
text slicing on `config.toml`.

The projection function must:

1. reject malformed input without launching Codex;
2. copy only the documented hook feature/table;
3. preserve no unknown top-level table by default;
4. write owner-only bytes atomically into the private child home;
5. avoid rendering config contents in `Debug`, error, or receipt output;
6. test hostile keys whose names resemble allowed keys;
7. test static secret sentinel values under excluded MCP/app/plugin tables and
   prove they do not reach the child config or output.

Adding `toml = { workspace = true }` to `nils-codex-cli` is acceptable if no
existing shared helper owns this parsing contract. Do not add `toml_edit`
unless comment-preserving edits become necessary; this implementation writes
a new machine-owned file.

### Process lifecycle rules

The first-event deadline must not compromise the current evidence boundary.

- Keep stdout bounded by the prototype's maximum event size.
- Never read stdout only after `wait`, which can deadlock on a full pipe.
- Treat partial final JSONL lines as invalid evidence according to the
  existing parser.
- Terminate and reap the process group on timeout.
- Keep stderr inherited or safely streamed to stderr; never mix it into JSON
  stdout or the receipt.
- Preserve final-capture file ownership and publication behavior.
- Do not publish a success receipt after timeout, reader failure, helper
  mismatch, or cleanup uncertainty.

## Test-First Contract

Write meaningful red tests before production edits.

### CLI and default-mode tests

- `agent run --help` exposes only `disabled|inherited`.
- Omitted `--mcp-mode` selects `disabled`.
- Invalid mode exits `64`.
- Generated zsh/bash completions include both enum values.
- No environment variable can select inherited mode.

### Governance projection tests

Use a fake real Codex home containing:

- `AGENTS.md` sentinel;
- file-backed `auth.json`;
- lifecycle-hook sentinel;
- direct MCP command sentinel;
- plugin-provided MCP command sentinel;
- app/connector sentinel;
- secret values under excluded tables;
- unrelated UI and notification config.

Assert in `disabled` mode:

- the child `CODEX_HOME` is unique and private;
- auth is a symlink, not a copy;
- home and project instruction sentinels are visible to
  `codex debug prompt-input`;
- the lifecycle hook sentinel executes;
- direct, plugin, and app MCP sentinels do not execute;
- excluded secrets and config values do not appear in child argv, stdout,
  stderr, receipt, or retained artifacts;
- the temporary home is removed after completion;
- replacing the auth symlink produces the established warning.

Assert in `inherited` mode:

- the child receives the real `CODEX_HOME`;
- the prototype argv remains otherwise unchanged;
- MCP/plugin/app sentinels are available;
- the stderr warning and receipt fields report inherited mode.

### Project-config tests

- no `.codex/config.toml` passes;
- a safe project config passes;
- malformed TOML fails before Codex starts;
- direct `mcp_servers` fails closed without echoing values;
- plugin MCP controls fail closed;
- app/connector config fails closed;
- nested or similarly named benign keys do not produce false positives;
- symlink/path-swap attempts cannot change the checked project config after
  preflight without detection.

### Startup and process tests

- first JSONL event before deadline proceeds normally;
- no event triggers the stable timeout;
- a child that ignores the first termination signal is forcefully reaped
  according to the existing process helper contract;
- stderr remains visible but absent from JSON stdout/receipt;
- a slow post-first-event model turn does not trigger the startup timeout;
- event-reader overflow and invalid JSON retain existing failure behavior;
- timeout can never produce `ok: true`.

### Capsule regression tests

Preserve all existing prototype coverage for:

- private capsule permissions and owner checks;
- symlink/hardlink/path-swap resistance;
- manifest and entrypoint digest validation;
- workspace versus host acknowledgement;
- exact helper command/event matching;
- nonce-bound attestations;
- repeated script and validation event selection;
- final report schema validation;
- receipt recovery publication;
- direct script path independence;
- Git preconditions and concurrent state changes.

## Acceptance Criteria

1. A default `agent run` does not initialize any configured direct, plugin, or
   app MCP server.
2. A default run does not attempt MCP OAuth refresh.
3. A default run still loads active home and project instructions.
4. A default run still executes configured lifecycle hooks and repository Git
   hooks.
5. A default run preserves command rules, signing, sandbox, capsule access,
   helper attestation, and receipt integrity.
6. `--mcp-mode inherited` restores the prototype's full-home behavior only
   when explicitly supplied.
7. The runner never falls back from `disabled` to `inherited`.
8. Applicable project MCP/plugin/app configuration fails closed when no-MCP
   enforcement cannot be proven.
9. The first supervisor phase is visible on stderr, and a missing first JSONL
   event terminates with a stable bounded error instead of hanging.
10. JSON stdout remains a single documented envelope and contains no progress
    prose.
11. Receipt v1 records the effective MCP mode and supervisor runtime with
    additive checked fields.
12. No server name, URL, auth status, token, static header, hook command, or
    raw child stderr is added to the receipt.
13. Existing isolated prompt runtime behavior remains unchanged.
14. Existing Execution Capsule prototype tests remain green after baseline
    recovery.
15. CLI help, zsh completion, bash completion, README, spec, schemas, and JSON
    consumer runbook agree.
16. Runtime-kit adopts only a released nils-cli version whose deterministic
    product acceptance proves the default no-MCP supervisor behavior.

## Validation Plan

### Red/green implementation loop

```sh
cargo test -p nils-codex-cli --test integration execution_capsule
cargo test -p nils-codex-cli --test integration agent_isolation
cargo fmt --all -- --check
cargo clippy -p nils-codex-cli --all-targets --all-features -- -D warnings
```

Use the repository's actual integration-test filter syntax if the recovered
prototype registers modules differently.

### Documentation and completion

```sh
bash scripts/ci/docs-placement-audit.sh --strict
bash scripts/ci/docs-hygiene-audit.sh --strict
bash scripts/ci/markdownlint-audit.sh --strict
zsh -n completions/zsh/_codex-cli
bash -n completions/bash/codex-cli
zsh -f tests/zsh/completion.test.zsh
bash scripts/ci/completion-freshness-audit.sh --strict
bash scripts/ci/completion-flag-parity-audit.sh --strict
```

### Required local finish line

```sh
bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast
```

### Release-quality verification

Before release, run the repository's full declared checks using the normal
release workflow. The GitHub required-check gate can be restored when the
provider path is available; it is not waived by the current local-only
handoff.

### Runtime-kit adoption

After releasing and pinning the new `nils-cli` version:

```sh
bash tests/runtime-smoke/run.sh --mode product --product codex --probe-only
bash tests/runtime-smoke/run.sh --mode convergence
bash scripts/ci/all.sh
bash tests/hooks/run.sh
```

Add deterministic runtime-kit acceptance that supplies a fake Codex home with
failing direct/plugin MCP sentinels and proves a default Execution Capsule
still completes without starting them.

Do not use a real expired OAuth credential as the primary acceptance fixture.

## Risks and Guardrails

| Risk | Guardrail |
| --- | --- |
| Project config reintroduces MCP | Parse applicable project config and fail closed unless enforcement is proven |
| Governance disappears with MCP | Use a distinct supervisor projection; test instructions and hooks positively |
| Projection copies secrets | Fresh allowlist construction, private file modes, sentinel leakage tests |
| Plugin MCP bypasses direct overrides | Disable plugin/app discovery; do not rely on `mcp_servers.*` enumeration |
| Inherited mode becomes implicit | CLI-only explicit flag; no env/config default and no fallback |
| Startup timeout kills legitimate work | Apply only before the first JSONL event; no total execution timeout |
| Reader/timeout weakens evidence | Keep bounded concurrent drain and existing helper/final/receipt verification |
| Prototype source is not on main | Recover and green its baseline before MCP edits; never cite the private snapshot as canonical source |
| Docs and schemas drift | Update checked schema, JSON consumer runbook, completion assets, and contract tests together |

## Recommended Review

The eventual L2 implementation should receive:

- API-contract review for CLI flags, error envelopes, receipt schema, and
  completion behavior;
- security review for config projection, auth bridging, hook preservation,
  project config, process termination, and secret leakage;
- maintainability review for shared child-home abstractions;
- testing review for deterministic capability and timeout coverage.

Run a red-team follow-up if any specialist identifies a critical issue in the
governance projection or evidence boundary.

## Read First

1. `crates/codex-cli/src/runtime/isolated.rs`
2. `crates/codex-cli/src/runtime/agent_mode.rs`
3. `crates/codex-cli/tests/integration/agent_isolation.rs`
4. `crates/codex-cli/docs/README.md`
5. After prototype recovery:
   `crates/codex-cli/docs/specs/execution-capsule-v1.md`
6. After prototype recovery:
   `crates/codex-cli/src/agent/capsule.rs`
7. After prototype recovery:
   `crates/codex-cli/tests/integration/execution_capsule.rs`
8. Official Codex configuration reference:
   <https://learn.chatgpt.com/docs/config-file/config-reference>
9. Official Codex MCP documentation:
   <https://learn.chatgpt.com/docs/extend/mcp>

## Recommended Next Artifact

When implementation is ready to begin, graduate this report into an L2 plan
bundle. Use this document as `Source type: discussion-to-implementation-doc`
under the plan's `Read First` section. The plan must separate:

1. Execution Capsule prototype recovery and baseline validation;
2. shared supervisor-home/runtime primitives;
3. MCP mode, observability, receipt, and CLI contracts;
4. release and runtime-kit adoption.

Do not open the plan, issue, or PR until implementation is intentionally
resumed.
