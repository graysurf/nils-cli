# forge-cli Inbox Provider Timeout Resilience Source

- Status: ready for plan generation
- Date: 2026-05-24
- Source: user report that the company GitLab server requires VPN and can make
  mixed-provider `forge-cli inbox` calls feel blocked when VPN is disconnected,
  follow-up clarification that the company VPN uses OpenVPN, and a read-only
  source review of the current `forge-cli inbox` implementation.
- Intended next step: create an implementation plan and issue-backed tracker for
  bounded GitLab failure handling without hiding provider errors.

## Purpose

Make `forge-cli inbox` usable when GitHub is reachable but a configured GitLab
host is temporarily unreachable because VPN is disconnected. The command should
return reachable provider results promptly, preserve machine-readable evidence
that GitLab failed, and give automation a strict mode when partial provider
failure should fail the invocation.

This is a follow-up to the existing inbox latency work. The prior latency plan
covered query pruning and parallel provider/query execution. This work covers
VPN readiness gating, bounded subprocess/provider waits, explicit timeout
errors, strict partial failure handling, and optional stale-cache fallback.

## Source Tags

- `[U1]` User reports that the company GitLab server requires VPN, and VPN may
  or may not be connected during daily use.
- `[U2]` User wants GitHub results to be displayable once GitHub responds,
  instead of waiting for a slow or unreachable GitLab server.
- `[U3]` User explicitly says silently eating GitLab connection failure is not
  acceptable.
- `[U4]` User clarified that the company VPN uses OpenVPN and supplied a local
  profile path in chat. That path is intentionally not recorded in this source
  document or the provider issue.
- `[U5]` User allows installing a missing OpenVPN CLI through Homebrew when
  local implementation or smoke validation needs it.
- `[F1]` `crates/forge-cli/src/ops/inbox.rs` defaults mixed-provider inbox mode
  to GitHub plus GitLab when top-level `--provider` is not supplied.
- `[F2]` `crates/forge-cli/src/ops/inbox.rs` already runs provider plans in
  parallel, so GitHub work does not wait for the GitLab identity lookup to
  start.
- `[F3]` `crates/forge-cli/src/ops/inbox.rs` still waits for every provider
  thread to join before emitting the single final envelope.
- `[F4]` `crates/forge-cli/src/ops/inbox.rs` records provider-local failures in
  `data.providers[]`, adds a warning, keeps successful provider items, and only
  fails the whole call when all selected providers fail.
- `[F5]` `crates/forge-cli/src/backend.rs` executes `gh` and `glab`
  subprocesses through blocking `Command::output()` without a subprocess
  deadline.
- `[F6]` `crates/nils-common/src/cli_contract.rs` success envelopes can carry
  `data` plus warnings, while failure envelopes currently carry `error` without
  the success payload.
- `[F7]` The existing `forge-cli-inbox-latency` plan deliberately deferred
  persistent caching until filtering and parallelism had been measured.
- `[A1]` Local preflight on 2026-05-24 found Homebrew available and did not find
  an `openvpn` binary on `PATH`.
- `[I1]` Inference from `[F2]`, `[F3]`, and `[F5]`: current mixed-provider mode
  is parallel but still bounded by the slowest selected backend process because
  no child process timeout exists.
- `[I2]` Inference from `[U2]`, `[U3]`, and `[F4]`: default interactive behavior
  should remain partial success, not fail-fast, but provider timeout must be
  visible in provider status and warnings.
- `[I3]` Inference from `[F6]`: a strict mode can fail partial provider failure,
  but it must define whether partial data is omitted, moved into error details,
  or emitted as a success envelope with a non-zero exit code.
- `[I4]` Inference from `[U4]` and OpenVPN behavior: an OpenVPN profile path is
  configuration for starting a tunnel, not proof that the tunnel is currently
  connected. Runtime readiness should therefore use a fast probe such as TCP to
  the GitLab host or a user-supplied command, not profile-path existence alone.

## Confirmed Facts

- Mixed-provider `forge-cli inbox` selects GitHub and GitLab by default. `[F1]`
- GitHub and GitLab provider plans are already started concurrently. `[F2]`
- The final output remains a single envelope, so the command still waits for
  all selected providers to finish or fail before printing results. `[F3]`
- GitLab failure is not currently hidden when GitHub succeeds: the envelope
  includes a failed GitLab provider row and warning while returning GitHub
  items. `[F4]`
- There is no current subprocess-level timeout in the common backend runner.
  `[F5]`
- The current local machine can install missing OpenVPN CLI support through
  Homebrew if implementation or smoke validation needs it. `[A1][U5]`

## Decisions

- Preserve the single-envelope JSON/text contract for default `inbox` output.
  Do not introduce streaming JSON or NDJSON in this plan; that would be a new
  consumer contract and is unnecessary for the immediate VPN failure mode.
