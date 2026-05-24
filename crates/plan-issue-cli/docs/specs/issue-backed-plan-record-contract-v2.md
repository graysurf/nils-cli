# Issue-Backed Plan Record Contract v2

## Status

- This spec defines the breaking v3 issue-backed plan record lifecycle owned by
  the `plan-issue record ...` surface.
- It replaces [issue-backed plan record contract v1](issue-backed-plan-record-contract-v1.md).
- It supersedes the prior `start-plan` / `start-sprint` Task Decomposition
  runtime for new agent-runtime-kit lifecycles; see
  [plan-issue state machine v2](plan-issue-state-machine-v2.md).

The spec is normative for v3 implementations across Sprints 1-5 of the
`plan-issue-lifecycle-v3` plan.

## Scope

`plan-issue record` is now the high-level **owner** of issue-backed plan
record lifecycle. It opens, posts to, audits, repairs, and closes provider
issues that carry a plan execution timeline through append-only comments.

In scope:

- One canonical marker family for source, plan, state, session, validation,
  review, and closeout comments.
- Structured payload model embedded in lifecycle comments so audit and
  closeout never parse prose Markdown for state.
- High-level live commands `record open`, `record post`, `record audit`,
  `record repair-dashboard`, and `record close` that perform provider issue
  mutations (create, comment, edit, close) without composing `forge-cli` at
  the skill level.
- Strict closeout gate with provider-verified linked PR evidence.
- Deterministic fixture mode for tests.

Out of scope:

- Plan parsing, validation, and PR-split modeling (owned by `plan-tooling`).
- General provider lifecycle outside of plan-issue records (still owned by
  `forge-cli` / provider adapters).
- Migrating consumer skills until a `plan-issue` release containing this
  contract is published.

## Breaking Changes vs v1

The following v1 surfaces are retired in v2:

- `--marker-family` argument and the dual `compat` / `shared` marker
  families.
- `record render-comment` and `record render-dashboard` as primary
  user-facing commands. Comment rendering becomes an implementation detail of
  `record post`; dashboard rendering moves under `record repair-dashboard`.
- `record closeout-gate` as a standalone command and its
  `--require-complete`, `--require-session`, `--require-validation`,
  `--require-review`, and `--require-closeout` flags. Closeout-gate
  evaluation moves inside `record close` and is strict by default.
- Implicit acceptance of v1 `plan-tracking-issue:*`,
  `execute-from-tracking-issue:*`, `tracking-issue-closeout:*`, and
  `deliver-dispatch-plan:*` markers as current lifecycle evidence. They are
  ignored or reported as unsupported.
- Prose-Markdown status parsing (`- Status: complete`) as the authoritative
  signal for closeout state detection. The structured payload is the source
  of truth.

## Canonical Marker Family

Every lifecycle comment opens with a single HTML-comment marker on its first
non-empty line:

```text
<!-- plan-issue-record:v2 role=<role> profile=<profile> -->
```

Recognized roles:

| Role         | Required (tracking) | Required (dispatch) | Purpose                                                  |
| ------------ | ------------------- | ------------------- | -------------------------------------------------------- |
| `source`     | yes                 | yes                 | Source snapshot (discussion / review document).          |
| `plan`       | yes                 | yes                 | Plan document snapshot.                                  |
| `state`      | yes                 | yes                 | Latest execution state (status, tasks, prs, updated_at). |
| `session`    | optional            | yes                 | Implementation session log entry.                        |
| `validation` | yes for closeout    | yes for closeout    | Validation command rows and overall status.              |
| `review`     | yes for closeout    | yes for closeout    | Specialist review findings and decision.                 |
| `closeout`   | yes for closeout    | yes for closeout    | Final closeout summary, PR merge evidence, approval.     |

Recognized profiles: `tracking`, `dispatch`.

Marker detection only accepts markers on the first non-empty line. Markers
quoted inside snapshot bodies or fenced code blocks are ignored. v1 marker
families are not recognized as current lifecycle markers.

