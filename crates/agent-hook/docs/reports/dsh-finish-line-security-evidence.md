# DSH Finish-Line Security Evidence

## Scope

- Date: 2026-08-18
- Component: `agent-hook finish-line`
- Product: DSH
- Evidence status: the real-host Linux path and focused contract gates pass.
  Host-unavailable, untrusted-binary, and forced non-quiescent branches are
  source-enforced but do not have deterministic local fault-injection coverage;
  platform-specific coverage is distinguished below.

## Contract changes

| Risk | Enforced contract | Regression evidence |
| --- | --- | --- |
| A caller could fabricate a successful outcome or discover cleanup as a public authority | Public help and completion expose only `open`, `begin`, `run`, `stop`, and `status`. `run` rejects unknown result fields and derives evidence from the process nils launches and observes. Hidden `quiesce` can only clear an exactly bound pending operation after containment cleanup; it cannot report an outcome. There is no completion, waiver, approval, or revocation authority. | Locally exercised: `nils_executes_validation_and_removes_caller_reported_outcomes_and_waivers`; `strict_run_schema_and_timeout_bounds_fail_closed`; CLI help/completion contract |
| A wrong or cross-session bearer could use hidden cleanup to kill another live unit or clear its pending record | `quiesce` authenticates the exact session-bound runner capability and operation binding before touching the unit or state. Rejection leaves both the live unit and pending record unchanged; only the correct capability may perform cleanup. | `quiesce_rejects_wrong_and_cross_session_capabilities_without_touching_the_unit` |
| The plugin could bypass classification or submit a background shell | The DSH integration probes every foreground Bash without `execution`; exact targets return `ready` and all others return `ordinary-ready`. Background Bash is rejected at the plugin boundary before finish-line invocation. The public nils request has no caller-controlled foreground/background or completion assertion. | `probe_is_non_executing_and_sandbox_runner_failure_is_not_validation_evidence`; `non_contract_foreground_shell_is_supervised_once_and_invalidates_validation_evidence`; DSH plugin integration acceptance outside this crate |
| A provider wrapper could substitute a different command or manufacture evidence before validation ran | A confined argv must end with the exact `-- bash -c <contract-command>` tuple. Runner failures classified before authoritative execution retain their pending cleanup binding and remain missing evidence. | `nils_executes_the_provider_confined_argv_and_rejects_command_substitution`; `probe_is_non_executing_and_sandbox_runner_failure_is_not_validation_evidence`; `failed_contained_runner_stays_pending_until_authenticated_quiescence` |
| A mutable path could replace the runner or alter its config between validation and launch | Config is an anonymous sealed memfd. Systemd `OpenFile` passes the exact current runner inode, sealed config, and control memfd as named descriptors 3, 4, and 5. A verified root-owned ELF interpreter loads `/proc/self/fd/3`; the contained runner validates the descriptor names/count plus config metadata and seals before parsing. | Source-enforced and exercised by the real-host Linux suite's successful launches. No deterministic runner/config descriptor-tamper negative test is claimed. |
| A provider could forge terminal facts or collide with an internal exit/signal sentinel | The config binds a random control nonce. Before provider spawn, the runner validates FD5, sets `FD_CLOEXEC`, becomes non-dumpable, and emits `ready`. The supervisor then closes config, changes control mode to `0400`, drops its writable view, becomes non-dumpable, and acknowledges; only that exact handshake permits provider spawn. The runner clears the acknowledgement, publishes one strict nonce-bound terminal record, and seals the memfd itself before exit; the supervisor accepts only the sealed record after unit/cgroup quiescence. No provider exit or signal value is reserved for control. | `provider_cannot_reopen_finish_line_memfds_through_procfs`; `provider_exit_and_signal_facts_do_not_collide_with_runner_control`; `timeout_kills_the_validation_cgroup_and_records_failure` |
| A failed, denied, timed-out, or concurrently stale execution could satisfy stop | Nils normalizes only the observed exit/signal/timeout state, records sandbox denial facts as failure, kills the transient validation cgroup on timeout, waits for a quiescent unit, and prevents a result made stale by a later edit from satisfying the new generation. | `confined_denial_is_recorded_as_a_failed_validation_with_provider_facts`; `observed_exit_and_output_drive_failure_then_exact_retry_success`; `timeout_kills_the_validation_cgroup_and_records_failure`; `edit_during_validation_makes_the_observed_success_stale` |
| An ordinary foreground shell could run outside generation accounting or manufacture validation evidence | Nils advances repository generation before launching an ordinary command, returns `ordinary-applied` with observed execution facts, and never creates target validation evidence. A terminal retry of the same binding returns the durable result without re-execution or output replay. | `non_contract_foreground_shell_is_supervised_once_and_invalidates_validation_evidence`; `exact_duplicate_is_durable_and_never_reexecutes_the_command` |
| A descendant could outlive the response and mutate later | Linux execution is placed in a transient user cgroup with `KillMode=control-group`, immediate `SIGKILL`, `RuntimeMaxSec`, and nils-controlled unit teardown/status inspection. Same-group detached descendants and `setsid`/double-fork descendants are covered by the unit rather than only the launcher process group. | `ordinary_shell_cannot_leave_a_same_group_descendant_to_mutate_after_return`; `ordinary_shell_cannot_escape_with_a_new_session_or_double_fork_after_return`; `timeout_kills_the_validation_cgroup_and_records_failure` |
| Supervisor `SIGKILL` or an execution error could strand a running unit or lose its cleanup handle | The contained runner watches the supervisor through pidfd and kills its workload when that exact process disappears. Errors retain pending `active_unit`. Hidden `quiesce` performs bounded stop/status plus exact `list-jobs` barriers and requires three consecutive no-job/quiescent observations before removing pending state; a later active unit or pending job resets that count. | Locally exercised: `quiesce_recovers_a_killed_supervisor_and_prevents_late_mutation`; `quiesce_stabilizes_a_unit_cancelled_during_submission`; `failed_contained_runner_stays_pending_until_authenticated_quiescence`. Deterministic unit evidence: `stable_unit_wait_rejects_initial_absence_and_resets_on_late_appearance`. Forced `systemctl` timeout and forged non-quiescent responses remain source-enforced branches without deterministic local injection. |
| A command could escape the unit through the user manager or parent cgroup | The unit uses `PrivateUsers=yes`, excludes `AF_UNIX`, denies localhost, and sets `Delegate=no`. The regressions prove the exercised user-manager delegation and parent-cgroup migration routes fail. | `ordinary_shell_cannot_delegate_a_late_mutation_to_the_user_manager`; `contained_shell_cannot_migrate_itself_to_the_parent_cgroup` |
| An unsupported host or forged systemd tool could provide weak containment | `open` is Linux-only and validates fixed trusted systemd binaries, cgroup v2, user namespaces, and the user manager before state activation. Execution and cleanup fail closed on missing, untrusted, or non-quiescent boundaries. | Source-enforced. `finish_line_non_linux` contains the platform-gated regression but runs zero tests on this Linux host; non-Linux CI must execute it. Missing/untrusted/non-quiescent Linux fault branches are not claimed as locally exercised. |
| Evidence could cross session, generation, or contract boundaries | Target evidence binds the authoritative repository, DSH session, shared repository generation, contract digest, and exact target digest. Contract drift, a different session, and an edit by another session block prior evidence. | `evidence_is_session_scoped_and_contract_drift_invalidates_it`; `an_edit_in_another_session_invalidates_prior_repository_generation_evidence` |
| Durable edit history could exhaust the operation bound | Completed edit reservations are terminal records and compact by deterministic sequence at the same bounded trigger as completed validations. | `durable_edit_generation_records_compact_instead_of_exhausting_the_state_limit` |
| An ambiguous open, crash, resume, or hostile second open could exhaust state, take over a live session, resurrect a retired bearer, or let an old release delete a new incarnation | `open` derives the bearer from caller-held unpredictable retry material plus a persisted monotonic incarnation sequence. Exact live replay returns the same capability and renews a 24-hour lease; a different attempt cannot overwrite a live digest. Authenticated release retains a bounded duplicate-release tombstone keyed to the exact capability incarnation. Attempt tokens are idempotency material, not authorization: after release, even reuse of the old token advances the durable incarnation sequence and returns a byte-distinct capability, while the retired bearer cannot release or run against that live session. Unrelated release churn may evict the old duplicate receipt but cannot roll back the sequence. A stable rc.7 session identity may therefore reopen without an unbounded revocation set or probabilistic denial. Lease expiry alone removes nothing. Only at live-session capacity may one expired quiescent session be retired; a persisted cursor rotates the bounded eight-candidate window, and a busy crash orphan additionally requires exact unit-bound state rechecked around trusted stable stop/status, `list-jobs`, and cgroup quiescence proof. Active, indeterminate, unbound, oversized, and migrated no-lease sessions remain protected. | `open_is_retry_safe_and_cannot_take_over_a_live_session`; `tombstone_churn_never_resurrects_a_released_capability`; `expired_quiescent_sessions_are_reclaimed_but_pending_sessions_remain_protected`; `expired_crash_orphans_reclaim_only_after_trusted_unit_quiescence`; `released_sessions_do_not_exhaust_the_repository_session_limit`; `release_is_authenticated_and_cannot_reclaim_a_pending_session`; `expired_reclaim_window_rotates_past_a_busy_oldest_window` |
| Loss of an initialized repository state record could reset enforcement to generation zero | The persistent private repository lock is an initialization anchor. Once it exists, a missing paired state record is rejected as `finish-line-state-missing` instead of creating fresh allow-state. | `deleting_initialized_repository_state_fails_closed_instead_of_resetting_generation` |
| State or inspection could leak request identity, command, or output | State files are private, owner-controlled, bounded to 384 KiB, and contain digests plus normalized execution facts rather than raw session identifiers, commands, or output. `status` returns only redacted target facts. | `state_is_private_bounded_and_contains_no_raw_command_or_identity` |

