use serde_json::json;

use crate::cli::OutputFormat;
use crate::errors::AppError;
use crate::output::{emit_data, format_item_id, text};
use crate::storage::Storage;
use crate::storage::repository::{self, QueryState};

pub fn run(
    storage: &Storage,
    format: OutputFormat,
    state: QueryState,
    limit: usize,
    offset: usize,
) -> Result<(), AppError> {
    let rows =
        storage.with_connection(|conn| repository::list_items(conn, state, limit, offset))?;
    let truncated = limit > 0 && rows.len() == limit;

    if format.is_json() {
        let items = rows
            .iter()
            .map(|row| {
                json!({
                    "item_id": format_item_id(row.item_id),
                    "created_at": row.created_at,
                    "state": row.state,
                    "text_preview": row.text_preview,
                    "content_type": row.content_type,
                    "validation_status": row.validation_status,
                })
            })
            .collect::<Vec<_>>();
        return emit_data(
            "cli.memo-cli.list.v1",
            json!({
                "items": items,
                "pagination": {
                    "limit": limit,
                    "offset": offset,
                    "returned": rows.len(),
                    "truncated": truncated,
                },
            }),
        );
    }

    text::print_list(&rows, limit, offset, truncated);

    Ok(())
}
