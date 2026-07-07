# Source: Claude usage fallback for agent-console

## Origin

The user wants agent-console's Claude usage panel to behave more like CodexBar:
open a tracking issue first, inventory directly shippable approaches, implement
the selected path, deliver PRs, deploy the result, and leave the service ready
for validation.

## Current behavior

`agent-console` does not read Claude credentials in the browser edge. The edge
proxies `/api/usage` to `sympoies-infra`'s host-local
`agent-console-usage.service`, which currently shells out to
`claude-cli prompt-segment` so only one host caller refreshes the Anthropic OAuth
usage endpoint and writes the shared `claude-prompt-segment/usage.json` cache.

This is the right boundary, but `claude-cli prompt-segment` currently has one
live source: `GET https://api.anthropic.com/api/oauth/usage` using a Claude Code
OAuth access token. When the endpoint fails, the token is expired, the endpoint
429s, or no successful cache exists, the host reader can only show a stale or
tier-only note. The user-visible symptom is that usage appears to require
starting or using Claude Code to refresh credentials or restore a usable cache.

## Implementable method inventory

### Method A: Keep the current OAuth-only path and extend stale cache behavior

- Implementation: increase stale-cache retention and improve notes in
  `sympoies-infra` while leaving `claude-cli` as-is.
- Pros: smallest change; no new parser; low operational risk.
- Cons: does not add any new source of truth. If OAuth fetch never succeeds,
  agent-console still cannot recover.
- Decision: not sufficient by itself; keep as fallback behavior only.

### Method B: Add `claude-cli usage --format json --source auto`

- Implementation: add a service-consumable `usage` command in `nils-cli` that
  owns Claude usage source selection and returns a versioned, secret-free JSON
  envelope. Source order: cached OAuth/API result when fresh, OAuth API refresh,
  CLI `/usage` PTY fallback, then stale cache.
- Pros: correct layering. Claude-specific auth, endpoint, PTY parsing, and cache
  policy live in `claude-cli`; `sympoies-infra` remains a thin host reader. The
  command can be unit/integration tested and reused by prompt, starship, and
  agent-console.
- Cons: requires new PTY subprocess handling and parser coverage.
- Decision: selected.

### Method C: Implement CLI PTY parsing directly in `sympoies-infra`

- Implementation: have `agent_console_usage.py` spawn `claude`, send `/usage`,
  parse TUI text, and map it to the existing panel contract.
- Pros: fast to patch for this one deployment.
- Cons: wrong ownership. It duplicates Claude-specific logic in infra, makes the
  Python reader harder to test, and cannot be reused by other local consumers.
- Decision: reject except as an emergency hotfix.

### Method D: Add Claude web-cookie fallback

- Implementation: read/import claude.ai browser cookies and call web endpoints
  when OAuth/CLI fail.
- Pros: matches one CodexBar fallback class.
- Cons: browser-cookie access is OS/browser-specific, harder to keep secret-safe
  on a headless host, and higher maintenance risk than CLI PTY.
- Decision: defer; not needed for the first shippable improvement.

### Method E: Use Anthropic Admin Usage & Cost API

- Implementation: configure an Anthropic Admin API key and read org usage/cost.
- Pros: official for API/organization reporting.
- Cons: not the Claude Code subscription quota path and unavailable for
  individual accounts; it does not answer 5h/weekly Claude Code usage windows.
- Decision: reject for this problem.

### Method F: Parse local Claude Code JSONL logs

- Implementation: scan `~/.claude/projects/**/*.jsonl` and estimate cost/token
  history.
- Pros: useful for historical cost and does not need live network.
- Cons: logs do not provide authoritative current 5h/weekly subscription quota
  resets.
- Decision: out of scope for quota; may become a separate cost-history feature.

## Selected design

Add a native `claude-cli usage` surface:

- Text output may stay minimal, but JSON output is the service contract.
- JSON includes `schema_version`, `ok`, `source`, `stale`, `windows`,
  `updated_at`, optional `plan`, and optional `note`/`error`, with no tokens,
  emails, or credential material.
- `source=auto` selects OAuth API first, then CLI PTY fallback, then stale cache.
- `source=oauth` and `source=cli` allow focused debugging/tests.
- `claude-cli` writes only successful OAuth or CLI-derived usage payloads to the
  existing shared cache file so `sympoies-infra`, prompt rendering, and future
  consumers share one last-good store.
- `sympoies-infra` switches `agent-console-usage` from invoking
  `claude-cli prompt-segment` and parsing the shared raw cache to invoking
  `claude-cli usage --format json --source auto` and mapping the returned
  windows to the existing `/api/usage` contract.

## Acceptance

- `claude-cli` tests prove that when OAuth refresh fails and a fake `claude`
  binary prints a `/usage` panel, `claude-cli usage --format json --source auto`
  returns 5h + weekly windows and writes a cache.
- `claude-cli` tests prove no token value leaks to stdout/stderr/JSON/cache
  metadata.
- Existing `prompt-segment` behavior remains compatible with the shared cache.
- `sympoies-infra` tests prove the reader consumes the new JSON command, degrades
  to stale notes when unavailable, and still never emits token material.
- Local validation passes in both repos.
- PRs are delivered and merged.
- The released/installed `claude-cli` on sympoies is updated, the
  `agent-console-usage.service` is restarted, and `/usage` returns Claude windows
  or a clear stale note without crashing.

## Execution

- Recommended plan:
  `docs/plans/2026-07-07-claude-usage-fallback/claude-usage-fallback-plan.md`
- Recommended execution state:
  `docs/plans/2026-07-07-claude-usage-fallback/claude-usage-fallback-execution-state.md`
