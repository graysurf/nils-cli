# memo Storage Schema v1

## Purpose

Define the SQLite v1 schema for durable inbox capture and agent derivation workflows used by:

- `list` and `fetch` state filtering;
- `search` over raw plus active derived text;
- `report` aggregates over active categories/tags;
- `apply` versioned write-back with safe reprocessing.

This spec is aligned 1:1 with:

- `crates/memo/src/storage/sql/schema_v1.sql`

## Design Principles

- Raw capture supports explicit maintenance operations (`update`, `delete --hard`).
- Derivations are versioned by item and remain queryable for audit.
- Exactly one accepted active derivation is allowed per item.
- Reprocessing is idempotent by `(item_id, derivation_hash)`.
- Full-text search uses `fts5` over a denormalized per-item search document.
- Workflow extension data must be anchored to raw items through
  `workflow_item_anchors` with cascade cleanup.
- Storage bootstrap uses one consolidated schema snapshot (`schema_v1.sql`) as
  the source of truth; historical incremental migration chains are retired.
- Phase 1 format/validation metadata follows Approach A:
  canonical values live in derivation `payload_json`, with deterministic
  query tags (`fmt:*`, `val:*`).
- Phase 1 metadata rollout is additive and requires no schema migration.

## Core Tables

### `inbox_items`

Raw capture rows.

Columns:

- `item_id integer primary key`
- `source text not null default 'manual'`
- `raw_text text not null`
- `created_at text not null default current UTC timestamp`
- `inserted_at text not null default current UTC timestamp`

Constraints:

- `source` and `raw_text` must be non-empty after trim.

Indexes:

- `idx_inbox_items_created_item_desc(created_at desc, item_id desc)`

Why:

- Supports deterministic list/fetch ordering (`created_at desc`, `item_id desc`).

### `id_allocators`

Monotonic id allocator state for raw inbox keys.

Columns:

- `name text primary key`
- `last_id integer not null` (`>= 0`)

Rows:

- `('inbox_items', <last_allocated_item_id>)`

Why:

- Guarantees `item_id` is monotonic and non-reused per DB, including after
  hard delete clears all rows from `inbox_items`.
- Example: deleting `item_id=1` does not make `1` reusable; next insert
  allocates `2` or higher.
- Re-seeds missing allocator rows from `max(inbox_items.item_id)`.

### `item_derivations`

Versioned enrichment payloads written by `apply`.

Columns:

- `derivation_id integer primary key`
- `item_id integer not null references inbox_items(item_id) on delete restrict`
- `derivation_version integer not null`
- `status text not null` (`accepted|rejected|conflict`)
- `is_active integer not null default 0` (`0|1`)
- `base_derivation_id integer references item_derivations(derivation_id) on delete restrict`
- `derivation_hash text not null`
- `agent_run_id text not null`
- `summary text`
- `category text`
- `priority text` (`low|medium|high|urgent|NULL`)
- `due_at text`
- `normalized_text text`
- `confidence real` (`0.0..1.0|NULL`)
- `payload_json text not null`
- `conflict_reason text`
- `applied_at text not null default current UTC timestamp`

Constraints:

- `derivation_version > 0`
- `is_active in (0,1)`
- `is_active=1` only when `status='accepted'`
- `status='conflict'` requires non-null `conflict_reason`
- unique version key: `unique(item_id, derivation_version)`
- idempotency key: `unique(item_id, derivation_hash)`
- sequential version trigger:
  - `trg_item_derivations_next_version`

Indexes:

- `idx_item_derivations_one_active_per_item(item_id) where is_active=1 and status='accepted'` (unique)
- `idx_item_derivations_item_version_desc(item_id, derivation_version desc)`
- `idx_item_derivations_active_category(category, item_id) where is_active=1 and status='accepted'`
- `idx_item_derivations_applied_desc(applied_at desc, derivation_id desc)`

Why:

- Enables fast state checks for list/fetch and category rollups for report.
- Preserves full derivation history for audit/rollback.

Phase 1 metadata payload contract (Approach A):

- `payload_json` may include additive metadata fields:
  - `content_type`: `url|json|yaml|xml|markdown|text|unknown`
  - `validation_status`: `valid|invalid|unknown|skipped`
  - `validation_errors[]`: objects with `code`, `message`, optional `path`
