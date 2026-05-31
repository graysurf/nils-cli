pragma foreign_keys = on;

drop trigger if exists trg_inbox_items_append_only_update;
drop trigger if exists trg_inbox_items_append_only_delete;

create table if not exists inbox_items (
  item_id integer primary key,
  source text not null default 'manual',
  raw_text text not null,
  created_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  inserted_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  check(length(trim(source)) > 0),
  check(length(trim(raw_text)) > 0)
);

create table if not exists id_allocators (
  name text primary key,
  last_id integer not null check(last_id >= 0)
);

create index if not exists idx_inbox_items_created_item_desc
  on inbox_items(created_at desc, item_id desc);

create table if not exists item_derivations (
  derivation_id integer primary key,
  item_id integer not null references inbox_items(item_id) on delete restrict,
  derivation_version integer not null check(derivation_version > 0),
  status text not null check(status in ('accepted', 'rejected', 'conflict')),
  is_active integer not null default 0 check(is_active in (0, 1)),
  base_derivation_id integer references item_derivations(derivation_id) on delete restrict,
  derivation_hash text not null,
  agent_run_id text not null,
  summary text,
  category text,
  priority text check(priority is null or priority in ('low', 'medium', 'high', 'urgent')),
  due_at text,
  normalized_text text,
  confidence real check(confidence is null or (confidence >= 0.0 and confidence <= 1.0)),
  payload_json text not null,
  conflict_reason text,
  applied_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  check(length(trim(derivation_hash)) > 0),
  check(length(trim(agent_run_id)) > 0),
  check(is_active = 0 or status = 'accepted'),
  check(status <> 'conflict' or conflict_reason is not null),
  unique(item_id, derivation_version),
  unique(item_id, derivation_hash)
);

create trigger if not exists trg_item_derivations_next_version
before insert on item_derivations
for each row
begin
  select case
    when new.derivation_version <> coalesce(
      (select max(d.derivation_version) + 1 from item_derivations d where d.item_id = new.item_id),
      1
    )
    then raise(abort, 'item_derivations.derivation_version must be sequential per item')
  end;
end;

create unique index if not exists idx_item_derivations_one_active_per_item
  on item_derivations(item_id)
  where is_active = 1 and status = 'accepted';

create index if not exists idx_item_derivations_item_version_desc
  on item_derivations(item_id, derivation_version desc);

create index if not exists idx_item_derivations_active_category
  on item_derivations(category, item_id)
  where is_active = 1 and status = 'accepted';

create index if not exists idx_item_derivations_applied_desc
  on item_derivations(applied_at desc, derivation_id desc);

create table if not exists tags (
  tag_id integer primary key,
  tag_name text not null,
  tag_name_norm text not null,
  created_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  check(length(trim(tag_name)) > 0),
  check(length(trim(tag_name_norm)) > 0),
  check(tag_name_norm = lower(tag_name_norm)),
  unique(tag_name_norm)
);

create table if not exists item_tags (
  derivation_id integer not null references item_derivations(derivation_id) on delete cascade,
  tag_id integer not null references tags(tag_id) on delete restrict,
  created_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  primary key (derivation_id, tag_id)
);

create index if not exists idx_item_tags_tag_id_derivation_id
  on item_tags(tag_id, derivation_id);

create table if not exists workflow_item_anchors (
  anchor_id integer primary key,
  item_id integer not null references inbox_items(item_id) on delete cascade,
  workflow_type text not null,
  created_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  updated_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  check(length(trim(workflow_type)) > 0),
  unique(item_id, workflow_type)
);

create index if not exists idx_workflow_item_anchors_type_item
  on workflow_item_anchors(workflow_type, item_id);

create table if not exists workflow_game_entries (
  game_entry_id integer primary key,
  anchor_id integer not null references workflow_item_anchors(anchor_id) on delete cascade,
  game_name text not null,
  source_url text,
  description text,
  created_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  check(length(trim(game_name)) > 0)
);

create index if not exists idx_workflow_game_entries_anchor
  on workflow_game_entries(anchor_id);

