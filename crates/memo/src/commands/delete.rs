use serde_json::json;

use crate::cli::{DeleteArgs, OutputFormat};
use crate::errors::AppError;
use crate::output::{emit_data, format_item_id, parse_item_id, text};
use crate::storage::Storage;
use crate::storage::repository;
use nils_term::prompt::{self, PromptError, PromptOptions};

pub fn run(storage: &Storage, args: &DeleteArgs, format: OutputFormat) -> Result<(), AppError> {
    let item_id = parse_item_id(&args.item_id)
        .ok_or_else(|| AppError::usage("delete requires a valid item_id"))?;
    let item_id_label = format_item_id(item_id);

    if args.hard && !args.yes {
        return Err(AppError::usage(
            "delete --hard no longer confirms deletion; use --yes to confirm hard delete",
        ));
    }
    if args.hard && !format.is_json() {
        eprintln!("warning: memo delete --hard is deprecated; use --yes instead");
    }

    if !args.yes {
        let question = format!("Permanently delete {item_id_label}? [y/N] ");
        match prompt::confirm(&question, true, PromptOptions::new()) {
            Ok(true) => {}
            Ok(false) => return Err(AppError::runtime("delete cancelled")),
            Err(PromptError::NonInteractive) => {
                return Err(AppError::usage(
                    "delete requires --yes when stdin or stderr is not a TTY",
                ));
            }
            Err(PromptError::Io(err)) => {
                return Err(AppError::runtime(format!(
                    "failed to read delete confirmation: {err}"
                )));
            }
        }
    }

    let deleted = storage.with_transaction(|tx| repository::delete_item_hard(tx, item_id))?;

    if format.is_json() {
        return emit_data(
            "cli.memo.delete.v1",
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