- Metadata is derivation-scoped and versioned with each derivation row.

### `tags`

Canonical tag dictionary.

Columns:

- `tag_id integer primary key`
- `tag_name text not null`
- `tag_name_norm text not null`
- `created_at text not null default current UTC timestamp`

Constraints:

- `tag_name` and `tag_name_norm` are non-empty
- `tag_name_norm = lower(tag_name_norm)`
- `unique(tag_name_norm)`

Why:

- Stable dedupe and case-insensitive normalization for report/search labels.

Reserved deterministic metadata tag namespace (phase 1):

- `fmt:<content_type>` for detected payload format.
- `val:<validation_status>` for validation outcome.

Rules:

- Prefix tokens are lowercase and must use the documented taxonomy values.
- Per derivation version, write path should emit at most one `fmt:*` and one
  `val:*` tag (plus optional domain tags).

### `item_tags`

Many-to-many between a derivation version and canonical tags.

Columns:

- `derivation_id integer not null references item_derivations(derivation_id) on delete cascade`
- `tag_id integer not null references tags(tag_id) on delete restrict`
- `created_at text not null default current UTC timestamp`

Constraints:

- `primary key (derivation_id, tag_id)`

Indexes:

- `idx_item_tags_tag_id_derivation_id(tag_id, derivation_id)`

Why:

- Keeps historical tag assignments per derivation version.
- Supports top-tag report queries from active derivations.

### `workflow_item_anchors`

Extension ownership anchors for typed workflow records.

Columns:

- `anchor_id integer primary key`
- `item_id integer not null references inbox_items(item_id) on delete cascade`
- `workflow_type text not null`
- `created_at text not null default current UTC timestamp`
- `updated_at text not null default current UTC timestamp`

Constraints:

- `workflow_type` is non-empty
- `unique(item_id, workflow_type)`

Indexes:

- `idx_workflow_item_anchors_type_item(workflow_type, item_id)`

Why:

- Standardized ownership root so future workflow tables can cleanly cascade from
  one item.

### `workflow_game_entries` (example typed workflow table)

Example extension table for game-oriented enrichment data.

Columns:

- `game_entry_id integer primary key`
- `anchor_id integer not null references workflow_item_anchors(anchor_id) on delete cascade`
- `game_name text not null`
- `source_url text`
- `description text`
- `created_at text not null default current UTC timestamp`

Constraints:

- `game_name` is non-empty

Indexes:

- `idx_workflow_game_entries_anchor(anchor_id)`

Why:

- Demonstrates approved extension pattern using anchor ownership and cascade
  cleanup.

## FTS5 Strategy

### `item_search_documents`

Denormalized mutable search projection (one row per item).

Columns:

- `item_id integer primary key references inbox_items(item_id) on delete restrict`
- `raw_text text not null`
- `derived_text text not null default ''`
- `tags_text text not null default ''`
- `updated_at text not null default current UTC timestamp`

### `item_search_fts` (virtual table)

`fts5` index over `item_search_documents`:

- indexed columns: `raw_text`, `derived_text`, `tags_text`
- external-content mode:
  - `content='item_search_documents'`
  - `content_rowid='item_id'`
- tokenizer:
  - `unicode61 remove_diacritics 2 tokenchars '-_'`

Sync triggers:

- FTS content sync from projection table:
  - `trg_item_search_documents_ai`
  - `trg_item_search_documents_ad`
  - `trg_item_search_documents_au`
- Projection refresh from source tables:
  - `trg_inbox_items_ai_search_document`
  - `trg_inbox_items_au_refresh_search_document`
  - `trg_item_derivations_ai_refresh_search_document`
  - `trg_item_derivations_au_refresh_search_document`
  - `trg_item_derivations_ad_refresh_search_document`
  - `trg_item_tags_ai_refresh_search_document`
  - `trg_item_tags_ad_refresh_search_document`

Why:

- Search path can use one stable `fts5` lookup and then join metadata for ranking/tie-break.
- `derived_text` and `tags_text` always represent the current active accepted derivation.

## Lifecycle Rules

### 1. Raw capture lifecycle (`add`, `update`, `delete`)

