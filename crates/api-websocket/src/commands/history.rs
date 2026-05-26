use std::io::Write;
use std::path::{Path, PathBuf};

use api_testing_core::cli_history::{resolve_history_file, select_history_records};
use api_testing_core::config;

use crate::cli::{HistoryArgs, OutputFormat};
use api_testing_core::cli_util::trim_non_empty;

const HISTORY_SCHEMA_VERSION: &str = "cli.api-websocket.history.v1";

pub(crate) fn cmd_history(
    args: &HistoryArgs,
    invocation_dir: &Path,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let config_dir = args
        .config_dir
        .as_deref()
        .and_then(trim_non_empty)
        .map(PathBuf::from);
    let history_file = match resolve_history_file(
        invocation_dir,
        config_dir.as_deref(),
        args.file.as_deref(),
        "WS_HISTORY_FILE",
        |cwd, config_dir| {
            config::resolve_websocket_setup_dir_for_history(cwd, invocation_dir, config_dir)
        },
        ".ws_history",
    ) {
        Ok(v) => v,
        Err(err) => {
            return fail_history(
                args,
                stdout,
                stderr,
                1,
                "history_resolve_error",
                &format!("{err}"),
                None,
            );
        }
    };

    if !history_file.is_file() {
        return fail_history(
            args,
            stdout,
            stderr,
            1,
            "history_not_found",
            &format!("History file not found: {}", history_file.display()),
            Some(serde_json::json!({
                "history_file": history_file.to_string_lossy(),
            })),
        );
    }

    let records = match api_testing_core::history::read_records(&history_file) {
        Ok(v) => v,
        Err(err) => {
            return fail_history(
                args,
                stdout,
                stderr,
                1,
                "history_read_error",
                &format!("{err}"),
                None,
            );
        }
    };

    if records.is_empty() {
        return fail_history(
            args,
            stdout,
            stderr,
            3,
            "history_empty",
            "History file is empty",
            Some(serde_json::json!({
                "history_file": history_file.to_string_lossy(),
            })),
        );
    }

    let tail = if args.last { Some(1) } else { args.tail };
    let selected = select_history_records(&records, tail, args.command_only);

    if matches!(args.format, OutputFormat::Json) {
        let payload = serde_json::json!({
            "schema_version": HISTORY_SCHEMA_VERSION,
            "ok": true,
            "data": {
                "history_file": history_file.to_string_lossy(),
                "count": selected.len(),
                "records": selected,
            }
        });
        let _ = serde_json::to_writer_pretty(&mut *stdout, &payload);
        let _ = stdout.write_all(b"\n");
        return 0;
    }

    for record in selected {
        let _ = stdout.write_all(record.as_bytes());
    }

    0
}

fn fail_history(
    args: &HistoryArgs,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    exit_code: i32,
    error_code: &str,
    message: &str,
    details: Option<serde_json::Value>,
) -> i32 {
    if matches!(args.format, OutputFormat::Json) {
        let mut error = serde_json::json!({
            "code": error_code,
            "message": message,
        });
        if let Some(details) = details {
            error["details"] = details;
        }
        let payload = serde_json::json!({
            "schema_version": HISTORY_SCHEMA_VERSION,
            "ok": false,
            "error": error,
        });
        let _ = serde_json::to_writer_pretty(&mut *stdout, &payload);
        let _ = stdout.write_all(b"\n");
    } else {
        let _ = writeln!(stderr, "{message}");
    }
    exit_code
}