create table if not exists item_search_documents (
  item_id integer primary key references inbox_items(item_id) on delete restrict,
  raw_text text not null,
  derived_text text not null default '',
  tags_text text not null default '',
  updated_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

create virtual table if not exists item_search_fts using fts5(
  raw_text,
  derived_text,
  tags_text,
  content='item_search_documents',
  content_rowid='item_id',
  tokenize='unicode61 remove_diacritics 2 tokenchars ''-_'''
);

create trigger if not exists trg_item_search_documents_ai
after insert on item_search_documents
begin
  insert into item_search_fts(rowid, raw_text, derived_text, tags_text)
  values (new.item_id, new.raw_text, new.derived_text, new.tags_text);
end;

create trigger if not exists trg_item_search_documents_ad
after delete on item_search_documents
begin
  insert into item_search_fts(item_search_fts, rowid, raw_text, derived_text, tags_text)
  values ('delete', old.item_id, old.raw_text, old.derived_text, old.tags_text);
end;

create trigger if not exists trg_item_search_documents_au
after update on item_search_documents
begin
  insert into item_search_fts(item_search_fts, rowid, raw_text, derived_text, tags_text)
  values ('delete', old.item_id, old.raw_text, old.derived_text, old.tags_text);
  insert into item_search_fts(rowid, raw_text, derived_text, tags_text)
  values (new.item_id, new.raw_text, new.derived_text, new.tags_text);
end;

create trigger if not exists trg_inbox_items_ai_search_document
after insert on inbox_items
begin
  insert into item_search_documents(item_id, raw_text, derived_text, tags_text, updated_at)
  values (new.item_id, new.raw_text, '', '', new.inserted_at);
end;

create trigger if not exists trg_inbox_items_au_refresh_search_document
after update of raw_text on inbox_items
begin
  update item_search_documents
  set raw_text = new.raw_text,
      updated_at = (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
  where item_id = new.item_id;
end;

create trigger if not exists trg_item_derivations_ai_refresh_search_document
after insert on item_derivations
begin
  update item_search_documents
  set derived_text = coalesce((
    select trim(
      coalesce(d.summary, '') || ' ' || coalesce(d.category, '') || ' ' || coalesce(d.normalized_text, '')
    )
    from item_derivations d
    where d.item_id = new.item_id
      and d.is_active = 1
      and d.status = 'accepted'
    order by d.derivation_version desc, d.derivation_id desc
    limit 1
  ), ''),
  tags_text = coalesce((
    select group_concat(t.tag_name, ' ')
    from item_tags it
    join tags t on t.tag_id = it.tag_id
    join item_derivations d on d.derivation_id = it.derivation_id
    where d.item_id = new.item_id
      and d.is_active = 1
      and d.status = 'accepted'
  ), ''),
  updated_at = (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
  where item_id = new.item_id;
end;

create trigger if not exists trg_item_derivations_au_refresh_search_document
after update of is_active, status, summary, category, normalized_text on item_derivations
begin
  update item_search_documents
  set derived_text = coalesce((
    select trim(
      coalesce(d.summary, '') || ' ' || coalesce(d.category, '') || ' ' || coalesce(d.normalized_text, '')
    )
    from item_derivations d
    where d.item_id = new.item_id
      and d.is_active = 1
      and d.status = 'accepted'
    order by d.derivation_version desc, d.derivation_id desc
    limit 1
  ), ''),
  tags_text = coalesce((
    select group_concat(t.tag_name, ' ')
    from item_tags it
    join tags t on t.tag_id = it.tag_id
    join item_derivations d on d.derivation_id = it.derivation_id
    where d.item_id = new.item_id
      and d.is_active = 1
      and d.status = 'accepted'
  ), ''),
  updated_at = (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
  where item_id = new.item_id;
end;

create trigger if not exists trg_item_derivations_ad_refresh_search_document
after delete on item_derivations
begin
  update item_search_documents
  set derived_text = coalesce((
    select trim(
      coalesce(d.summary, '') || ' ' || coalesce(d.category, '') || ' ' || coalesce(d.normalized_text, '')
    )
    from item_derivations d
    where d.item_id = old.item_id
      and d.is_active = 1
      and d.status = 'accepted'
    order by d.derivation_version desc, d.derivation_id desc
    limit 1
  ), ''),
  tags_text = coalesce((
    select group_concat(t.tag_name, ' ')
    from item_tags it
    join tags t on t.tag_id = it.tag_id
    join item_derivations d on d.derivation_id = it.derivation_id
    where d.item_id = old.item_id
      and d.is_active = 1
      and d.status = 'accepted'
  ), ''),
  updated_at = (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
  where item_id = old.item_id;
end;

create trigger if not exists trg_item_tags_ai_refresh_search_document
after insert on item_tags
begin
  update item_search_documents
  set tags_text = coalesce((
    select group_concat(t.tag_name, ' ')
    from item_tags it
    join tags t on t.tag_id = it.tag_id
    join item_derivations d on d.derivation_id = it.derivation_id
    where d.item_id = item_search_documents.item_id
      and d.is_active = 1
      and d.status = 'accepted'
  ), ''),
  updated_at = (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
  where item_id = (select d.item_id from item_derivations d where d.derivation_id = new.derivation_id);
end;

create trigger if not exists trg_item_tags_ad_refresh_search_document
after delete on item_tags
begin
  update item_search_documents
  set tags_text = coalesce((
    select group_concat(t.tag_name, ' ')
    from item_tags it
    join tags t on t.tag_id = it.tag_id
    join item_derivations d on d.derivation_id = it.derivation_id
    where d.item_id = item_search_documents.item_id
      and d.is_active = 1
      and d.status = 'accepted'
  ), ''),
  updated_at = (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
  where item_id = (select d.item_id from item_derivations d where d.derivation_id = old.derivation_id);
end;