- `add` inserts new raw rows into `inbox_items`.
- `add` increments `id_allocators('inbox_items').last_id` and uses it as the new `item_id`.
- `update` rewrites one `raw_text` value and clears dependent derivation/extension
  rows for that item so it returns to pending state.
- `delete --hard` removes one item plus dependent derivation/search/extension rows.

### 2. Derivation versioning (`apply`)

- `apply` inserts a new `item_derivations` row per accepted new payload.
- `derivation_version` must be strictly sequential per item (guarded by trigger).
- Prior derivations remain queryable and are never removed by normal flow.
- `update` and `delete --hard` are explicit maintenance flows that may clear rows.

### 3. Active selection

- Active row criteria: `is_active=1 and status='accepted'`.
- At most one active row per item is enforced by unique partial index.
- Expected transactional update order in `apply`:
  1. Optionally mark previous active row inactive (`is_active=0`).
  2. Insert next derivation version (usually `is_active=1` when accepted).
  3. Insert `item_tags` rows for the new derivation.
  4. Let refresh triggers rebuild `item_search_documents` and `item_search_fts`.

### 4. Reprocessing and idempotency

- Same semantic payload for same item should reuse `derivation_hash`.
- `unique(item_id, derivation_hash)` turns duplicate apply into a safe no-op (`insert or ignore` style).
- New payload variant creates a new derivation version and may become active.

### 5. Conflict handling

- `base_derivation_id` stores the derivation revision the worker read.
- If payload is stale against current active revision, `apply` may record:
  - `status='conflict'`
  - `is_active=0`
  - `conflict_reason` populated
- Conflict rows are retained for audit and retry logic; they do not replace the active accepted row.

### 6. Format and validation metadata persistence (Approach A, phase 1)

- Canonical metadata is persisted in `item_derivations.payload_json`.
- Query-friendly metadata is mirrored through `tags` + `item_tags` using
  deterministic `fmt:*` and `val:*` tags.
- `update` may replace `inbox_items.raw_text`; metadata for previous raw text is
  dropped through derivation cleanup.
- Existing `payload_json`, `tags`, and `item_tags` structures are reused, so no
  schema migration is required for phase 1.

### 7. Workflow extension ownership and cleanup

- Extension records must be rooted at `workflow_item_anchors`.
- `workflow_item_anchors.item_id` must use `on delete cascade`.
- Typed workflow tables (for example game/health/sport) must FK to
  `workflow_item_anchors.anchor_id` with `on delete cascade`.
- `on delete restrict` is disallowed on the cleanup path from extension rows to
  raw items.

## Query Path Mapping

- `list --state all`:
  - `inbox_items` ordered by `created_at desc, item_id desc`.
- `list --state pending` and `fetch --state pending`:
  - anti-join against active accepted `item_derivations`.
- `list --state enriched`:
  - join against active accepted `item_derivations`.
- `search <query>`:
  - `item_search_fts match ?` then join `inbox_items` and active derivation metadata.
- `report week|month`:
  - capture totals from `inbox_items.created_at`
  - category rollups from active `item_derivations.category`
  - tag rollups from active derivations + `item_tags` + `tags`.
- Workflow-specific queries:
  - join typed workflow tables through `workflow_item_anchors`.

## Alignment Contract

The SQL file must keep these object names unchanged:

- tables: `inbox_items`, `id_allocators`, `item_derivations`, `tags`, `item_tags`,
  `workflow_item_anchors`, `workflow_game_entries`, `item_search_documents`
- virtual table: `item_search_fts`
- critical indexes:
  - `idx_inbox_items_created_item_desc`
  - `idx_item_derivations_one_active_per_item`
  - `idx_item_derivations_item_version_desc`
  - `idx_item_derivations_active_category`
  - `idx_item_tags_tag_id_derivation_id`
  - `idx_workflow_item_anchors_type_item`

In addition, the runtime migration layer creates and maintains:

- `schema_migrations(version, applied_at)`: bookkeeping table that records which
  versions of `schema_v1.sql` have been applied; not part of the v1 data
  schema and not queried by any command surface.

## Compatibility Policy

- Post-consolidation builds initialize and maintain storage from
  `schema_v1.sql` only.
- Pre-consolidation DB files are outside compatibility scope; operators should
  recreate DB files when upgrading across this boundary.
