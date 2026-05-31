use rusqlite::{Connection, params};
use serde::Serialize;

use crate::errors::AppError;

const INBOX_ITEM_ALLOCATOR_NAME: &str = "inbox_items";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryState {
    All,
    Pending,
    Enriched,
}

#[derive(Debug, Clone, Serialize)]
pub struct AddedItem {
    pub item_id: i64,
    pub created_at: String,
    pub source: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdatedItem {
    pub item_id: i64,
    pub updated_at: String,
    pub text: String,
    pub cleared_derivations: i64,
    pub cleared_workflow_anchors: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeletedItem {
    pub item_id: i64,
    pub deleted_at: String,
    pub removed_derivations: i64,
    pub removed_workflow_anchors: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListItem {
    pub item_id: i64,
    pub created_at: String,
    pub state: String,
    pub text_preview: String,
    pub content_type: Option<String>,
    pub validation_status: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FetchItem {
    pub item_id: i64,
    pub created_at: String,
    pub source: String,
    pub text: String,
    pub state: String,
    pub content_type: Option<String>,
    pub validation_status: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FetchCursor {
    pub item_id: i64,
    pub created_at: String,
}

fn ensure_item_allocator_seeded(conn: &Connection) -> Result<(), AppError> {
    conn.execute(
        "insert into id_allocators(name, last_id)
         values (?1, coalesce((select max(item_id) from inbox_items), 0))
         on conflict(name) do update
         set last_id = max(id_allocators.last_id, excluded.last_id)",
        params![INBOX_ITEM_ALLOCATOR_NAME],
    )
    .map_err(AppError::db_write)?;
    Ok(())
}

fn allocate_next_item_id(conn: &Connection) -> Result<i64, AppError> {
    ensure_item_allocator_seeded(conn)?;
    conn.execute(
        "update id_allocators
         set last_id = last_id + 1
         where name = ?1",
        params![INBOX_ITEM_ALLOCATOR_NAME],
    )
    .map_err(AppError::db_write)?;

    conn.query_row(
        "select last_id from id_allocators where name = ?1",
        params![INBOX_ITEM_ALLOCATOR_NAME],
        |row| row.get(0),
    )
    .map_err(AppError::db_query)
}

pub fn add_item(
    conn: &Connection,
    text: &str,
    source: &str,
    created_at: Option<&str>,
) -> Result<AddedItem, AppError> {
    let item_id = allocate_next_item_id(conn)?;

    if let Some(created_at) = created_at {
        conn.execute(
            "insert into inbox_items(item_id, source, raw_text, created_at)
             values(?1, ?2, ?3, ?4)",
            params![item_id, source, text, created_at],
        )
        .map_err(AppError::db_write)?;
    } else {
        conn.execute(
            "insert into inbox_items(item_id, source, raw_text) values(?1, ?2, ?3)",
            params![item_id, source, text],
        )
        .map_err(AppError::db_write)?;
    }

    let created_at: String = conn
        .query_row(
            "select created_at from inbox_items where item_id = ?1",
            params![item_id],
            |row| row.get(0),
        )
        .map_err(AppError::db_query)?;

    Ok(AddedItem {
        item_id,
        created_at,
        source: source.to_string(),
        text: text.to_string(),
    })
}

pub fn update_item(conn: &Connection, item_id: i64, text: &str) -> Result<UpdatedItem, AppError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(AppError::usage("update requires a non-empty text argument"));
    }

    let exists: i64 = conn
        .query_row(
            "select count(*) from inbox_items where item_id = ?1",
            params![item_id],
            |row| row.get(0),
        )
        .map_err(AppError::db_query)?;
    if exists == 0 {
        return Err(AppError::usage("item_id does not exist")
            .with_code("invalid-item-id")
            .with_details(serde_json::json!({ "item_id": item_id })));
    }

    let removed_workflow_anchors = conn
        .execute(
            "delete from workflow_item_anchors where item_id = ?1",
            params![item_id],
        )
        .map_err(AppError::db_write)? as i64;
    let cleared_derivations = conn
        .execute(
            "delete from item_derivations where item_id = ?1",
            params![item_id],
        )
        .map_err(AppError::db_write)? as i64;
    conn.execute(
        "update inbox_items set raw_text = ?1 where item_id = ?2",
        params![text, item_id],
    )
    .map_err(AppError::db_write)?;
    conn.execute(
        "delete from tags
         where not exists (
           select 1
           from item_tags it
           where it.tag_id = tags.tag_id
         )",
        [],
    )
    .map_err(AppError::db_write)?;

    let updated_at: String = conn
        .query_row(
            "select updated_at from item_search_documents where item_id = ?1",
            params![item_id],
            |row| row.get(0),
        )
        .map_err(AppError::db_query)?;

    Ok(UpdatedItem {
        item_id,
        updated_at,
        text: text.to_string(),
        cleared_derivations,
        cleared_workflow_anchors: removed_workflow_anchors,
    })
}

pub fn delete_item_hard(conn: &Connection, item_id: i64) -> Result<DeletedItem, AppError> {
    let exists: i64 = conn
        .query_row(
            "select count(*) from inbox_items where item_id = ?1",
            params![item_id],
            |row| row.get(0),
        )
        .map_err(AppError::db_query)?;
    if exists == 0 {
        return Err(AppError::usage("item_id does not exist")
            .with_code("invalid-item-id")
            .with_details(serde_json::json!({ "item_id": item_id })));
    }

    let removed_workflow_anchors = conn
        .execute(
            "delete from workflow_item_anchors where item_id = ?1",
            params![item_id],
        )
        .map_err(AppError::db_write)? as i64;
    let removed_derivations = conn
        .execute(
            "delete from item_derivations where item_id = ?1",
            params![item_id],
        )
        .map_err(AppError::db_write)? as i64;
    conn.execute(
        "delete from item_search_documents where item_id = ?1",
        params![item_id],
    )
    .map_err(AppError::db_write)?;
    conn.execute(
        "delete from inbox_items where item_id = ?1",
        params![item_id],
    )
    .map_err(AppError::db_write)?;
    conn.execute(
        "delete from tags
         where not exists (
           select 1
           from item_tags it
           where it.tag_id = tags.tag_id
         )",
        [],
    )
    .map_err(AppError::db_write)?;

    let deleted_at: String = conn
        .query_row("select strftime('%Y-%m-%dT%H:%M:%fZ', 'now')", [], |row| {
            row.get(0)
        })
        .map_err(AppError::db_query)?;

    Ok(DeletedItem {
        item_id,
        deleted_at,
        removed_derivations,
        removed_workflow_anchors,
    })
}

pub fn list_items(
    conn: &Connection,
    state: QueryState,
    limit: usize,
    offset: usize,
) -> Result<Vec<ListItem>, AppError> {
    let state_filter = state_sql(state);
    let sql = format!(
        "select
            i.item_id,
            i.created_at,
            case
                when ad.derivation_id is not null then 'enriched'
                else 'pending'
            end as state,
            substr(i.raw_text, 1, 80) as text_preview,
            json_extract(ad.payload_json, '$.content_type') as content_type,
            json_extract(ad.payload_json, '$.validation_status') as validation_status
        from inbox_items i
        left join item_derivations ad
          on ad.derivation_id = (
            select d.derivation_id
            from item_derivations d
            where d.item_id = i.item_id
              and d.is_active = 1
              and d.status = 'accepted'
            order by d.derivation_version desc, d.derivation_id desc
            limit 1
          )
        where {state_filter}
        order by i.created_at desc, i.item_id desc
        limit ?1 offset ?2"
    );

    let mut stmt = conn.prepare(&sql).map_err(AppError::db_query)?;
    let rows = stmt
        .query_map(params![limit as i64, offset as i64], |row| {
            Ok(ListItem {
                item_id: row.get(0)?,
                created_at: row.get(1)?,
                state: row.get(2)?,
                text_preview: row.get(3)?,
                content_type: row.get(4)?,
                validation_status: row.get(5)?,
            })
        })
        .map_err(AppError::db_query)?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(AppError::db_query)
}

pub fn lookup_fetch_cursor(
    conn: &Connection,
    item_id: i64,
) -> Result<Option<FetchCursor>, AppError> {
    conn.query_row(
        "select item_id, created_at from inbox_items where item_id = ?1",
        params![item_id],
        |row| {
            Ok(FetchCursor {
                item_id: row.get(0)?,
                created_at: row.get(1)?,
            })
        },
    )
    .map(Some)
    .or_else(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(AppError::db_query(other)),
    })
}

pub fn fetch_pending_page(
    conn: &Connection,
    limit: usize,
    cursor: Option<&FetchCursor>,
) -> Result<Vec<FetchItem>, AppError> {
    let mut stmt = conn
        .prepare(
            "select i.item_id, i.created_at, i.source, i.raw_text
                    , null as content_type
                    , null as validation_status
            from inbox_items i
            where not exists (
                select 1 from item_derivations d
                where d.item_id = i.item_id and d.is_active = 1 and d.status = 'accepted'
            )
              and (
                ?1 is null
                or i.created_at < ?2
                or (i.created_at = ?2 and i.item_id < ?1)
              )
            order by i.created_at desc, i.item_id desc
            limit ?3",
        )
        .map_err(AppError::db_query)?;

    let cursor_item_id = cursor.map(|value| value.item_id);
    let cursor_created_at = cursor.map(|value| value.created_at.as_str());

    let rows = stmt
        .query_map(
            params![cursor_item_id, cursor_created_at, limit as i64],
            |row| {
                Ok(FetchItem {
                    item_id: row.get(0)?,
                    created_at: row.get(1)?,
                    source: row.get(2)?,
                    text: row.get(3)?,
                    state: "pending".to_string(),
                    content_type: row.get(4)?,
                    validation_status: row.get(5)?,
                })
            },
        )
        .map_err(AppError::db_query)?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(AppError::db_query)
}

fn state_sql(state: QueryState) -> &'static str {
    match state {
        QueryState::All => "1 = 1",
        QueryState::Pending => {
            "not exists (
                select 1 from item_derivations d
                where d.item_id = i.item_id and d.is_active = 1 and d.status = 'accepted'
            )"
        }
        QueryState::Enriched => {
            "exists (
                select 1 from item_derivations d
                where d.item_id = i.item_id and d.is_active = 1 and d.status = 'accepted'
            )"
        }
    }
}
