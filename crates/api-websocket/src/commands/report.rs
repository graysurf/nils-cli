use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::cli::{CallArgs, OutputFormat, ReportArgs, ReportFromCmdArgs};
use crate::commands::call::{
    resolve_auth_for_call, resolve_endpoint_for_call, validate_bearer_token_if_jwt,
};
use api_testing_core::cli_io;
use api_testing_core::cli_report::{self, ReportMetadata, ReportMetadataConfig};
use api_testing_core::cli_util::{bool_from_env, maybe_relpath, shell_quote, trim_non_empty};

pub(crate) fn cmd_report(
    args: &ReportArgs,
    invocation_dir: &Path,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let case_name = args.case.trim();
    if case_name.is_empty() {
        let _ = writeln!(stderr, "error: --case is required");
        return 1;
    }

    let request_path = PathBuf::from(&args.request);
    if !request_path.is_file() {
        let _ = writeln!(stderr, "Request file not found: {}", request_path.display());
        return 1;
    }

    let request_file =
        match api_testing_core::websocket::schema::WebsocketRequestFile::load(&request_path) {
            Ok(v) => v,
            Err(err) => {
                let _ = writeln!(stderr, "{err}");
                return 1;
            }
        };

    let ReportMetadata {
        project_root,
        out_path,
        report_date,
        generated_at,
    } = cli_report::build_report_metadata(ReportMetadataConfig {
        case_name,
        out_path: args.out.as_deref(),
        project_root: args.project_root.as_deref(),
        report_dir_env: "WS_REPORT_DIR",
        invocation_dir,
    });

    let include_command = !args.no_command
        && bool_from_env(
            std::env::var("WS_REPORT_INCLUDE_COMMAND_ENABLED").ok(),
            "WS_REPORT_INCLUDE_COMMAND_ENABLED",
            true,
            Some("api-websocket"),
            stderr,
        );
    let include_command_url = !args.no_command_url
        && bool_from_env(
            std::env::var("WS_REPORT_COMMAND_LOG_URL_ENABLED").ok(),
            "WS_REPORT_COMMAND_LOG_URL_ENABLED",
            true,
            Some("api-websocket"),
            stderr,
        );

    if let Some(parent) = out_path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        let _ = writeln!(
            stderr,
            "error: cannot create report directory {}: {err}",
            parent.display()
        );
        return 1;
    }

    let endpoint_note = cli_report::endpoint_note(
        args.url.as_deref(),
        args.env.as_deref(),
        "Endpoint: (implicit; see WS_URL / WS_ENV_DEFAULT)",
    );

    let mut stderr_note: Option<String> = None;
    let mut run_exit_code: i32 = 0;

    let (transcript_value, last_received) = if args.run {
        let call_args = CallArgs {
            env: args.env.clone(),
            url: args.url.clone(),
            token: args.token.clone(),
            config_dir: args.config_dir.clone(),
            no_history: true,
            format: OutputFormat::Text,
            request: args.request.clone(),
        };

        match run_report_request(&call_args, invocation_dir, stderr) {
            Ok(run) => {
                run_exit_code = if run.ok { 0 } else { 1 };
                if let Some(note) = run.stderr_note {
                    stderr_note = Some(note);
                }
                (run.transcript, run.last_received)
            }
            Err(err) => {
                run_exit_code = 1;
                stderr_note = Some(format!("{err:#}"));
                (serde_json::json!({"transcript": []}), None)
            }
        }
    } else {
        let Some(resp) = args.response.as_deref().and_then(trim_non_empty) else {
            let _ = writeln!(stderr, "error: Use either --run or --response.");
            return 1;
        };
        let mut stdin = std::io::stdin();
        let response_raw = match cli_io::read_response_bytes(&resp, &mut stdin) {
            Ok(v) => v,
            Err(err) => {
                let _ = writeln!(stderr, "{err}");
                return 1;
            }
        };
        parse_transcript_input(&response_raw)
    };

    let request_json = {
        let mut v = request_file.request.raw.clone();
        if !args.no_redact {
            let _ = api_testing_core::redact::redact_json(&mut v);
        }
        api_testing_core::markdown::format_json_pretty_sorted(&v).unwrap_or_else(|_| v.to_string())
    };

    let transcript_json = api_testing_core::markdown::format_json_pretty_sorted(&transcript_value)
        .unwrap_or_else(|_| transcript_value.to_string());

    let command_snippet = if include_command {
        Some(build_report_command_snippet(
            args,
            &project_root,
            include_command_url,
        ))
    } else {
        None
    };

    let result_note = if args.run {
        if run_exit_code == 0 {
            "Result: PASS".to_string()
        } else {
            format!("Result: FAIL (api-websocket exit={run_exit_code})")
        }
    } else {
        "Result: (response provided; request not executed)".to_string()
    };

    let mut assertions: Vec<api_testing_core::websocket::report::WebsocketReportAssertion> =
        Vec::new();
    if let Some(expect) = &request_file.request.expect {
        if let Some(text_contains) = expect.text_contains.as_deref() {
            let state = if args.run {
                if run_exit_code == 0 { "PASS" } else { "FAIL" }
            } else if let Some(last) = last_received.as_deref() {
                if last.contains(text_contains) {
                    "PASS"
                } else {
                    "FAIL"
                }
            } else {
                "NOT_EVALUATED"
            };
            assertions.push(
                api_testing_core::websocket::report::WebsocketReportAssertion {
                    label: format!("expect.textContains: {text_contains}"),
                    state: state.to_string(),
                },
            );
        }

        if let Some(expr) = expect.jq.as_deref() {
            let jq_state = if args.run {
                if run_exit_code == 0 { "PASS" } else { "FAIL" }
            } else if let Some(last) = last_received.as_deref() {
                match serde_json::from_str::<serde_json::Value>(last) {
                    Ok(json) => {
                        if api_testing_core::jq::eval_exit_status(&json, expr).unwrap_or(false) {
                            "PASS"
                        } else {
                            "FAIL"
                        }
                    }
                    Err(_) => "FAIL",
                }
            } else {
                "NOT_EVALUATED"
            };
            assertions.push(
                api_testing_core::websocket::report::WebsocketReportAssertion {
                    label: format!("expect.jq: {expr}"),
                    state: jq_state.to_string(),
                },
            );
        }
    }

    let report = api_testing_core::websocket::report::WebsocketReport {
        report_date,
        case_name: case_name.to_string(),
        generated_at,
        endpoint_note,
        result_note,
        command_snippet,
        assertions,
        request_json,
        transcript_json,
        stderr_note,
    };

    let markdown = api_testing_core::websocket::report::render_websocket_report_markdown(&report);
    if std::fs::write(&out_path, markdown).is_err() {
        let _ = writeln!(
            stderr,
            "error: failed to write report: {}",
            out_path.display()
        );
        return 1;
    }

    let _ = writeln!(stdout, "{}", out_path.display());
    0
}

