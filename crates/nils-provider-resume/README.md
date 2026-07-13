# nils-provider-resume

Shared provider session-resume resolver for the `nils-cli` workspace.

Codex and Claude Code both record the original working directory in their local
session history, so resuming a known session id only needs a bounded scan of
that history to recover the cwd. This crate owns that scan, the `session_meta` /
transcript parsing, the bounded-scan budgets, and the missing / ambiguous /
truncated outcome handling so that `agent-session`, `codex-cli`, and
`claude-cli` do not each maintain an independent copy.

It is a library-only crate. Callers keep their own user-facing error text,
exit-code mapping, and final provider command composition; this crate returns
structured results only.

It deliberately lives outside `nils-common`: the bounded scan needs monotonic
time (`Instant::now`) to cap its wall-clock cost, which the `nils-common`
render-path determinism gate forbids.

For canonical Codex UUIDv7 ids, resolution first derives the UTC
`sessions/YYYY/MM/DD` directory from the id timestamp and checks only matching
rollout filenames there. If that fast path finds no valid metadata, resolution
falls back to the bounded full-history scan so non-UUIDv7 ids and non-canonical
history layouts remain supported. A valid canonical-day match is authoritative;
the fallback does not override it. Both phases share one aggregate entry and
deadline budget.

## Surface

- `resolve_resume_source(provider, session_id) -> Result<ResolvedResume, ResumeResolveError>`
  — scan the provider's default history root (honoring `CODEX_HOME` /
  `CLAUDE_CONFIG_DIR`) and resolve the id to a single recorded cwd.
- `resolve_resume_source_in(provider, root, session_id)` — same, against an
  explicit history root.
- `normalize_resume_id(session_id) -> Result<String, ResumeIdError>` — trim and
  reject empty / control-character ids before any scan.
- `ResumeProvider` (`Codex` / `Claude`), `ResolvedResume { cwd, capture_method }`,
  `ResumeResolveError` (`NotFound` / `Ambiguous { cwd_count }` / `Truncated`).
- Lower-level primitives reused by `agent-session` (post-launch capture and
  transcript-path resolution): `codex_sessions_root`, `claude_projects_root`,
  `read_codex_session_meta` (returning `CodexSessionMeta`),
  `read_claude_session_cwd`, `collect_codex_provider_resume_matches`,
  `collect_claude_provider_resume_matches`, `ProviderHistoryMatch`,
  `CodexResumeScanBudget`, `ClaudeResumeScanBudget`,
  `CODEX_RESUME_SCAN_MAX_DEPTH`, `CLAUDE_SESSION_META_MAX_LINE_BYTES`.

## Scan tuning

The bounded scan reads these env vars (defaults chosen to stay well under a
second on typical histories):

- `AGENT_SESSION_CODEX_RESUME_SCAN_MAX_ENTRIES` (fallback:
  `AGENT_SESSION_CODEX_CAPTURE_MAX_ENTRIES`) — max Codex entries visited.
- `AGENT_SESSION_CODEX_SCAN_SLICE_MS` — Codex scan time slice.
- `AGENT_SESSION_CLAUDE_RESUME_SCAN_MAX_ENTRIES` — max Claude entries visited.
- `AGENT_SESSION_CLAUDE_RESUME_SCAN_SLICE_MS` — Claude scan time slice.

The `AGENT_SESSION_*` names are retained from the original `agent-session`
implementation so existing tuning keeps working unchanged.
