# Claude CLI Feature Gap and Feasibility Implementation Handoff

- Status: first recommended delivery implemented and validated
- Date: 2026-07-25
- Source: discussion-to-implementation-doc from the requested `codex-cli` /
  `claude-cli` comparison
- Intended next step: select a separately bounded capsule delivery only if
  Execution Capsule work is wanted
- Ownership: workspace-level transient development record

## Purpose

Inventory the capabilities present in `codex-cli` but absent or materially
different in `claude-cli`, then assess the initial feasibility and value of
adding each gap.

The request used the name `codec-cli`. This document interprets that as the
repository's `codex-cli`; the workspace contains `crates/codex-cli` and no
`codec-cli` crate or binary.

This began as an implementation-readiness source. It now preserves both the
pre-implementation baseline and the completed outcome of the first recommended
delivery. It does not authorize provider delivery or any deferred feature.

## Evidence

### User and repository evidence

- `[U1]` User request: compare the mature `codex-cli` with `claude-cli`,
  assess the feasibility of each missing capability, preserve the result as an
  implementation-readiness document, and commit it to a local branch.
- `[F1]` Current command models and dispatch:
  `crates/codex-cli/src/{cli,main}.rs` and
  `crates/claude-cli/src/{cli,main}.rs`.
- `[F2]` Current user-facing contracts:
  `crates/codex-cli/README.md`, `crates/claude-cli/README.md`, and the two
  crate-local docs indexes.
- `[F3]` Codex implementation depth:
  `crates/codex-cli/src/{agent,auth,rate_limits,runtime}/`.
- `[F4]` Claude implementation:
  `crates/claude-cli/src/agent/resume.rs` and
  `crates/claude-cli/src/prompt_segment/`.
- `[F5]` Contract and consumer evidence:
  `crates/codex-cli/docs/`,
  `docs/specs/codex-gemini-cli-parity-contract-v1.md`, and
  `crates/agent-session/src/serve.rs`.
- `[F6]` Shared runtime boundary:
  `crates/nils-common/src/provider_runtime/`; its execution profile currently
  supports `CodexStyle` and `GeminiStyle`, not Claude.
- `[F7]` Historical prior art:
  commit `6a00f166` added an earlier full-surface `claude-cli`, commit
  `291fee6c` removed that provider architecture, and commit `ca0533c7`
  introduced the current narrower native helper.

### Current Claude Code evidence

- `[A1]` Local capability probe on 2026-07-25:
  `claude --version` reported Claude Code `2.1.220`; `claude --help`,
  `claude auth --help`, `claude auth login --help`, and
  `claude auth status --help` were inspected without starting a model turn.