The containment claim is intentionally narrow. Local regressions cover the
real-host descendant lifetime, `setsid`, double-fork, supervisor `SIGKILL` plus
cleanup, user-manager delegation, parent-cgroup migration, timeout, and
cancellation paths. The unit is not a general network namespace and this
evidence does not claim to prevent every network or IPC delegation mechanism.

The fixed-binary trust checks, sealed-descriptor validation, pidfd-unavailable
path, bounded `systemctl` failure path, and rejection of a forged non-quiescent
unit remain fail-closed source contracts. The successful real-host suite passes
through their normal path, but it does not deterministically induce each
failure. The real-host regression cancels a supervisor after its submitting
unit identity is durable; three stable observations plus the exact `list-jobs`
barrier then prevent one transient absent-unit observation from completing
cleanup. This exercises the identified submission window, but it is not a
complete host-manager scheduling or fault matrix. The non-Linux source test is
executable evidence only when run by a non-Linux platform job.

The deterministic stabilization test scripts two absent/no-job observations,
one late active/job observation, and then three inactive/no-job observations;
acceptance occurs only after the final three, proving the late appearance resets
the count. The killed-supervisor integration test atomically publishes the
child PID before its readiness marker, so cancellation starts only after the
PID needed for the post-cleanup liveness assertion is available.