## Structured Payload Model

Every lifecycle comment includes one hidden HTML-comment payload carrier. The
payload is the structured source of truth for audit, dashboard repair, and
closeout gating. Visible Markdown around it is human commentary only.

Implementations recognize current payloads by reading a comment whose inner
text starts with `plan-issue-record-payload:hex:` and hex-decoding the
following JSON envelope. For backward compatibility, audit still accepts the
older visible fenced JSON block whose info-string is the literal token
`plan-issue-record-payload`, but new provider-backed comments must not render
that visible fence by default.

The envelope shape is:

```json
{
  "schema": "plan-issue-record.payload.v2",
  "role": "state",
  "profile": "tracking",
  "updated_at": "2026-05-23T08:42:11Z",
  "data": { }
}
```

Per-role `data` shapes:

### `source` / `plan`

```json
{
  "path": "docs/plans/<slug>/<file>.md",
  "commit": "<full-sha>",
  "title": "<optional>",
  "summary": "<optional one-line summary>"
}
```

### `state`

```json
{
  "status": "in-progress|complete|blocked",
  "target_scope": "<text>",
  "current": "<text>",
  "next_action": "<text>",
  "tasks": [
    {"id": "1.1", "status": "pending|in-progress|done|deferred", "title": "<text>"}
  ],
  "prs": [
    {"ref": "owner/repo#123", "url": "<url>", "status": "open|merged|closed"}
  ],
  "blockers": ["<text>"],
  "links": {
    "source": "<url>",
    "plan": "<url>",
    "previous_state": "<url>"
  }
}
```

### `session`

```json
{
  "summary": "<one-line>",
  "highlights": ["<text>"],
  "links": {"state": "<url>", "plan": "<url>"}
}
```

### `validation`

```json
{
  "overall": "pass|fail|partial",
  "commands": [
    {"command": "<exact-command>", "status": "pass|fail|skipped", "evidence": "<optional-url-or-path>"}
  ],
  "waivers": [{"command": "<text>", "reason": "<text>"}]
}
```

### `review`

```json
{
  "decision": "approve|request-changes|comments-only",
  "lenses": ["testing", "maintainability", "..."],
  "findings": [
    {"id": "F1", "severity": "blocker|major|minor|nit", "disposition": "fixed|residual|follow-up|deferred|no-action", "summary": "<text>"}
  ],
  "outcome_comment_url": "<url>"
}
```

### `closeout`

```json
{
  "final_status": "complete",
  "approval": {"comment_url": "<url>", "approver": "<login>"},
  "linked_prs": [
    {"ref": "owner/repo#123", "url": "<url>", "merge_sha": "<sha>", "checks": "pass|fail|none"}
  ],
  "final_validation_url": "<optional-url>",
  "notes": "<optional>"
}
```

The schema field name `plan-issue-record.payload.v2` is the on-wire schema
identity. Audit logic must reject unknown schema names rather than guess.

## Command Boundary

- `plan-tooling`: plan parsing, validation, sprint and task split modeling.
- `plan-issue record`: issue-backed plan record lifecycle, structured payload
  rendering, audit, dashboard repair, closeout gating, and provider-backed
  open / post / close.
- `forge-cli`: general provider issue and PR operations that are not part of
  the issue-backed plan record lifecycle (lane PRs, ad-hoc issues, comments
  outside this lifecycle).
- `review-evidence` / `code-review-specialists`: retained review records and
  read-only specialist passes consumed by `record post --kind review`.

## Provider-Backed Command Surface

### `plan-issue record open`

Bundle-first command that:

1. Loads a plan bundle (source, plan, execution-state) from
   `--bundle <dir>` or explicit `--source-file` / `--plan-file` /
   `--execution-state-file` paths.
2. Validates the plan bundle via `plan-tooling validate`.
3. Verifies local source/plan files are committed and clean unless
   `--allow-dirty` is passed (live mode only).
4. Creates the provider issue with the dashboard rendered from the initial
   execution state (issue body).
