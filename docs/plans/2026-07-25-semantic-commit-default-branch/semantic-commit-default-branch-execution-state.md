# `semantic-commit default-branch` Local-Only Delivery — Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->

## Execution State

- Status: active
- Source document:
  `docs/plans/2026-07-25-semantic-commit-default-branch/semantic-commit-default-branch-plan.md`
- Implementation source:
  `docs/plans/2026-07-25-semantic-commit-default-branch/semantic-commit-default-branch-discussion-source.md`
- Direct source-doc execution waiver: not applicable
- Tracking issue: none — current-request maintainer waiver for local-only
  execution because GitHub PR, issue, and Actions access are unavailable
- Main Agent run: `4b4aeba5-9ace-42a0-b4c1-66231f342b10`
- Delivery: signed local `main` commits only; no push or provider mutation
- Active task: Tasks 1.1–1.3 review repair
- Last checkpoint: signed candidate reviewed; blocking findings reassigned to a
  fresh authenticated worker after the original controller became unavailable

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | review-repair | Establish meaningful red coverage | signed candidate `a69897b4fd562cb26afd85606e1f5b722f246e1a`; validated specialist review | Add the missing observable boundary and preflight cases before acceptance. |
| 1.2 | review-repair | Implement the new atomic command and receipt boundary | signed candidate `a69897b4fd562cb26afd85606e1f5b722f246e1a`; validated specialist review | Repair duplicate singleton parsing, remove `--message-out`, and preserve usage exit semantics. |
| 1.3 | review-repair | Align docs and generated completions | signed candidate `a69897b4fd562cb26afd85606e1f5b722f246e1a`; validated specialist review | Exact parser/completion parity remains required. |
| 1.4 | pending | Repair Main Agent startup diagnosis and recovery | first-worker failure evidence | Separate nils-cli assignment after the current contract lane. |
| 2.1 | pending | Replace runtime hook and policy references | none | Serial runtime-kit assignment after nils contract. |
| 2.2 | pending | Make Main Agent abnormal-state handling proactive | user-requested execution expansion | Update skill/protocol and failure matrix in runtime-kit. |
| 3.1 | pending | Independently review and validate both candidates | none | Main Agent acceptance gate. |
| 3.2 | pending | Commit both local main branches before cutover | none | Use pre-change installed command only for bootstrap. |
| 3.3 | pending | Deploy and prove fresh-session hook permissions | none | Completion gate includes provider-observed fresh turn. |

## Scope decisions

- `local-default` receives no alias or compatibility implementation.
- The command stays in `semantic-commit`; `git-cli` and `forge-cli` retain
  their existing specialist boundaries.
- Main Agent safety continues to require authenticated claim/checkpoint
  evidence, but abnormal workers are inspected proactively through bounded
  metadata and diagnostic surfaces rather than passive timeout-only waiting.
- Pane content can classify a startup failure only after metadata is
  insufficient; it never proves authorization, completion, or acceptance.
- The first failed worker remains visible until the repaired facade can prove a
  pre-claim, operation-free, clean-worktree cancel and guarded retire. Direct
  session deletion or group force cleanup is not an acceptable substitute.
- Main Agent operations are macro-first (`start`, `supervise`, `reassign`,
  `retire`) with independently callable typed primitives for diagnosis,
  submit-key recovery, cancellation, wait, message, and delete. A macro failure
  routes to primitives from its last proven safe state rather than repeating
  opaque ceremony or stopping immediately.
- The implementation uses one managed worker at a time under enforce
  coordination; cross-repository work is serial to preserve bootstrap order.
- All required evidence is local. GitHub and remote-provider operations are
  prohibited for this plan.

## Validation