#[derive(Debug)]
struct ReportRunResult {
    ok: bool,
    transcript: serde_json::Value,
    last_received: Option<String>,
    stderr_note: Option<String>,
}

fn run_report_request(
    args: &CallArgs,
    invocation_dir: &Path,
    stderr: &mut dyn Write,
) -> api_testing_core::Result<ReportRunResult> {
    let request_path = PathBuf::from(&args.request);
    let request_file =
        api_testing_core::websocket::schema::WebsocketRequestFile::load(&request_path)?;

    let config_dir = args
        .config_dir
        .as_deref()
        .and_then(trim_non_empty)
        .map(PathBuf::from);
    let setup_dir = api_testing_core::config::resolve_websocket_setup_dir_for_call(
        invocation_dir,
        invocation_dir,
        &request_path,
        config_dir.as_deref(),
    )?;

    let setup = api_testing_core::config::ResolvedSetup::websocket(setup_dir, None);

    let endpoint = resolve_endpoint_for_call(args, &setup)?;
    let auth = resolve_auth_for_call(args, &setup)?;

    if let Some(token) = auth.bearer_token.as_deref() {
        validate_bearer_token_if_jwt(token, &auth.auth_source_used, &auth.token_name, stderr)?;
    }

    let executed = api_testing_core::websocket::runner::execute_websocket_request(
        &request_file,
        &endpoint.websocket_url,
        auth.bearer_token.as_deref(),
    )?;

    let eval = api_testing_core::websocket::expect::evaluate_main_response(
        &request_file.request,
        &executed,
    );
    let ok = eval.is_ok();
    let stderr_note = eval.err().map(|err| format!("{err}"));

    let transcript = serde_json::json!({
        "target": executed.target,
        "transcript": executed.transcript,
        "lastReceived": executed.last_received,
    });

    Ok(ReportRunResult {
        ok,
        transcript,
        last_received: executed.last_received,
        stderr_note,
    })
}