- `[W1]` The official
  [Claude Code CLI reference](https://code.claude.com/docs/en/cli-reference)
  documents non-interactive `-p`, JSON/stream-JSON output, JSON Schema,
  no-session persistence, tool and permission controls, `--safe-mode`,
  `--bare`, strict MCP configuration, settings-source selection, auth
  commands, and read-only `claude doctor`.
- `[W2]` The official
  [headless-mode guide](https://code.claude.com/docs/en/headless) documents
  programmatic Claude Code execution.
- `[W3]` The official
  [authentication guide](https://code.claude.com/docs/en/authentication)
  documents Claude Code-owned credential storage: encrypted macOS Keychain on
  macOS and a mode-`0600` credentials file on Linux and Windows.
- `[W4]` The official
  [settings reference](https://code.claude.com/docs/en/settings) documents
  user, project, local, managed, hook, MCP, and plugin configuration surfaces.

External capability claims below are current as of the date above. Every
implementation must probe the installed Claude Code capabilities and fail
closed instead of assuming that a version string implies a flag contract.

## Evaluation scale

| Rating | Meaning |
| --- | --- |
| High | Supported by current upstream CLI primitives and local shared helpers; bounded implementation risk |
| Medium | Technically plausible, but provider semantics or security boundaries need a Claude-specific design |
| Low | Possible only with brittle credential/state handling or substantial new architecture |
| Not applicable | Codex-specific behavior has no useful Claude equivalent and should not be copied |

Effort is an initial relative estimate:

- `S`: localized command/adapter and contract tests
- `M`: several modules plus completion/docs/contract work
- `L`: security-sensitive or cross-module design
- `XL`: standalone subsystem with retained artifacts and adversarial tests

## Confirmed baseline before implementation

### Command-surface comparison

| Capability | `codex-cli` | `claude-cli` | Current conclusion |
| --- | --- | --- | --- |
| Root help/version | Present | Present | Equivalent |
| Completion export | Bash and zsh | Bash and zsh | Equivalent |
| `agent resume` | Present | Present | Provider-specific equivalent |
| `agent prompt` | Present | Missing | Real gap |
| `agent advice` | Present | Missing | Real gap |
| `agent knowledge` | Present | Missing | Real gap |
| `agent commit` | Present | Missing | Real gap |
| `agent doctor` | Present | Missing | Real gap |
| `agent run` capsule | Present | Missing | Real gap, large subsystem |
| Auth login/status | Wrapper-owned | Missing from wrapper | Upstream Claude commands exist |
| Named auth profiles | Full lifecycle | Missing | Codex-specific storage model |
| Auth refresh/remote authority | Present | Missing | Codex-specific token ownership |
| Quota/usage | `diag rate-limits` | Top-level `usage` | Core single-account outcome exists |
| Multi-profile quota/watch | Present | Missing | Depends on a profile model |
| Config show/set | Present | Missing | Real gap |
| Prompt segment | Present | Present | Claude has smaller flag set and a blocking stale-refresh path |
| Versioned JSON docs | Auth, diag, capsule specs and schemas | README-only usage/status description | Documentation/contract gap |

### Scale indicators

These numbers indicate surface area, not quality:

- Codex Rust source: about 15,423 lines across 42 source files.
- Claude Rust source: about 2,705 lines across 12 source files.
- Codex test annotations: 364 across the crate source and integration tree.
- Claude test annotations: 31 in the current crate source and consolidated
  integration target.
- Codex agent/runtime alone is about 4,833 lines; auth is about 3,709 lines;
  rate-limit handling is about 4,633 lines.
- Claude's current 2,705 lines are primarily usage probing, cache safety,
  prompt rendering, and resume behavior.

The gap is therefore architectural, not just missing clap variants.

## Decisions and working assumptions

1. Target outcome parity, not identical command topology. Claude-native
   capabilities may keep Claude-native names when an alias would add no value.
2. New agent execution should wrap the installed `claude` binary. Do not
   resurrect the removed `claude-core` direct HTTP client; native CLI execution
   preserves Claude subscription auth and upstream runtime semantics.
3. Preserve the existing `claude-cli.usage.v1` and
   `claude-cli.prompt-segment.v1` contracts. Additive aliases must delegate to
   the same typed implementation rather than create a second behavior path.
4. Do not read, copy, export, or refresh Claude OAuth material behind Claude
   Code's back. Auth lifecycle work must delegate to supported upstream
   commands or an explicitly designed non-OAuth profile mechanism.
5. A Claude one-shot runtime must use a Claude-specific safety name and
   contract. Do not call it equivalent to Codex `isolated` unless tests prove
   the same instruction, hook, MCP, plugin, tool, persistence, and credential
   boundaries.
6. Capability detection is the compatibility gate. Missing required flags fail
   closed with a stable error; there is no silent fallback from the safe
   profile to inherited ambient configuration.
7. Automated tests and repository gates do not spend live model turns. The
   user's later execution request required real E2E validation, so the delivery
   also includes a bounded manual live smoke recorded below.

## Feature gap and feasibility assessment

### Agent commands

| Missing capability | Feasibility | Effort | Assessment and recommended boundary |
| --- | --- | --- | --- |
| `agent prompt` | High | M | Claude Code supports `-p`, model/effort selection, no-session persistence, JSON output, tool restriction, safe mode, and strict MCP configuration `[A1][W1]`. Add argv-based execution with explicit safe/inherited profiles, stdin fallback, deterministic exit mapping, and no shell interpolation. |
| `agent advice` | High | M | Same execution adapter as `prompt`, with a versioned repository-owned prompt template. Default to a read-only tool profile and no session persistence. |
| `agent knowledge` | High | M | Same as `advice`; use one shared input/template runner and a read-only tool profile. |
| `agent commit` | Medium-High | L | Claude Code supports `--json-schema`, `-p`, and no-session persistence `[W1]`. Reuse the Codex commit safety sequence: staged-context bundle, bounded structured message, HEAD/tree drift check, and `semantic-commit` as the sole commit writer. Do not copy the inherited fallback or let the model invoke Git. |
| `agent doctor` | High | M | Probe the exact flags needed by the selected Claude runtime and emit a secret-free JSON contract. It can also report upstream `claude doctor` status, but upstream install health is not proof of wrapper isolation. |
| `agent run --capsule` | Medium | XL | Claude Code has stream JSON, JSON Schema, tool controls, hooks, strict MCP configuration, and safe mode `[W1]`. The hard part is preserving repository governance/hooks while excluding undeclared MCP/plugins and producing equivalent attestations. Treat this as a provider-neutral capsule-engine design, not a port of `capsule.rs`. |

#### Agent-runtime constraints

- `--bare` provides the strongest upstream minimal mode, but the inspected
  `2.1.220` help says it uses API-key or `apiKeyHelper` auth and does not read
  OAuth/Keychain credentials. It cannot be the unconditional default for
  subscription users.
- `--safe-mode` preserves authentication and disables most customizations, but
  managed policy can still affect the runtime `[W1][W4]`. It is a useful
  provider primitive, not sufficient evidence for Codex-equivalent isolation.
- Advice, knowledge, and commit should not receive edit or shell authority.
  Prompt authority should be an explicit, documented profile rather than an
  accidental consequence of Claude defaults.
- The current `nils-common::provider_runtime::ExecInvocation` cannot represent
  Claude's command shape `[F6]`. Start with a crate-local adapter; extract a
  shared abstraction only after its invariants are clear.

### Authentication commands

| Missing capability | Feasibility | Effort | Assessment and recommendation |
| --- | --- | --- | --- |
| `auth login` | High | S-M | Delegate to `claude auth login`, including supported Claude subscription, Console, email, and SSO options `[A1][W1]`. Normalize exit status and optional wrapper JSON without capturing credentials. |
| `auth status` | High | S | Delegate to `claude auth status`, which has upstream JSON/text output and documented `0`/`1` login semantics `[A1][W1]`. Parse only bounded public status fields into a `claude-cli.auth.v1` envelope. |
| `auth logout` | High | S | Add the Claude-native counterpart even though Codex has no exact same command. Delegate to `claude auth logout`; never delete credential storage directly. |
| `auth use <profile>` | Low | L-XL | There is no supported named OAuth-profile switch in the inspected upstream CLI. Cross-platform Keychain/file mutation would be brittle. Defer until a profile contract based on supported `CLAUDE_CONFIG_DIR`, API keys, or `apiKeyHelper` is intentionally selected. |
| `auth save <profile>` | Low | L-XL | Do not copy managed OAuth credentials. A future command could register a non-secret profile descriptor, but it must not serialize token material. |
| `auth remove <profile>` | Low | L | Without a wrapper-owned profile store there is nothing safe to remove. Use upstream `auth logout` for the active Claude credential. |
| `auth current` | Low-Medium | M | Upstream status can identify authentication type, but named-profile identity does not exist. Implement only after a profile model exists; otherwise `auth status` is the correct outcome. |
| `auth sync` | Not applicable | — | Codex syncs a file-backed active auth document into named secret files. Claude Code owns its credential persistence; copying this command would create unsafe duplicate ownership. |
| `auth refresh` | Not applicable | — | The upstream CLI owns OAuth refresh and exposes no supported refresh command. Wrapper-side refresh-token handling is out of scope. |
| `auth auto-refresh` | Not applicable | — | Same boundary as manual refresh. Usage failures should return stable reason codes and recommend supported re-login, not manipulate tokens. |
| `auth remote pull` | Low | XL | Upstream Claude does not expose an access-only OAuth export/import contract. Do not design SSH token transport around private credential formats. A future remote authority must use an officially supported API-key/helper flow. |

The safe near-term auth surface is therefore `login`, `status`, and `logout`.
Named-profile parity is deliberately deferred rather than partially emulated.

### Usage and diagnostics

`claude-cli usage` already provides a useful provider-specific counterpart to
Codex `diag rate-limits`: OAuth, bounded native-CLI fallback, cache fallback,
versioned JSON, normalized windows, and stable provider-neutral reason codes
`[F2][F4][F5]`.

| Missing or different capability | Feasibility | Effort | Assessment and recommendation |
| --- | --- | --- | --- |
| `diag usage` namespace alias | High | S | Optional additive alias to the existing `usage` implementation. Do not rename or duplicate the current top-level contract. |
| Cache clear | High | S | Add an explicit usage/prompt cache clear command or flag with exact-path validation and deterministic JSON/text output. |
| Debug diagnostics | Medium | M | Expose bounded source-attempt classifications and timing, never provider bodies, terminal transcripts, credentials, or absolute private paths. |
| One-line output | Complete | — | Current text usage output is already one line. |
| Cached-only mode | Complete | — | `--source cache` already provides the outcome. |
| Single live source selection | Complete | — | `--source oauth` and `--source cli` already provide focused diagnostics. |
| `--all` named accounts | Low | L-XL | Not meaningful until a supported profile model exists. |
| `--async --jobs` | Low | L | Multi-account concurrency depends on `--all`; do not add concurrency around one active account. |
| `--watch` | Low-Medium | M-L | Technically simple for one account, but poor value without multiple profiles. Prefer the existing prompt-segment/cache consumers until a concrete operator need exists. |

### Configuration

| Missing capability | Feasibility | Effort | Assessment and recommendation |
| --- | --- | --- | --- |
| `config show` | High | S-M | Report effective wrapper configuration such as model, effort, runtime profile, persistence, usage cache, and credential source class. Never print API keys, token content, raw credential files, or unredacted upstream settings. |
| `config set` | High | S-M | Match Codex's current-shell behavior by emitting a safely quoted export snippet. Do not mutate Claude settings files. Validate enums and numeric limits before output. |

Potential wrapper keys are `model`, `effort`, `runtime`, and
`no-session-persistence`. Usage/prompt cache knobs already have established
environment names. API keys are explicitly excluded from `config set`.

### Prompt-segment parity

| Missing or different capability | Feasibility | Effort | Assessment and recommendation |
| --- | --- | --- | --- |
| Non-blocking stale refresh | High | M | High-value gap. Codex renders cache then starts a detached refresh; Claude currently performs a blocking HTTP attempt on the prompt path. Reuse Claude's existing cooldown/lock primitives, spawn `claude-cli prompt-segment --refresh`, and test slow-server latency plus eventual cache update. |
| `--no-5h` | High | S | Add render filtering without changing the cache schema. |
| `--show-timezone` | High | S | Add a timezone-aware default format while preserving explicit `--time-format` precedence. |
| Zsh percent escaping | High | S | Add an opt-in Claude environment switch matching the Codex rendering guardrail. |
| Explicit enable gate | Medium | S-M | Claude currently treats available credentials as enabled. Keep that default unless an operator-facing disable requirement is established; an additive disable env is safe. |
| Account/name prefix | Low | M-L | Depends on a supported profile/identity model. Do not infer or print full user identity from credential material. |

### Contract, documentation, and test foundation

| Gap | Feasibility | Effort | Assessment and recommendation |
| --- | --- | --- | --- |
| Claude JSON contract spec | High | M | Add a crate-local spec covering existing usage/status schemas before adding auth or agent JSON. Record compatibility, failure envelopes, exit codes, and redaction. |
| Consumer runbook | High | S | Document `agent-session` as an existing `claude-cli.usage.v1` consumer and define branching on fields, never messages. |
| Modular integration tests | High | M | Split the current consolidated source file into modules under the retained single integration target, following repository test-target policy. |
| Completion parity tests | High | S-M | Add leaf-help/flag parity checks comparable to Codex; regenerate both shell assets with every command change. |
| Runtime capability fixtures | High | M | Fake multiple Claude CLI capability sets so removed flags fail closed and current flags produce exact argv. |

## Findings and recommended priority

| Priority | Finding | Evidence | Likely fix location | Acceptance signal |
| --- | --- | --- | --- | --- |
| P0 | Claude lacks a documented safe one-shot runtime contract | `[F1][F4][A1][W1]` | `crates/claude-cli/src/agent/`, crate-local spec | Fake binary proves exact argv, no session persistence, bounded tools, and fail-closed capability handling |
| P0 | Existing Claude JSON schemas have no standalone contract document | `[F2][F5]` | `crates/claude-cli/docs/specs/` and docs index | Spec covers current consumers, schemas, exits, errors, and redaction |
| P1 | `prompt`, `advice`, and `knowledge` are missing despite strong upstream support | `[F1][F7][A1][W1]` | `src/agent/`, `src/cli.rs`, `src/main.rs` | All three work through one adapter with text/stdin cases and no live calls |
| P1 | Prompt-segment stale refresh can block shell rendering | `[F4]` | `src/prompt_segment/{mod,cache}.rs` plus refresh module | Slow endpoint does not delay cached prompt output; detached refresh updates cache |
| P1 | Safe upstream auth operations are not exposed | `[A1][W1][W3]` | `src/auth/`, CLI/dispatch, JSON spec | Login/status/logout delegate without token leakage and preserve upstream status |
| P1 | Wrapper config discovery is missing | `[F1]` | `src/config.rs`, CLI/dispatch | Show is secret-free; set output is quoted and validated |
| P2 | Safe semantic commit generation is missing | `[F3][A1][W1]` | `src/agent/commit.rs`; possible shared extraction | Structured output only, drift detection, and `semantic-commit` sole writer |
| P2 | Runtime readiness cannot be checked without manual reasoning | `[F3][A1]` | `src/agent/doctor.rs` | Secret-free text/JSON capability report with no model turn |
| P3 | Execution Capsule has no Claude provider | `[F3][A1][W1][W4]` | New provider-neutral capsule core plus thin adapters | Equivalent manifest, authority, integrity, event, receipt, and failure contracts |
| Deferred | Named auth profiles, refresh, remote pull, multi-account usage | `[W3][F3][F4]` | No current fix location | Revisit only after a supported, non-token-copying profile contract exists |

## Recommended delivery boundary

The first implementation delivery should contain only:

- the Claude runtime contract and capability probe;
- the crate-local JSON contract specification;
- `agent prompt`, `agent advice`, and `agent knowledge`;
- `auth login`, `auth status`, and `auth logout`;
- `config show` and `config set`;
- non-blocking prompt-segment refresh and the small render flags;
- completion, README, dependency, and focused contract-test updates.

Keep `agent commit` as a second bounded delivery because it adds Git/index
integrity and semantic-commit orchestration. Keep Execution Capsule separate
because its security model and evidence artifacts need independent review.

Do not include named auth profiles, token refresh, remote token pull,
multi-account quota concurrency, or watch mode in either initial delivery.

## First-delivery implementation outcome

Completed in this delivery:

- one shared Claude one-shot adapter for `agent prompt`, `agent advice`, and
  `agent knowledge`;
- explicit `safe` and `inherited` runtime profiles, fail-closed installed-CLI
  capability probing, read-only/empty tool allowlists, and default
  no-session persistence;
- bounded 1 MiB prompt input delivered over stdin rather than child argv;
- in-flight stdout/stderr caps, five/three-second deadlines, and process-group
  termination for capability/auth-status probes;
- upstream-delegating `auth login`, `auth status`, and `auth logout`;
- redacted `claude-cli.auth.v1` output with strict JSON-shape and exit-status
  consistency checks;
- validated, secret-free `config show` and shell-exporting `config set`;
- non-blocking, coalesced prompt-segment background refresh with
  OS-backed exclusive refresh locks;
- `--no-5h`, `--show-timezone`, and opt-in zsh percent escaping;
- standalone JSON contract and consumer documentation, updated runtime
  dependencies, and regenerated bash/zsh completion assets.

The implementation deliberately remains crate-local. It did not weaken the
Codex/Gemini shared runtime abstraction to fit Claude-specific invocation
semantics.

### E2E and validation outcome

- Real installed Claude Code `2.1.220` successfully completed the safe
  `agent prompt`, `agent advice`, and `agent knowledge` paths with distinct
  exact success markers and exit `0`.
- Real `claude auth status --json` delegation succeeded through the redacted
  wrapper contract. Login and logout were not run against the active account
  because they would mutate credentials; direct executable E2E fixtures prove
  their exact argv and exit propagation instead.
- Real `config show` and `config set effort high` succeeded.
- Real cached prompt rendering with `--no-5h --show-timezone` succeeded.
- A live prompt-segment OAuth refresh could not be certified because the
  installed Claude login did not expose a prompt-segment access token. The
  detached process/network path is instead covered end to end with a delayed
  loopback HTTP server, including immediate return, exactly one child launch,
  eventual cache creation/update, and subsequent rendering.
- The crate test suite, clippy, generated-completion comparison, shell syntax
  checks, completion behavior test, and repository local-fast gate are the
  declared automated gates. Their final results are recorded in the delivery
  commit and handoff.

Still deferred by design:

- Execution Capsule support;
- named auth profiles, wrapper-side OAuth refresh, remote credential transfer,
  and multi-account usage/watch behavior;
- modularizing the retained integration test target.

## Second-delivery implementation outcome

Completed on 2026-07-26:

- `agent commit` with an always-safe structured-output runtime, optional
  model/effort and wording guidance, explicit `--auto-stage`, and opt-in
  `--push`;
- bounded `semantic-commit staged-context` input, an empty Claude tool list,
  temporary child cwd, cleared Git control environment, and no session
  persistence;
- fail-closed `nils-scrub` scanning before staged content can reach Claude;
- upstream JSON Schema constraints plus local Conventional Commit validation;
- post-model `HEAD` and staged-tree drift rejection;
- `semantic-commit` as the sole commit writer with `--expect-head` and
  automation mode;
- stable index preservation before the commit writer runs, post-failure
  repository re-observation instead of an unsafe staged-state claim, plus
  preservation of a successful local commit when push fails;
- aggregate stdout/stderr caps, mutation deadlines, process-group cleanup,
  bounded transient process retries, created-commit parent/tree verification,
  nonblocking output capture that cannot be held by escaped descendants, and
  explicit verified-object push to a captured/revalidated endpoint pinned
  against inherited Git URL rewrite chains;
- secret-free `agent doctor` text/JSON reporting for required Claude flags,
  `git`, the exact `semantic-commit` help contract, and bounded upstream
  installation health.

### Second-delivery E2E outcome

- Real Claude Code `2.1.220` structured output generated the final
  post-review commit
  `test(agent-commit): add final E2E marker for push endpoint pinning`.
- Real `semantic-commit` created commit
  `73b59ec4dafce0423e328c904bdefb14e2f3896c` in an isolated non-default Git
  fixture branch.
- `--auto-stage` picked up the untracked marker and `--push` created the same
  branch in a local `/tmp` bare remote; local and remote object IDs matched.
- Real `agent doctor --format json` reported all required flags and
  dependencies available, upstream doctor status `ready`, overall
  `ready: true`, and exit `0`.
- Fixture tests additionally prove invalid structured messages and concurrent
  index drift cannot create a commit, the index remains staged, upstream
  doctor output is bounded, and recognizable private markers are not emitted.
- Automated Git fixtures cover both `--auto-stage --push` success against a
  bare remote, remote-endpoint retarget rejection, chained
  `pushInsteadOf`/`insteadOf` pinning, and push failure after a verified local
  commit; process tests prove timeout cleanup reaches descendants and that
  escaped pipe holders cannot defeat timeout/output limits, while mutation
  diagnostics prove a changed `HEAD` is never misreported as a still-staged
  index.
- The final Claude crate run used retries disabled and passed all `105/105`
  tests. The standard repository `--local-fast` gate passed all `7,476/7,476`
  workspace tests plus doctests; its only retry was an unrelated existing
  `nils-agent-hook` flaky test.
- An additional non-required whole-workspace run with retries forcibly
  disabled stopped on the existing issue-`#934` `nils-agent-session` timing
  flake. That exact test passed when rerun alone, and neither the no-retry
  Claude crate run nor the standard repository gate reported a Claude failure.

## Implementation boundaries

### Process execution

- Build `claude` argv as discrete arguments; never shell-interpolate prompts,
  paths, JSON Schema, or settings.
- Inherit stdout/stderr only for human text mode. JSON-producing wrapper paths
  must bound and parse captured output before emitting their own envelope.
- Bound subprocess startup/execution where the command is diagnostic or
  non-interactive. Propagate meaningful child exit codes through the workspace
  exit taxonomy.
- Clear wrapper control variables from child environments when they could
  recursively alter the selected runtime.

### Runtime safety

- Define explicit safe and inherited profiles.
- Safe mode must state exactly which instructions, settings, hooks, MCP,
  plugins, skills, tools, credentials, and session files can apply.
- Required upstream flags are probed before launch.
- Missing safe capabilities return a stable unsupported error. They never
  select inherited mode automatically.
- A doctor result reports only tested properties; it must not describe
  `--safe-mode` as stronger than the official/provider evidence supports.

### Credentials

- Use upstream auth commands for login/status/logout.
- Treat macOS Keychain and the platform credentials file as upstream-owned.
- Never emit tokens, raw credential JSON, Keychain payloads, refresh tokens,
  API keys, emails, absolute credential paths, or provider response bodies.
- Preserve the current prompt-segment automation overrides, but describe them
  as explicit caller-supplied credentials, not a named-profile store.

### Contracts

- Use `nils_common::diag_output` for versioned envelopes.
- New machine errors include stable code, deterministic exit status, typed
  retryability/next action where recoverable, and bounded recovery detail.
- Preserve existing `usage` success-with-reason semantics. Do not silently
  convert current unavailable-usage results into command errors.
- Additive command aliases delegate to the same implementation and schema.

### Shared-code policy

- Reuse `nils-common` process, shell quoting, diagnostic envelope, provider
  usage classification, Git, and cache-policy helpers.
- Do not add `ClaudeStyle` to `provider_runtime` merely to avoid a small
  crate-local adapter. Extract only when the shared type can express Claude's
  safe/inherited profiles without weakening Codex/Gemini contracts.
- For commit work, prefer extracting the Codex structured-message validation
  and drift-safe `semantic-commit` handoff over copying 500 lines into Claude.

### Completion and documentation

- Keep clap as the command source of truth.
- Regenerate `completions/zsh/_claude-cli` and
  `completions/bash/claude-cli` in the same change.
- Retain the current no-alias policy unless a separate shell UX decision adds a
  Claude alias family.
- Update `BINARY_DEPENDENCIES.md` when agent commands make `claude` a broader
  runtime dependency than resume/usage fallback.

## Acceptance criteria

### First delivery

- Root help exposes the new command families and keeps `-V, --version`.
- `agent prompt|advice|knowledge`:
  - accept argv input and stdin fallback;
  - reject empty input deterministically;
  - keep bounded prompt input out of argv and avoid shell interpolation;
  - use the declared safe profile by default;
  - never persist a session in the safe profile;
  - fail closed when a required Claude capability is absent;
  - never use a live Claude API in automated tests.
- `auth login|status|logout`:
  - delegate only to supported upstream commands;
  - normalize success/failure without token or private-path leakage;
  - preserve upstream authenticated/unauthenticated exit meaning;
  - expose a versioned wrapper JSON contract where JSON is supported.
- `config show` is secret-free and reports effective wrapper values.
- `config set` validates values and emits safely quoted current-shell exports
  without writing settings or credential files.
- Prompt-segment:
  - prints eligible cached output before starting refresh;
  - returns promptly when the endpoint is slow;
  - coalesces background refresh attempts;
  - preserves the 599-second display ceiling and five-second future-clock
    tolerance;
  - supports the new render flags without changing cache schema.
- Existing `claude-cli.usage.v1`,
  `claude-cli.prompt-segment.v1`, resume behavior, completion architecture, and
  `agent-session` consumption remain compatible.

### Commit delivery

- The model receives only a bounded staged-context bundle.
- Claude structured output is validated against both JSON Schema and local
  semantic constraints.
- HEAD and staged tree are unchanged before committing.
- `semantic-commit` is the sole commit writer.
- Model failure, schema failure, or pre-writer drift leaves the index staged
  and reports a stable error.
- Commit-writer failure re-observes `HEAD` and the index, preserves any
  observed mutation, skips push, and never falsely claims the index is staged.
- Push is never implicit; partial commit-success/push-failure is reported
  without resetting the commit, and captured upstream endpoint drift prevents
  a push.

### Capsule delivery

- Provider-neutral manifest semantics are unchanged or intentionally
  versioned.
- Workspace and host access are separately acknowledged and attested.
- Hook/instruction preservation and MCP/plugin exclusion are positively
  tested, not inferred from flag names.
- Startup timeout, process-group cleanup, integrity rechecks, validation
  execution, final schema, receipts, and redaction match the retained capsule
  trust model.

## Validation commands

Focused checks:

```bash
cargo test -p nils-claude-cli --test integration
mkdir -p target/verification
cargo run -q -p nils-claude-cli -- completion zsh > target/verification/claude-cli.zsh
cargo run -q -p nils-claude-cli -- completion bash > target/verification/claude-cli.bash
cmp target/verification/claude-cli.zsh completions/zsh/_claude-cli
cmp target/verification/claude-cli.bash completions/bash/claude-cli
zsh -n completions/zsh/_claude-cli
bash -n completions/bash/claude-cli
zsh -f tests/zsh/completion.test.zsh
```

Repository gate:

```bash
bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast
```

For this assessment document alone, use the docs-only gate:

```bash
bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only
```

Live Claude calls remain excluded from automated validation. The bounded
manual smoke above records only the Claude version, command class, redacted
result class, and exit code; it retains no credentials or private paths.

## Risks and guardrails

- **Isolation overclaim:** Claude safe/bare modes do not have the same auth and
  managed-policy behavior as Codex's private child home. Name and test the
  actual Claude boundary.
- **Upstream CLI drift:** Claude flags evolve quickly. Probe exact capabilities
  and cover old/missing flag fixtures.
- **Credential duplication:** Named-secret parity would tempt direct Keychain
  or credentials-file manipulation. Keep upstream ownership.
- **Contract drift:** `agent-session` already consumes
  `claude-cli.usage.v1`. Preserve existing field meaning and reason-code
  behavior.
- **Prompt latency:** Background refresh must detach safely, coalesce attempts,
  and not outlive its need as an unbounded process.
- **Authority broadening:** Prompt/advice/knowledge profiles must not inherit
  ambient tools, hooks, MCP, or plugins by accident.
- **Shared abstraction pressure:** Do not weaken Codex/Gemini profiles to make
  Claude fit an enum designed for different invocation shapes.
- **Historical-code trap:** The removed 2026-02 implementation is useful
  interface prior art, but its direct API/provider architecture is not the
  current target.
- **Scope explosion:** Capsule, auth profiles, and multi-account diagnostics
  are independent design problems and stay outside the first delivery.

## Retention intent

Keep this file under `docs/discussions/` as the historical baseline, decision
record, and first-delivery outcome. Durable user and machine contracts have
been distilled into `crates/claude-cli/README.md` and
`crates/claude-cli/docs/`.

## Read-first references

- `crates/claude-cli/README.md`
- `crates/claude-cli/src/cli.rs`
- `crates/claude-cli/src/main.rs`
- `crates/claude-cli/src/prompt_segment/`
- `crates/claude-cli/tests/integration.rs`
- `crates/codex-cli/README.md`
- `crates/codex-cli/src/agent/`
- `crates/codex-cli/src/runtime/`
- `crates/codex-cli/src/auth/`
- `crates/codex-cli/src/rate_limits/`
- `crates/codex-cli/docs/`
- `crates/nils-common/src/provider_runtime/`
- `docs/runbooks/cli-completion-development-standard.md`
- `docs/specs/cli-service-json-contract-guideline-v1.md`
- Official Claude Code references `[W1]` through `[W4]`

## Recommended next artifact

If further parity work is requested, create a new bounded artifact for one
separate outcome: commit delivery, doctor delivery, or capsule delivery. Keep
named auth profiles and multi-account diagnostics deferred until a supported
non-token-copying profile contract exists.
