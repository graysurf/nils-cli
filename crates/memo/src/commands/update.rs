use serde_json::json;

use crate::cli::{OutputFormat, UpdateArgs};
use crate::errors::AppError;
use crate::output::{emit_data, format_item_id, parse_item_id, text};
use crate::storage::Storage;
use crate::storage::repository;

pub fn run(storage: &Storage, args: &UpdateArgs, format: OutputFormat) -> Result<(), AppError> {
    let item_id = parse_item_id(&args.item_id)
        .ok_or_else(|| AppError::usage("update requires a valid item_id"))?;
    let text = args.text.trim();
    if text.is_empty() {
        return Err(AppError::usage("update requires a non-empty text argument"));
    }

    let updated = storage.with_transaction(|tx| repository::update_item(tx, item_id, text))?;

    if format.is_json() {
        return emit_data(
            "cli.memo.update.v1",
            json!({
                "item_id": format_item_id(updated.item_id),
                "updated_at": updated.updated_at,
                "text": updated.text,
                "state": "pending",
                "cleared_derivations": updated.cleared_derivations,
                "cleared_workflow_anchors": updated.cleared_workflow_anchors,
            }),
        );
    }

    text::print_update(
        updated.item_id,
        &updated.updated_at,
        updated.cleared_derivations,
        updated.cleared_workflow_anchors,
    );
    Ok(())
}
