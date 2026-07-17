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
capability, while `authorization_turn_digest` binds the authorizing turn without
retaining its text.

## Receipt and Adoption Binding

The receipt preserves the challenge's session, checkout, snapshot, and
authorization identities. It adds:

- `receipt_id`, the durable receipt identity;
- `reason_digest`, which binds the reason file without retaining its content;
- `challenge_digest`, which binds the consumed challenge artifact; and
- `adopted_at`, the final accepted transition time.

The standalone adoption record is identical to the `adoption` object embedded
in a v2 lease. Its receipt, snapshot, authorization, reason, challenge, and
adoption-time fields equal the corresponding receipt fields.
`challenge_issued_at` equals the source challenge's `issued_at`, and must not be
after `adopted_at`.

## Lease Versions and Paths

A v1 lease contains textual absolute checkout and Git-directory paths. A v2
lease retains those fields and adds lowercase hexadecimal encodings of the
native operating-system path bytes:

- `checkout_root_bytes` decodes exactly to `checkout_root` and the current
  checkout root;
- `checkout_git_dir_bytes` decodes exactly to `checkout_git_dir` and the
  current checkout Git directory.

This preserves non-UTF-8 path identity on platforms that expose native path
bytes. Both versions remain strict wire variants; a record cannot combine v1
and v2 fields.

## Evolution

Changing field names, field meanings, requiredness, encoding, or cross-record
relationships requires a new schema version and new canonical fixtures.
Consumers must fail closed on unsupported schema identifiers rather than
silently accepting a changed contract.
