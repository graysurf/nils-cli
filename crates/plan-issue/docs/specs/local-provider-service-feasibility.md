# Local Provider Service Feasibility

## Status

- Type: feasibility evaluation — go / no-go, **eval only**.
- Date: 2026-05-31.
- Depends on: Task 3.1 (cross-provider conformance harness).
- Decision gate: this document records a recommendation. It does **not** start
  a build. Building the service requires a separate, explicit go.

## Question

Can `forge-cli Provider::Local` — the in-process, file-backed backend frozen by
`local-provider-contract-v1.md` and proven by the conformance harness — be
lifted behind HTTP as a standalone **plan / issue tracking service**?

The honest framing the rollout settled on: a service is viable for the issue /
timeline half only. The local backend is not a VCS, and networking it does not
make it one. The realistic product is an *issue / plan-tracking service*, not a
GitHub/GitLab replacement.

## The capability split bounds the answer

The 11 `ProviderAdapter` methods cleave into two halves (see
`local-provider-contract-v1.md` §"The Capability Split"). That line is exactly
the line a service can and cannot cross.

| Half | Methods | Local today | Service-grade? |
| --- | --- | --- | --- |
| A — issue / timeline | `create_issue`, `issue_body`, `issue_evidence`, `list_open_tracker_issues`, `edit_issue_body`, `comment_issue`, `edit_issue_labels`, `close_issue` | REAL — the file store is the source of truth | **Yes** — the store *is* the system of record |
| B — PR / merge / CI | `pr_is_merged`, `pr_merge_summary`, `pr_comments` | SEEDED STUB — returns what a test wrote | **No** — there is no VCS/CI behind it |

A service can own Half A because the store holds the authoritative state: an
issue's body, labels, comments, and open/closed status are not projections of
some upstream system — they are the truth. Half B returns seeded fixtures with
no backing VCS/CI; networking it would expose a fake. Half B stays a stub
unless a real VCS/CI is wired in, which is a different (and much larger)
project. **The service surface is Half A.**

## What the conformance evidence buys us

Task 3.1's harness (`crates/forge-cli/tests/integration/conformance.rs`) drives
`{local, github, gitlab}` through the real binary and asserts the issue-half
observable is identical across all three. Task 3.2 ran the full plan-tracking
lifecycle (create → execute → closeout) green against a real self-hosted
GitLab project.

Net: the Half A contract the service would expose is already pinned to
real-provider behaviour, not asserted in isolation. A service built on the same
store + op layer inherits that conformance — the API's observable semantics are
a known quantity, not a fresh design.

## Architecture sketch

The local backend is already cleanly layered: ops build provider-agnostic
calls, `LocalRunner` interprets them against `crate::local::store::Store`, and
`Store` owns the on-disk schema (`repo.json`, `issues/<n>.json`, `prs/<n>.json`).
A service reuses the bottom two layers and swaps the top:

- **Transport.** A thin HTTP layer (one handler per Half A operation) maps
  requests onto the same `Store` calls `LocalRunner` makes today. The envelope
  contract (`{schema_version, ok, data}` / `{ok:false, error}`) maps directly
  onto JSON responses, so clients see the same shapes the CLI already emits.
- **State.** `Store` is the system of record. The on-disk schema is the wire
  contract the driver and `forge-cli` already share, so the service starts from
  a frozen, tested data model.
- **Reuse, not rewrite.** The issue lifecycle logic (`alloc_issue_number`,
  label set mutation with add/remove, AND-semantics list filtering, comment
  append) lives in `Store` + `LocalRunner` and is already conformance-tested.
  The service is a new front door on a proven core.

## Gaps to close before any build

These are the real prerequisites — the reasons this is a *conditional* go, not
a green light. Each is a deliberate difference between a hermetic test backend
and a multi-client service.

| Gap | Today (test backend) | Service needs |
| --- | --- | --- |
| Concurrency | single-process, no locking; last-writer-wins on a file | per-repo write serialization (advisory lock) or a real transactional store (e.g. SQLite/Postgres) behind the same schema |
| Timestamps | deterministic monotonic clock seeded at `2026-01-01T00:00:00Z` (no wall clock — required for reproducible tests) | real wall-clock timestamps; the synthetic clock is a *test* property and must not ship |
| Tenancy | one store root = one repo (`RepoFile` holds a single slug) | a keyspace / store-per-repo layout and request routing by repo |
| Identity | comment author hardcoded `"local"`; no auth | authenticated principals; author derived from the caller; per-repo authz |
| URLs | synthetic `local://<slug>/...` scheme | real `https://<host>/...` URLs the lifecycle can store and round-trip (resolve-approval scans stored comment URLs) |
| Durability / ops | a directory on one machine | backup, migration of the on-disk schema, observability, rate limits |

None of these is a blocker to the *concept*; all of them are work that the
current backend deliberately omits because it is a test fake. The clock and the
author field in particular are intentional test simplifications, not service
behaviour.

## Out of scope / non-goals

- Half B (PR / merge / CI) as a service — stays a seeded stub; out unless a
  real VCS/CI is wired in.
- Any build commitment. This eval does not authorize implementation.
- A migration path from the file store to a database — noted as a gap, not
  designed here.

## Recommendation

**Conditional GO — for an issue / plan-tracking service scoped to Half A.**

- The capability split makes the scope unambiguous and honest: ship the issue /
  timeline half, leave PR / CI out. The realistic product is a plan/issue
  tracking service, and that half is genuinely service-grade because the store
  is the system of record.
- The conformance harness (3.1) + the real-GitLab e2e (3.2) de-risk the API
  semantics: the surface a service would expose is already pinned to
  real-provider behaviour.
- The prerequisites above (concurrency, real timestamps, tenancy, auth, real
  URLs, durability) are the gating work. A build proposal must address them
  explicitly; the synthetic clock and hardcoded author must be replaced, not
  shipped.
- **No build starts without a separate, explicit go.** This document satisfies
  the rollout's gate by recording the recommendation; the next step, if taken,
  is a dedicated service design + plan, not a continuation of this rollout.

## References

- Contract: `local-provider-contract-v1.md` (capability split, on-disk schema,
  per-method contract, determinism, store locator).
- Conformance: `crates/forge-cli/tests/integration/conformance.rs` (Task 3.1).
- As-built backend: `crates/forge-cli/src/local/` (`store.rs`, `runner.rs`,
  `mod.rs`).
- Source vision: the rollout discussion-source — "the realistic service = an
  issue/plan-tracking service" and "the service path (P5) is a natural
  extension (network the local backend)".