| Command | Status | Summary |
| --- | --- | --- |
| `plan-archive catalog --grep semantic-commit --deep --format json` | pass | Historical agent-flow plan reviewed; no current rename plan found. |
| `plan-archive catalog --grep local-default --deep --format json` | pass | No archived plan matched. |
| `plan-archive catalog --grep default-branch --deep --format json` | pass | No archived plan matched. |
| `agent-session activity doctor --agent codex --format json` | pass | Supported provider and executable helper; compatibility preview required. |
| `agent-session activity setup --agent codex --repair --dry-run --format json` | pass | Converged, no changes, no representation conflict. |
| `review-specialists validate` and `merge` | pass | Full L2 testing, maintainability, API-contract, security, performance, and red-team findings merged into the repair packet. |
| `scripts/smoke-agent-console.sh` | pass | Non-destructive Agent Console restart recovered the daemon and its full smoke suite; the existing worker controller still could not reconnect. |
| `main-agent worker start ... --await-ready 5m` | pass | Fresh review-repair worker reached `working` through an authenticated checkpoint. |

## Blockers

- Retained diagnostic: the first nils-cli assignment passed a literal worktree
  path into a field that accepts only an HMAC fingerprint. Worker bootstrap
  exited 65 before claim acquisition or mutation, and folded readiness ended as
  `readiness_failed` with `automatic_retry_safe:false`. The exact worker is
  retained without prompt retry. Reassignment uses a new isolated worktree and
  omits the optional packet `worktree` field so the claim owner derives the
  checkout fingerprint from the authenticated session `cwd`.
- The submitted implementation worker became unreachable through its app-server
  controller after completing a clean, signed candidate. Its broker proves no
  claim and no active or uncertain operation. A bounded Agent Console service
  restart and full smoke pass did not reconnect that existing controller, so
  queued mailbox messages were not treated as delivery. Review repair was
  explicitly reassigned to a fresh worktree; the old worker remains retained
  until the new guarded per-assignment cancel and retire path exists.

## Session Log

- 2026-07-25: Maintainer explicitly enabled Main Agent Mode and authorized the
  bounded local-main delivery, local build/install/deploy, and fresh-session
  hooks permission acceptance.
- 2026-07-25: Initialized durable L2 Main Agent run
  `4b4aeba5-9ace-42a0-b4c1-66231f342b10` with Codex as the supported worker
  provider.
- 2026-07-25: Promoted the implementation-readiness source into this local-only
  plan bundle. No GitHub tracker was opened under the explicit delivery
  constraint.
- 2026-07-25: First nils-cli worker
  `worker-1b7b9850-9ae2-4086-9574-db72b8acfb67` completed its provider turn
  without bootstrapping. Durable state remained `starting` with no checkpoint,
  claim, diff, or commit. The typed start result was `readiness_failed`;
  runtime-owned single-Enter recovery was exhausted. Read-only source
  inspection identified the optional assignment `worktree` serialization bug
  and a claim-safe packet shape for a fresh isolated reassignment.
- 2026-07-25: Replacement worker produced signed candidate
  `a69897b4fd562cb26afd85606e1f5b722f246e1a`, passed focused package checks and
  the complete local-fast gate, released its claim, and submitted evidence.
- 2026-07-25: Full L2 specialist review found blocking parser/hook agreement,
  side-effect, completion parity, observable-boundary, preflight, waiver, and
  exit-contract gaps. A repair packet recorded required fixes and explicitly
  bounded residual architectural risks.
- 2026-07-25: Mailbox delivery to the submitted worker remained queued because
  its controller was unavailable. The main agent inspected broker state,
  restarted the Agent Console runtime once through the approved project
  runbook, and proved the daemon healthy with its full smoke suite. Because the
  old controller could not reconnect, the main agent did not send raw terminal
  input or replay the prompt.
- 2026-07-25: After proving no claim, no active or uncertain operation, a clean
  source worktree, and a valid signed candidate, the main agent expanded the
  planned `reassign` macro manually: created a fresh isolated worktree and
  launched `nils-default-branch-review-repair`. Authenticated readiness passed
  and the assignment entered `working`.
