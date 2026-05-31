use serde_json::json;

use crate::cli::OutputFormat;
use crate::errors::AppError;
use crate::output::{emit_data, format_item_id, parse_item_id, text};
use crate::storage::Storage;
use crate::storage::repository;

pub fn run(
    storage: &Storage,
    format: OutputFormat,
    limit: usize,
    cursor: Option<&str>,
) -> Result<(), AppError> {
    if limit == 0 {
        return Err(AppError::usage("--limit must be greater than 0"));
    }

    let cursor = if let Some(raw_cursor) = cursor {
        if raw_cursor.trim().is_empty() {
            return Err(AppError::usage("--cursor must be non-empty when provided"));
        }

        let cursor_item_id = parse_item_id(raw_cursor)
            .ok_or_else(|| AppError::invalid_cursor(raw_cursor).with_code("invalid-cursor"))?;
        Some(
            storage
                .with_connection(|conn| repository::lookup_fetch_cursor(conn, cursor_item_id))?
                .ok_or_else(|| AppError::invalid_cursor(raw_cursor))?,
        )
    } else {
        None
    };

    let mut rows = storage.with_connection(|conn| {
        repository::fetch_pending_page(conn, limit.saturating_add(1), cursor.as_ref())
    })?;
    let has_more = rows.len() > limit;
    if has_more {
        rows.truncate(limit);
    }
    let next_cursor = has_more.then(|| {
        rows.last()
            .map(|row| format_item_id(row.item_id))
            .unwrap_or_default()
    });

    if format.is_json() {
        let items = rows
            .iter()
            .map(|row| {
                json!({
                    "item_id": format_item_id(row.item_id),
                    "created_at": row.created_at,
                    "source": row.source,
                    "text": row.text,
                    "state": row.state,
                    "content_type": row.content_type,
                    "validation_status": row.validation_status,
                })
            })
            .collect::<Vec<_>>();

        return emit_data(
            "cli.memo.fetch.v1",
            json!({
                "items": items,
                "pagination": {
                    "limit": limit,
                    "returned": rows.len(),
                    "next_cursor": next_cursor,
                    "has_more": has_more,
                },
            }),
        );
    }

    text::print_fetch(&rows);
    if let Some(next_cursor) = next_cursor {
        eprintln!("next cursor: {next_cursor}");
    }

    Ok(())
}
