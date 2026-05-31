use serde_json::json;

use crate::cli::{AddArgs, OutputFormat};
use crate::errors::AppError;
use crate::output::{emit_data, format_item_id, text};
use crate::storage::Storage;
use crate::storage::repository;
use crate::timestamps::{format_utc, parse_rfc3339_utc};

pub fn run(storage: &Storage, args: &AddArgs, format: OutputFormat) -> Result<(), AppError> {
    let text = args.text.trim();
    if text.is_empty() {
        return Err(AppError::usage("add requires a non-empty text argument"));
    }

    let source = args.source.trim();
    if source.is_empty() {
        return Err(AppError::usage("--source must be non-empty"));
    }

    let created_at = args
        .at
        .as_deref()
        .map(|raw| parse_rfc3339_utc(raw, "--at").map(format_utc))
        .transpose()?;

    let added = storage
        .with_transaction(|tx| repository::add_item(tx, text, source, created_at.as_deref()))?;

    if format.is_json() {
        return emit_data(
            "cli.memo.add.v1",
            json!({
                "item_id": format_item_id(added.item_id),
                "created_at": added.created_at,
                "source": added.source,
                "text": added.text,
            }),
        );
    }

    text::print_add(added.item_id, &added.created_at);
    Ok(())
}