fn parse_transcript_input(raw: &[u8]) -> (serde_json::Value, Option<String>) {
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(raw) {
        if let Some(obj) = value.as_object()
            && obj.contains_key("transcript")
        {
            let last = obj
                .get("lastReceived")
                .or_else(|| obj.get("last_received"))
                .and_then(|v| v.as_str())
                .map(ToString::to_string)
                .or_else(|| derive_last_received_from_transcript(&value));
            return (value, last);
        }

        let as_text = value
            .as_str()
            .map(ToString::to_string)
            .unwrap_or_else(|| value.to_string());
        let transcript = synthetic_transcript_for_text(&as_text);
        return (transcript, Some(as_text));
    }

    let text = String::from_utf8_lossy(raw).to_string();
    let transcript = synthetic_transcript_for_text(&text);
    (transcript, Some(text))
}

fn derive_last_received_from_transcript(value: &serde_json::Value) -> Option<String> {
    let transcript = value.get("transcript")?.as_array()?;
    for item in transcript.iter().rev() {
        let direction = item
            .get("direction")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if direction == "receive" {
            return item
                .get("payload")
                .and_then(|v| v.as_str())
                .map(ToString::to_string);
        }
    }
    None
}

fn synthetic_transcript_for_text(text: &str) -> serde_json::Value {
    serde_json::json!({
        "target": "",
        "transcript": [
            {
                "direction": "receive",
                "payload": text,
            }
        ],
        "lastReceived": text,
    })
}

pub(crate) fn cmd_report_from_cmd(
    args: &ReportFromCmdArgs,
    invocation_dir: &Path,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let _ = args.allow_empty;

    let response = args.response.as_deref().and_then(trim_non_empty);
    let response_is_stdin = matches!(response.as_deref(), Some("-"));

    if response_is_stdin && args.stdin {
        let _ = writeln!(
            stderr,
            "error: When using --response -, stdin is reserved for the response body; provide the snippet as a positional argument."
        );
        return 1;
    }

    let snippet = if args.stdin {
        let mut buf = Vec::new();
        if std::io::stdin().read_to_end(&mut buf).is_err() {
            let _ = writeln!(stderr, "error: failed to read command snippet from stdin");
            return 1;
        }
        String::from_utf8_lossy(&buf).to_string()
    } else {
        args.snippet.clone().unwrap_or_default()
    };

    let parsed = match api_testing_core::cmd_snippet::parse_report_from_cmd_snippet(&snippet) {
        Ok(v) => v,
        Err(err) => {
            let _ = writeln!(stderr, "error: {err}");
            return 1;
        }
    };

    let api_testing_core::cmd_snippet::ReportFromCmd::Websocket(ws) = parsed else {
        let _ = writeln!(
            stderr,
            "error: expected a WebSocket call snippet (api-websocket/websocket.sh)"
        );
        return 1;
    };

    let api_testing_core::cmd_snippet::WebsocketReportFromCmd {
        case: derived_case,
        config_dir,
        env,
        url,
        token,
        request,
    } = ws;

    let case_name = args
        .case
        .as_deref()
        .and_then(trim_non_empty)
        .unwrap_or_else(|| derived_case.clone());

    let report_args = ReportArgs {
        case: case_name,
        request,
        out: args.out.clone(),
        env,
        url,
        token,
        run: response.is_none(),
        response,
        no_redact: false,
        no_command: false,
        no_command_url: false,
        project_root: None,
        config_dir,
    };

    if args.dry_run {
        let cmd = build_report_from_cmd_dry_run_command(&report_args);
        let _ = writeln!(stdout, "{cmd}");
        return 0;
    }

    cmd_report(&report_args, invocation_dir, stdout, stderr)
}

