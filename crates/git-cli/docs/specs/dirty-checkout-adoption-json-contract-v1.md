# Dirty Checkout Adoption JSON Contract v1

## Ownership and Scope

This is the producer-owned JSON contract for dirty-checkout snapshots and the
private challenge, receipt, lease, and adoption records written or consumed by
`nils-git-cli`.

The canonical instances live under
`tests/fixtures/dirty-checkout-adoption/`. They are bare protocol records, not
the shared CLI success/error envelope. The snapshot fixture corresponds to the
payload emitted by `git-cli worktree dirty-snapshot --format json`; the other
fixtures model private state used by adoption and lease enforcement.

These fixtures are canonical examples rather than JSON Schema documents. A unit
test constructs each producer type, compares it with the corresponding fixture
using semantic JSON equality, parses it through the strict consumer, and checks
relationships across records.

## Schemas

| Schema | Canonical fixture | Purpose |
| --- | --- | --- |
| `agent-runtime.dirty-checkout-snapshot.v1` | `dirty-checkout-snapshot-v1.json` | Snapshot identity and bounded accounting |
| `agent-runtime.dirty-checkout-challenge.v1` | `dirty-checkout-challenge-v1.json` | Short-lived, snapshot-bound authorization challenge |
| `agent-runtime.dirty-checkout-receipt.v1` | `dirty-checkout-receipt-v1.json` | Durable adoption receipt |
| `agent-runtime.checkout-lease.v1` | `checkout-lease-v1.json` | Original checkout lease |
| `agent-runtime.checkout-lease.v2` | `checkout-lease-v2.json` | Lease with native path identity and adoption provenance |
| `agent-runtime.dirty-checkout-adoption.v1` | `dirty-checkout-adoption-v1.json` | Adoption provenance embedded in a v2 lease |

`agent-runtime.dirty-checkout-pending.v1` is an internal crash-recovery record.
It is not a cross-language handoff contract and has no canonical fixture here.

## Common Encoding Rules

- JSON objects are strict: duplicate, unknown, missing, or wrongly typed fields
  are rejected.
- Schema identifiers are exact, case-sensitive strings.
- Digests, keys, snapshot IDs, and receipt IDs are lowercase hexadecimal with
  the length enforced by the consuming record.
- Timestamps are unsigned Unix seconds. A challenge expires no more than 300
  seconds after issuance. Lease timestamps are ordered as
  `acquired_at <= refreshed_at <= expires_at`.
- `head_oid` is a lowercase Git object ID of 40 to 64 hexadecimal characters,
  or the implementation's explicit unborn-HEAD identity.
- Numeric snapshot counters are non-negative JSON integers and remain subject
  to implementation resource limits.

## Snapshot and Challenge Binding

The challenge copies these fields from the accepted snapshot:

- `repository_key`
- `checkout_key`
- `checkout_instance`
- `snapshot_id`
- `head_oid`
- `branch_ref_digest`

The raw challenge token is never stored. `token_digest` identifies the bearer
capability and keys live-challenge and pending-recovery lookup. It is distinct
from the SHA-256 digest of the exact challenge-file bytes.
`authorization_turn_digest` binds the authorizing turn without retaining its
text.

## Receipt and Adoption Binding

The receipt preserves the challenge's session, checkout, snapshot, and
authorization identities. It adds:

- `receipt_id`, the durable receipt identity;
- `reason_digest`, which binds the reason file without retaining its content;
- `challenge_digest`, the SHA-256 digest of the exact consumed challenge-file
  bytes, including their encoding and whitespace, rather than the bearer
  `token_digest`; and
- `adopted_at`, the transition time selected at the durable authorization
  boundary.

Challenge validity is checked through the post-install verification barrier. If
it expires before that barrier completes, the provisional lease is revoked and
the authorization is not accepted.

The standalone adoption record is identical to the `adoption` object embedded
in a v2 lease. Its receipt, snapshot, authorization, reason, challenge, and
adoption-time fields equal the corresponding receipt fields.
`challenge_issued_at` equals the source challenge's `issued_at`, and must not be
after `adopted_at`.

## Lease Versions and Paths

A v1 lease contains textual absolute checkout and Git-directory paths. A v2
lease retains those fields and adds lowercase hexadecimal encodings of the
native operating-system path bytes:

- `checkout_root_bytes` decodes exactly to the current native checkout-root
  bytes;
- `checkout_git_dir_bytes` decodes exactly to the current native checkout
  Git-directory bytes.

The native-byte fields are authoritative. The textual fields remain required
compatibility/display values and equal the implementation's deterministic lossy
UTF-8 rendering of those native bytes. For UTF-8 paths this is the original path
text; non-UTF-8 paths can contain replacement characters. Both versions remain
strict wire variants; a record cannot combine v1 and v2 fields.

## Retry, Revocation, and Recovery

A retry with the same bearer and reason returns the persisted receipt only when
the active lease, receipt, exact spent-challenge bytes, challenge identities,
current checkout snapshot, and all adoption fields match. A changed input or
artifact fails closed. An install that reports an error but leaves the exact
fully validated expected lease is observably committed and follows this same
recovery path.

Revocation first durably renames the active lease to its receipt-bound
`.revoked-<receipt_id>.json` tombstone. Recovery then removes receipt, spent
challenge, live challenge, and pending records idempotently, including states
where any cleanup subset already completed. Tombstones are retained to make
revocation retries idempotent and pruned oldest-first under a fixed count bound;
each rename, cleanup phase, and pruning phase is directory-synchronized before
the next durable conclusion.

Pending adoption records carry both the bearer `token_digest` used for lookup
and the exact-artifact `challenge_digest`. They also bind any predecessor
artifacts that still existed when replacement began, so independently completed
predecessor cleanup can resume without treating absence as corruption.

## Evolution

Changing field names, field meanings, requiredness, encoding, or cross-record
relationships requires a new schema version and new canonical fixtures.
Consumers must fail closed on unsupported schema identifiers rather than
silently accepting a changed contract.