The regression suite also freezes output retention as a bounded
initial-response behavior and `stop` as a read-only decision over native
validation evidence. The runner capability remains a session-bound private
bearer by contract. No caller-reported outcome or waiver state exists.

## Validation record

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Pass |
| `cargo test -p nils-agent-hook --test finish_line` | Pass: 31 Linux tests |
| `cargo test -p nils-agent-hook --lib finish_line::tests::stable_unit_wait_rejects_initial_absence_and_resets_on_late_appearance` | Pass: deterministic stabilization reset test |
| `cargo test -p nils-agent-hook --test finish_line_non_linux` | Pass on this Linux host with 0 tests by design; non-Linux execution evidence requires platform CI |
| `cargo test -p nils-agent-hook --test contracts` | Pass: 6 tests, including CLI help/default/completion contract |
| `bash crates/agent-hook/tests/contract-freeze.test.sh` | Pass |
| `bash .agents/skills/project-verify-required-checks/scripts/project-verify-required-checks.sh --docs-only` | Pass: placement, hygiene, Markdown, plan-bundle, CLI-output, and fixture-redaction checks |
| `npx --yes rumdl@0.1.62 check --config .rumdl.toml crates/agent-hook/docs/reports/dsh-finish-line-security-evidence.md` | Pass: no issues in the new evidence file |
| `bash scripts/ci/third-party-artifacts-audit.sh --strict` | Pass: regenerated artifacts have no drift or missing files |
| `bash scripts/ci/completion-freshness-audit.sh --strict --bin agent-hook --skip-build` | Pass: bash and zsh snapshots |
| `zsh -f tests/zsh/completion.test.zsh` | Pass |
| `bash -n completions/bash/agent-hook` | Pass |
| `zsh -n completions/zsh/_agent-hook` | Pass |
| `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast` | Docs, third-party artifacts, fmt, workspace clippy, and the current finish-line tests passed. Workspace nextest failed only in 12 unrelated `agent_run_inspect` cases because Bubblewrap could not configure loopback on this host (`RTM_NEWADDR: Operation not permitted`); an immediate workspace rerun excluding that exact host-only group passed and the private-TMPDIR probe reported no leak. The current 31-test Linux suite is covered by the focused command above. |

No commit or provider mutation is part of this evidence.
