use serde::Serialize;

use crate::error::CliError;
use crate::model::SuccessEnvelope;

pub mod ax_action;
pub mod ax_attr;
pub mod ax_click;
pub mod ax_common;
pub mod ax_list;
pub mod ax_session;
pub mod ax_type;
pub mod ax_watch;
pub mod input_click;
pub mod input_hotkey;
pub mod input_key;
pub mod input_pointer;
pub mod input_source;
pub mod input_type;
pub mod list;
pub mod observe;
pub mod profile;
pub mod scenario;
pub mod wait;
pub mod window_activate;

const TSV_LIST_ONLY_FORMAT_MESSAGE: &str =
    "--format tsv is only supported for `windows list` and `apps list`";

pub(crate) fn emit_json_success<T>(command: &'static str, result: T) -> Result<(), CliError>
where
    T: Serialize,
{
    let payload = SuccessEnvelope::new(command, result);
    println!(
        "{}",
        serde_json::to_string(&payload)
            .map_err(|err| CliError::runtime(format!("failed to serialize json output: {err}")))?
    );
    Ok(())
}

pub(crate) fn reject_tsv_for_list_only() -> Result<(), CliError> {
    Err(CliError::usage(TSV_LIST_ONLY_FORMAT_MESSAGE))
}