fn build_report_from_cmd_dry_run_command(args: &ReportArgs) -> String {
    let mut cmd = String::from("api-websocket report");

    cmd.push_str(" --case ");
    cmd.push_str(&shell_quote(&args.case));

    cmd.push_str(" --request ");
    cmd.push_str(&shell_quote(&args.request));

    if let Some(out) = args.out.as_deref().and_then(trim_non_empty) {
        cmd.push_str(" --out ");
        cmd.push_str(&shell_quote(&out));
    }

    if let Some(cfg) = args.config_dir.as_deref().and_then(trim_non_empty) {
        cmd.push_str(" --config-dir ");
        cmd.push_str(&shell_quote(&cfg));
    }

    if let Some(url) = args.url.as_deref().and_then(trim_non_empty) {
        cmd.push_str(" --url ");
        cmd.push_str(&shell_quote(&url));
    }
    if let Some(env) = args.env.as_deref().and_then(trim_non_empty)
        && args.url.as_deref().and_then(trim_non_empty).is_none()
    {
        cmd.push_str(" --env ");
        cmd.push_str(&shell_quote(&env));
    }
    if let Some(token) = args.token.as_deref().and_then(trim_non_empty) {
        cmd.push_str(" --token ");
        cmd.push_str(&shell_quote(&token));
    }

    if args.run {
        cmd.push_str(" --run");
    } else if let Some(resp) = args.response.as_deref().and_then(trim_non_empty) {
        cmd.push_str(" --response ");
        cmd.push_str(&shell_quote(&resp));
    } else {
        cmd.push_str(" --run");
    }

    cmd
}

fn build_report_command_snippet(
    args: &ReportArgs,
    project_root: &Path,
    include_command_url: bool,
) -> String {
    let req_arg = PathBuf::from(&args.request);
    let req_for_cmd = if req_arg.is_absolute() {
        maybe_relpath(&req_arg, project_root)
    } else {
        args.request.clone()
    };

    let mut cmd = String::new();
    cmd.push_str("api-websocket call");

    if let Some(cfg) = args.config_dir.as_deref().and_then(trim_non_empty) {
        cmd.push_str(" --config-dir ");
        cmd.push_str(&shell_quote(&cfg));
    }

    if let Some(url) = args.url.as_deref().and_then(trim_non_empty) {
        if include_command_url {
            cmd.push_str(" --url ");
            cmd.push_str(&shell_quote(&url));
        }
    } else if let Some(env) = args.env.as_deref().and_then(trim_non_empty) {
        cmd.push_str(" --env ");
        cmd.push_str(&shell_quote(&env));
    }

    if let Some(token) = args.token.as_deref().and_then(trim_non_empty) {
        cmd.push_str(" --token ");
        cmd.push_str(&shell_quote(&token));
    }

    cmd.push(' ');
    cmd.push_str(&shell_quote(&req_for_cmd));
    cmd
}