- Add a VPN readiness gate before backend timeout. When GitLab is selected,
  configured as VPN-required, and the fast readiness probe says VPN is down,
  skip `glab` and report a provider-local `vpn_unavailable` failure.
- Support OpenVPN as the first concrete VPN provider metadata, but do not make
  `forge-cli inbox` start OpenVPN. The runtime check should be a fast, bounded
  readiness probe such as `tcp:<host>:<port>` or `cmd:<script>`.
- Treat local VPN profile paths as sensitive local configuration. They may be
  supplied through env/config/flag for diagnostics or helper scripts, but they
  must be redacted from issue comments, JSON output, warnings, logs, and
  committed docs.
- Add bounded timeout behavior at the subprocess/backend layer so a hung `glab`
  child can be killed. Provider-thread timeout alone is insufficient because it
  would leave an uncancelled child process running.
- Keep default mixed-provider behavior as partial success: if GitHub succeeds
  and GitLab times out, print GitHub results, mark GitLab failed, and attach a
  timeout warning.
- Add a strict mode for automation. Strict mode may discard the successful item
  payload and return a failure envelope with provider failure details, but that
  behavior must be explicit and tested.
- Add stale-cache support only as an opt-in fallback. Cached GitLab items must
  never make a timed-out provider look healthy, and output must expose cache
  age/staleness.

## Requirements

- Mixed-provider inbox calls must not wait indefinitely for an unreachable
  GitLab host.
- If GitLab is configured as VPN-required, mixed-provider calls should first run
  a bounded readiness probe and skip GitLab quickly when VPN is unavailable.
- VPN readiness failures must use a distinct machine-readable error kind, such
  as `vpn_unavailable`, and indicate that the backend was not attempted.
- Timeout failures must use a distinct machine-readable error kind, such as
  `backend_timeout` or `provider_timeout`, rather than being flattened into
  generic `backend_error`.
- Missing OpenVPN CLI support must not be flattened into GitLab backend failure.
  It should be reported as a VPN probe dependency problem with an install hint,
  while preserving default partial-success behavior when GitHub succeeds.
- Text output must still show successful provider items and a clear warning for
  timed-out providers.
- JSON output must preserve provider status rows so Alfred, schedulers, and
  agents can distinguish healthy, failed, timed-out, strict-failed, and
  stale-cache states.
- `--provider gitlab` should fail non-zero when GitLab times out, because no
  selected provider succeeded.
- Default mixed-provider mode should remain non-zero only when all selected
  providers fail, unless strict mode is explicitly requested.
- Tests must be offline and deterministic; they should use stubbed `gh`/`glab`
  binaries and short synthetic sleeps rather than live GitHub or GitLab access.
- Tests and fixtures must not contain private local OpenVPN profile paths.

## Non-Goals

- Do not broaden inbox coverage beyond the existing personal-work query model.
- Do not add direct GitHub/GitLab REST clients or token storage.
- Do not start or stop the VPN from `forge-cli inbox`.
- Do not persist or print local OpenVPN profile paths.
- Do not mutate GitLab todos, MRs, issues, or GitHub PRs/issues.
- Do not add a streaming output mode in this plan.
- Do not treat stale cache as successful live data.

## Open Questions

1. Final flag spelling for VPN controls: `--gitlab-vpn`,
   `--gitlab-vpn-check`, `--vpn-check-timeout`, and profile/path flags.
2. Final flag spelling: `--gitlab-timeout`, `--provider-timeout`, or both.
3. Default timeout values for VPN readiness and GitLab backend calls in
   mixed-provider mode.
4. Strict-mode envelope shape when some providers succeeded and others failed.
5. Whether stale cache should be stored under `XDG_CACHE_HOME` or an existing
   nils-cli state/cache convention.

## Execution

- Recommended plan: docs/plans/forge-cli-inbox-provider-timeouts/forge-cli-inbox-provider-timeouts-plan.md
- Recommended execution state: docs/plans/forge-cli-inbox-provider-timeouts/forge-cli-inbox-provider-timeouts-execution-state.md

## Retention Intent

This source document is execution coordination for a `forge-cli inbox`
resilience follow-up. It can be cleaned up after the tracking issue or plan is
closed, unless the timeout/strict/cache decisions are promoted into the
`forge-cli` docs as lasting provider-behavior guidance.

## Read First References

- `docs/plans/forge-cli-inbox-latency/forge-cli-inbox-latency-plan.md`
- `crates/forge-cli/src/ops/inbox.rs`
- `crates/forge-cli/src/backend.rs`
- `crates/forge-cli/src/cli.rs`
- `crates/nils-common/src/cli_contract.rs`
- `crates/forge-cli/tests/integration/inbox.rs`
- `crates/forge-cli/docs/specs/forge-cli-spec-v1.md`
