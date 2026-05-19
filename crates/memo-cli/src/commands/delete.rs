use serde_json::json;

use crate::cli::{DeleteArgs, OutputFormat};
use crate::errors::AppError;
use crate::output::{emit_data, format_item_id, parse_item_id, text};
use crate::storage::Storage;
use crate::storage::repository;

pub fn run(storage: &Storage, args: &DeleteArgs, format: OutputFormat) -> Result<(), AppError> {
    if !args.hard {
        return Err(AppError::usage(
            "delete requires --hard because only hard delete is supported",
        ));
    }

    let item_id = parse_item_id(&args.item_id)
        .ok_or_else(|| AppError::usage("delete requires a valid item_id"))?;

    let deleted = storage.with_transaction(|tx| repository::delete_item_hard(tx, item_id))?;

    if format.is_json() {
        return emit_data(
            "cli.memo-cli.delete.v1",
            json!({
                "item_id": format_item_id(deleted.item_id),
                "deleted": true,
                "deleted_at": deleted.deleted_at,
                "removed_derivations": deleted.removed_derivations,
                "removed_workflow_anchors": deleted.removed_workflow_anchors,
            }),
        );
    }

    text::print_delete(
        deleted.item_id,
        &deleted.deleted_at,
        deleted.removed_derivations,
        deleted.removed_workflow_anchors,
    );
    Ok(())
}
