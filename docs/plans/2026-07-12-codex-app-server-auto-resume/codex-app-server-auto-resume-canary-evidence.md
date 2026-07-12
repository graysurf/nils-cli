# Codex app-server auto-resume canary evidence

## Scope and privacy

The final acceptance run used installed Codex CLI 0.144.1, the repaired
`agent-session` candidate, two isolated app-server-backed sessions on the
authorized `sym` account, a dedicated tmux server, and private Unix sockets.
Retained evidence contains only protocol enums, booleans, counts, timestamps,
reset-credit counts, and projected runtime state. It excludes account
identifiers, auth material, prompts other than the product-owned fixed
continuation, assistant output, human error text, provider-issued account
identifiers, raw thread/turn ids, socket paths, and terminal transcripts.

## Final transparent-bridge acceptance

| Requirement | Result | Evidence |
| --- | --- | --- |
| Capability-gated app-server runtime | pass | Both `sym` sessions reported auto-resume supported after the bounded 0.144.1 version/help probe. The app-server and TUI-bridge sockets were mode `0600`; the 64-byte SHA-256 thread binding was mode `0600`; no raw handoff file existed. |
| Exact TUI-connection quota failure | pass | A real target turn emitted non-retrying `usageLimitExceeded` followed by matching terminal `failed` on the exact bridged TUI connection. The metadata-only journal retained `turn_failed`, `usage_exhausted`, authoritative confidence, stable v1 `provider_hook`, and Codex provider; activity became `waiting` at revision 2. |
| Same-account sibling isolation | pass | The target alone became `scheduled`. The independent `sym` reset-control session remained auto-resume disabled and was never armed; the earlier pre-review run also kept an enabled sibling unscheduled. |
| Reset recheck | pass | One user-authorized `sym` reset credit changed the available count from 2 to 1. After daemon restart, the bound control connection observed open usage and advanced the retained schedule from `2026-07-12T20:02:06Z` to `2026-07-12T17:47:45Z`. |
| Same-thread acknowledged continuation | pass | The original blocked revision and hashed thread binding remained unchanged; the control connection resumed the bound thread, received a `turn/start` acknowledgement, and durable auto-resume state became `resumed`. |
| Exactly once | pass | Durable state remained `resumed`, and the fixed product continuation appeared exactly once in the target TUI scrollback. |
| Daemon/process topology | pass | Stopping the daemon left both TUI and bridge alive. The restarted daemon reconnected without recreating the runtime. |
| Cleanup and privacy | pass | API deletion killed both tmux sessions and removed both app-server sockets, both bridge sockets, and all marker paths. The isolated `sym` auth home, dedicated tmux server, and stale canary processes were removed. |
| Human-text independence | pass | Classification and scheduling used only structured protocol values; terminal error text was neither parsed nor retained. |

## Topology decision evidence

The pre-review implementation used a second control connection as a passive
monitor. A dedicated isolated `sym` smoke proved that this connection could
bind and submit its own turn, but did not receive lifecycle notifications for a
turn initiated by the visible TUI. This corrected the earlier inference from a
control-owned turn and made the passive topology non-authoritative.

The final runtime therefore keeps a transparent private WebSocket bridge in the
tmux process tree. It forwards the TUI frames unchanged while reducing only the
exact structured thread/turn failure metadata in bounded background memory.
Projection loss disables an existing claim without disconnecting the TUI, and
direct TUI thread/turn creation cancels a pending claim before forwarding. The
separate daemon connection is retained for authoritative rate-limit reads,
reconnect, and acknowledged continuation submission. Daemon restart does not
interrupt the bridge or visible TUI.

An initial topology harness accidentally reused the host's existing tmux server
and therefore did not inherit the isolated `CODEX_HOME`. It was deleted before
any prompt, model turn, quota use, or reset. The retained topology runs used a
dedicated tmux server whose global environment was verified to point at the
isolated `sym` home.

After the final projection/backpressure repair, a no-prompt `sym` smoke again
created a supported remote TUI through mode-`0600` app-server and bridge
sockets, produced the mode-`0600` 64-byte thread binding, retained no handoff,
and deleted every runtime path. It used no model turn and no reset credit.

## Superseded pre-review canary

The initial pre-review canary used isolated `poies`, captured the same exact
structured quota failure, kept an enabled sibling unscheduled, and spent one
authorized reset credit. Opening the reset menu through the target correctly
triggered the existing manual-input cancellation invariant. A removed one-shot
test harness restored only that already-proven scheduled claim, after which the
production candidate rechecked usage and resumed exactly once. This historical
detour is retained for traceability but is not the basis of the final bridge
acceptance above.

## Account constraint

The final rerun used only `sym` and spent one of its two visible reset credits.
Any future quota or reset validation must use `sym`; never use `gamania`.
