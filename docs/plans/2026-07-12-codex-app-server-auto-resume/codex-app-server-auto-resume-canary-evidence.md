# Codex app-server auto-resume canary evidence

## Scope and privacy

This acceptance run used installed Codex CLI 0.144.1, a candidate
`agent-session` daemon, two isolated app-server-backed sessions on the same
account, and private Unix sockets. Retained evidence contains only protocol
enums, booleans, counts, timestamps, reset-credit counts, and projected runtime
state. It excludes account identifiers, auth material, prompts other than the
product-owned fixed continuation, assistant output, human error text, provider-issued
account identifiers, raw
thread/turn ids, socket paths, and terminal transcripts.

## Acceptance result

| Requirement | Result | Evidence |
| --- | --- | --- |
| Capability-gated app-server runtime | pass | Both serve-managed sessions reported auto-resume supported; the handoff was consumed and the attached marker was mode `0600`. |
| Exact structured quota failure | pass | The target emitted non-retrying `usageLimitExceeded` followed by the matching terminal `failed`; durable activity became authoritative `provider_protocol` / `waiting` at revision 6. |
| Same-account sibling isolation | pass | The target became `scheduled`; the sibling remained `enabled`, had no blocked turn/revision, and was never scheduled. |
| Reset recheck | pass | One user-authorized reset credit changed the available count from 2 to 1. No second credit and no other account were used. On daemon restart, an authoritative open-usage read advanced the retained future schedule to `2026-07-12T16:12:40Z`. |
| Same-thread acknowledged continuation | pass | The original blocked turn projection and revision remained unchanged through recovery; `turn/start` returned an acknowledged turn id and durable state became `resumed`. |
| Exactly once | pass | After multiple scheduler intervals, durable state remained `resumed` and the fixed continuation appeared exactly once in the target TUI scrollback. |
| Negative control after resume | pass | The sibling still reported `enabled`, `ever_scheduled=false`, and no blocked turn/revision after the target resumed. |
| Human-text independence | pass | Classification and retained evidence used only structured protocol values; terminal error text was neither parsed nor retained. |

## Manual-reset safety interaction

The reset menu was initially opened through the target session's authenticated
send endpoint. That input correctly exercised the existing safety invariant and
cancelled the armed target with `failure_reason=manual_input`. The reset itself
had already succeeded, so the run did not spend another credit. A one-shot
ignored test harness, removed immediately after use, restored only the original
claim's `enabled/scheduled` fields while asserting that the blocked turn
projection, blocked revision, and `ever_scheduled` marker were still present.
It set a deliberately future schedule, stopped, and did not submit a turn.

The production candidate was then restarted against the unmodified runtime and
provider state. Its reconnect-time `account/rateLimits/read` observed open
usage, advanced only that existing scheduled claim, rechecked the unchanged
activity revision, claimed before submission, received the exact-thread
`turn/start` acknowledgement, and reached `resumed`. This isolates the
production path under test from the canary-only repair and preserves a truthful
record of the manual-input detour.

## Account constraint

No further quota/reset run is required by this acceptance. The user-provided
local account aliases are retained only for the rerun constraint: if a future
rerun is needed, use `sym`; do not use `gamania`.