5. Posts the source, plan, and initial state comments with canonical markers
   and structured payloads.
6. Repairs the dashboard with the freshly-created comment URLs.
7. Returns issue URL and source/plan/state comment URLs.

Modes:

- `--dry-run`: render every provider mutation plan without writing.
- `--fixture <dir>`: deterministic mode for tests; produces the same JSON
  result shape using local files instead of provider calls.

Required inputs in live mode: `--repo OWNER/REPO`, `--bundle <dir>` (or
explicit file flags), and a title source (`--title` or plan-derived).

### `plan-issue record post`

High-level append-only comment command for `state`, `session`, `validation`,
`review`, and `closeout` kinds.

- Renders the canonical marker, human summary Markdown when supplied, and a
  hidden structured payload from explicit fields or a `--payload-file` JSON
  document.
- Posts to the provider issue and returns the comment URL.
- Supports `--dry-run` and `--fixture` modes.
- `--kind closeout` is callable directly only when `record close` is not
  used; `record close` posts the closeout comment internally and uses the
  same renderer.

### `plan-issue record audit`

Returns typed evidence from the provider issue body and comments:

- Live mode reads through the active provider.
- `--body-file` and `--comments-json` continue to work for deterministic
  tests.
- Output JSON exposes the latest marker URL, created timestamp, profile,
  role, status, and parsed payload per role.
- Reports `missing_required` codes for each lifecycle role not satisfied.

### `plan-issue record repair-dashboard`

- Reads the latest audit evidence.
- Computes the canonical dashboard from durable record links and the latest
  state payload (no caller-supplied URL flags required).
- Live mode edits the issue body. Local mode prints or writes the rendered
  dashboard.
- A complete record renders `## Final Dashboard`; otherwise `## Current
  Dashboard`.

### `plan-issue record close`

Strict, single-command closeout:

1. Fetches issue evidence through audit.
2. Verifies presence of source, plan, state (`status=complete`), session,
   validation (`overall=pass`), and review (decision among
   `approve` / `comments-only`).
3. Verifies linked PR evidence through provider state: every linked PR is
   merged, with `merge_sha` and CI status recorded.
4. Renders and posts the `closeout` comment with structured payload.
5. Renders and edits the `## Final Dashboard` issue body.
6. Closes the provider issue.

`record close` removes all `--require-*` flags. The strict gate is
non-optional. Inputs are limited to:

- `--issue <number-or-url>` and `--repo OWNER/REPO` (live mode).
- `--linked-pr <ref>` (repeatable; cross-checked against state payload).
- `--approval <url-or-text>`.
- `--bundle <dir>` (optional; used to validate that the closed plan still
  matches local plan content).
- `--fixture <dir>` + `--body-file` + `--comments-json` for tests.
- `--dry-run` for non-mutating previews.

## Strict Closeout Gate

Failure modes that block close:

- `state-missing` / `state-not-complete` / `state-stale` (state payload
  `updated_at` older than the most recent session entry without a session
  update).
- `validation-missing` / `validation-failed` / `validation-stale`.
- `review-missing` / `review-rejected` / `review-unresolved-findings`
  (any finding with disposition `residual` and severity `blocker` /
  `major`).
- `linked-pr-missing` / `linked-pr-not-merged` / `linked-pr-checks-failed`.
- `approval-missing` / `approval-invalid`.
- `dashboard-out-of-date` (recomputed dashboard differs from issue body).

Each failure returns a stable machine-readable code that maps to a single
unblock action.

## Provider Verification

Linked PR evidence is verified against the provider, not text matching:

- Each linked PR ref resolves to a provider PR.
- Provider state must be `merged`.
- Provider check rollup must be `success` or explicitly waived in the
  closeout payload `notes` (with reviewer approval recorded).
- The provider's `merge_commit_sha` is recorded back into the closeout
  payload `linked_prs[].merge_sha`.

## Result JSON Envelope

Every `record` subcommand emits the shared CLI JSON envelope on `--format
json`:

