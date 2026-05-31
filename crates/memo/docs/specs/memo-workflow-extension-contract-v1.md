# memo Workflow Extension Contract v1

## Purpose

Define the required schema contract for future workflow-specific data in
`memo` (for example `game`, `sport`, `health`) so hard delete and update
operations always clean dependent data without orphan rows.

## Required ownership model

1. Every workflow record must be rooted at one `inbox_items.item_id`.
2. Ownership must flow through `workflow_item_anchors`.
3. Cleanup path must use `on delete cascade` all the way down.

Required chain:

```text
inbox_items(item_id)
  -> workflow_item_anchors(item_id on delete cascade)
      -> workflow_<type>_entries(anchor_id on delete cascade)
```

## Required table-level rules

- Anchor table:
  - `workflow_item_anchors.anchor_id` is the stable parent key for typed rows.
  - `workflow_item_anchors.item_id` references `inbox_items(item_id)` with
    `on delete cascade`.
  - `workflow_item_anchors.workflow_type` is required and non-empty.
- Typed workflow tables:
  - Must reference `workflow_item_anchors(anchor_id)` with `on delete cascade`.
  - Must not reference `inbox_items` directly.
  - Must keep business fields optional unless truly required by that workflow.
- Disallowed patterns:
  - `on delete restrict` anywhere on the cleanup path from typed rows to raw
    item.
  - Triggers as primary cleanup mechanism for unknown future workflow tables.

## Trigger boundary policy

- Triggers are allowed for:
  - projection refresh (for example search docs),
  - deterministic derived invariants.
- Triggers are not the source of truth for extension cleanup.
- FK cascade chain is the source of truth for extension cleanup.

## Update/delete semantics

- `memo update <item_id> <text>`:
  - must clear `workflow_item_anchors` rows for that `item_id` in the same
    transaction as derivation cleanup.
  - typed extension rows are removed by anchor cascade.
- `memo delete <item_id> --hard`:
  - must remove the raw row and all extension rows reachable by cascade.

## Authoring checklist for new workflow types

1. Add typed table with FK to `workflow_item_anchors(anchor_id) on delete cascade`.
2. Add index for anchor lookup (`idx_workflow_<type>_entries_anchor`).
3. Add integration test that inserts:
   - one raw item,
   - one anchor row for `workflow_type=<type>`,
   - one typed row.
4. Verify hard delete removes typed row and anchor row.
5. Verify update clears typed row and anchor row while keeping raw item pending.