```json
{
  "schema_version": "plan-issue-cli.record.<subcommand>.v2",
  "command": "record.<subcommand>",
  "status": "ok|error",
  "payload": { ... }
}
```

Examples of `payload`:

- `record open` -> `{ "issue": {...}, "comments": {"source": "<url>", "plan": "<url>", "state": "<url>"} }`
- `record close` -> `{ "issue": {...}, "closeout_url": "<url>", "final_dashboard": {...}, "linked_prs": [...] }`

## Compatibility Layer

There is none. Consumers must migrate from v1 markers and v1 command flags
in a coordinated release of the consumer (agent-runtime-kit) after the
`plan-issue` v3 release ships.

## Consumer Migration

The agent-runtime-kit dispatch skills are the primary downstream consumer
of this contract. They currently call `plan-issue` 0.17.x with
`--marker-family compat` and assemble closeout from `record render-comment`,
`record render-dashboard`, and `record closeout-gate` by hand. The
migration is a one-time replacement of those invocations with the new
provider-backed surface.

### Before (v1, retired)

```bash
# Open a tracking issue and seed snapshots manually.
plan-issue record render-comment \
  --marker-family compat --kind source --content-file source.md --commit "$SOURCE_SHA" \
  --out source.md
forge-cli --provider github issue create --repo "$REPO" --title "$TITLE" --body-file source.md

# Post lifecycle comments manually.
plan-issue record render-comment --marker-family compat --kind state \
  --content-file state.md --out state.md
forge-cli --provider github issue comment "$ISSUE" --repo "$REPO" --body-file state.md

# Evaluate closeout gate explicitly.
plan-issue record closeout-gate \
  --body-file issue-body.md --comments-json comments.json \
  --require-complete --require-validation --require-review --require-closeout \
  --approval "$APPROVAL_URL"

# Close issue manually.
forge-cli --provider github issue close "$ISSUE" --repo "$REPO" --reason completed
```

### After (v2, current)

```bash
# Open the tracking issue from a plan bundle.
plan-issue --repo "$REPO" record open --bundle docs/plans/<slug>

# Post one lifecycle comment.
plan-issue --repo "$REPO" record post \
  --issue "$ISSUE" --kind state --payload-file state.json

# Refresh the dashboard from audit (no caller-supplied URLs).
plan-issue --repo "$REPO" record repair-dashboard --issue "$ISSUE"

# Strict closeout: audit, gate, comment, repair, close in one call.
plan-issue --repo "$REPO" record close \
  --issue "$ISSUE" \
  --linked-pr "$REPO#$PR_NUMBER" \
  --approval "$APPROVAL_URL"
```

### Migration checklist

- [ ] Replace any `--marker-family compat` callsites with v2 invocations.
- [ ] Replace `record render-comment | render-dashboard | closeout-gate`
      composition with `record open | post | repair-dashboard | close`.
- [ ] Switch JSON consumers from `audit.markers` (v1) to `audit.evidence`
      keyed by role (v2).
- [ ] Re-pin the JSON envelope on `plan-issue-cli.record.<sub>.v2` before
      reading the new top-level fields (`issue.url`, `comments.*`,
      `closeout_url`, `final_dashboard`).
- [ ] After upgrading, re-post v2-marker `source`, `plan`, and `state`
      comments on any in-flight tracking issue so audit can find them
      (Sprint 1 + 2 lifecycle comments on existing issues remain v1 and
      audit treats them as `unsupported_markers`).

## Test Fixtures

Live commands accept a `--fixture <dir>` argument. The directory contains:

- `issue-body.md`: provider issue body Markdown.
- `comments.json`: provider comments JSON in `gh issue view --json comments`
  shape.
- `prs/<owner>__<repo>__<number>.json`: provider PR snapshots for
  linked-PR verification.

Fixture mode never reaches the network. It produces the same result-JSON
shape as live mode so that consumer-side tests are deterministic.

The Sprint 4 deliverable adds an agent-runtime-kit closeout fixture that
exercises the strict close path end to end.
