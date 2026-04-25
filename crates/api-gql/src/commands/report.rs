use std::io::Write;
use std::path::{Path, PathBuf};

use api_testing_core::cli_io;
use api_testing_core::cli_report::{self, ReportMetadata, ReportMetadataConfig};
use api_testing_core::cli_util::{
    bool_from_env, maybe_relpath, parse_u64_default, shell_quote, trim_non_empty,
};

use crate::cli::{CallArgs, ReportArgs};
use crate::commands::call::cmd_call_internal;

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

    let op_path = PathBuf::from(&args.op);
    if !op_path.is_file() {
        let _ = writeln!(stderr, "Operation file not found: {}", op_path.display());
        return 1;
    }

    let vars_path = args
        .vars
        .as_deref()
        .and_then(trim_non_empty)
        .map(PathBuf::from);
    if let Some(vp) = vars_path.as_deref()
        && !vp.is_file()
    {
        let _ = writeln!(stderr, "Variables file not found: {}", vp.display());
        return 1;
    }

    let ReportMetadata {
        project_root,
        out_path,
        report_date,
        generated_at,
    } = cli_report::build_report_metadata(ReportMetadataConfig {
        case_name,
        out_path: args.out.as_deref(),
        project_root: args.project_root.as_deref(),
        report_dir_env: "GQL_REPORT_DIR",
        invocation_dir,
    });

    let include_command = !args.no_command
        && bool_from_env(
            std::env::var("GQL_REPORT_INCLUDE_COMMAND_ENABLED").ok(),
            "GQL_REPORT_INCLUDE_COMMAND_ENABLED",
            true,
            Some("api-gql"),
            stderr,
        );
    let include_command_url = !args.no_command_url
        && bool_from_env(
            std::env::var("GQL_REPORT_COMMAND_LOG_URL_ENABLED").ok(),
            "GQL_REPORT_COMMAND_LOG_URL_ENABLED",
            true,
            Some("api-gql"),
            stderr,
        );

    let allow_empty = args.allow_empty
        || bool_from_env(
            std::env::var("GQL_ALLOW_EMPTY_ENABLED").ok(),
            "GQL_ALLOW_EMPTY_ENABLED",
            false,
            Some("api-gql"),
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
        "Endpoint: (implicit; see GQL_URL / GQL_ENV_DEFAULT)",
    );

    let op_content = match std::fs::read_to_string(&op_path) {
        Ok(v) => v,
        Err(_) => {
            let _ = writeln!(
                stderr,
                "error: failed to read operation file: {}",
                op_path.display()
            );
            return 1;
        }
    };

    let vars_min_limit = parse_u64_default(std::env::var("GQL_VARS_MIN_LIMIT").ok(), 5, 0);
    let (variables_note, variables_json_value) = match vars_path.as_deref() {
        None => (None, serde_json::json!({})),
        Some(p) => {
            match api_testing_core::graphql::vars::GraphqlVariablesFile::load(p, vars_min_limit) {
                Ok(v) => {
                    let note = if vars_min_limit > 0 && v.bumped_limit_fields > 0 {
                        Some(format!(
                            "> NOTE: variables normalized: bumped {} limit field(s) to at least {} (GQL_VARS_MIN_LIMIT).",
                            v.bumped_limit_fields, vars_min_limit
                        ))
                    } else {
                        None
                    };
                    (note, v.variables)
                }
                Err(err) => {
                    let _ = writeln!(stderr, "{err}");
                    return 1;
                }
            }
        }
    };

    let mut variables_json_value = variables_json_value;
    if !args.no_redact {
        let _ = api_testing_core::redact::redact_json(&mut variables_json_value);
    }
    let variables_json =
        api_testing_core::markdown::format_json_pretty_sorted(&variables_json_value)
            .unwrap_or_else(|_| variables_json_value.to_string());

    let mut response_note: Option<String> = None;
    let response_raw: Vec<u8> = if args.run {
        let mut run_stdout = Vec::new();
        let mut run_stderr = Vec::new();
        let call_args = CallArgs {
            env: args.env.clone(),
            url: args.url.clone(),
            jwt: args.jwt.clone(),
            config_dir: args.config_dir.clone(),
            list_envs: false,
            list_jwts: false,
            no_history: true,
            operation: Some(args.op.clone()),
            variables: args.vars.clone(),
        };
        let run_exit_code = cmd_call_internal(
            &call_args,
            invocation_dir,
            false,
            false,
            &mut run_stdout,
            &mut run_stderr,
        );
        if run_exit_code != 0 {
            let _ = stderr.write_all(&run_stderr);
            return 1;
        }
        run_stdout
    } else {
        match args.response.as_deref().and_then(trim_non_empty) {
            Some(resp) => {
                let mut stdin = std::io::stdin();
                match cli_io::read_response_bytes(&resp, &mut stdin) {
                    Ok(v) => v,
                    Err(err) => {
                        let _ = writeln!(stderr, "{err}");
                        return 1;
                    }
                }
            }
            None if allow_empty => {
                response_note = Some("> NOTE: run the operation and replace this section with the real response (formatted JSON).".to_string());
                serde_json::to_vec(&serde_json::json!({})).unwrap_or_default()
            }
            None => {
                let _ = writeln!(stderr, "error: Use either --run or --response.");
                return 1;
            }
        }
    };

    let (response_lang, response_body, response_json_for_eval) =
        match serde_json::from_slice::<serde_json::Value>(&response_raw) {
            Ok(v) => {
                let eval_json = v.clone();
                let mut display_json = v;
                if !args.no_redact {
                    let _ = api_testing_core::redact::redact_json(&mut display_json);
                }
                let pretty = api_testing_core::markdown::format_json_pretty_sorted(&display_json)
                    .unwrap_or_else(|_| display_json.to_string());
                ("json".to_string(), pretty, Some(eval_json))
            }
            Err(_) => (
                "text".to_string(),
                String::from_utf8_lossy(&response_raw).to_string(),
                None,
            ),
        };

    if !allow_empty {
        if !args.run && args.response.as_deref().and_then(trim_non_empty).is_none() {
            let _ = writeln!(
                stderr,
                "Refusing to write a report without a real response. Use --run or --response (or pass --allow-empty for an intentionally empty/draft report)."
            );
            return 1;
        }

        if response_json_for_eval.is_none() {
            let _ = writeln!(
                stderr,
                "Response is not JSON; refusing to write a no-data report. Re-run with --allow-empty if this is expected."
            );
            return 1;
        }

        if !response_has_meaningful_data_records(response_json_for_eval.as_ref().expect("json")) {
            let _ = writeln!(
                stderr,
                "Response appears to contain no data records; refusing to write report. Adjust query/variables to return at least one record, or pass --allow-empty if an empty result is expected."
            );
            return 1;
        }
    }

    let result_note = if args.run {
        "Result: PASS".to_string()
    } else if args.response.as_deref().and_then(trim_non_empty).is_some() {
        "Result: (response provided; request not executed)".to_string()
    } else {
        "Result: (not executed)".to_string()
    };

    let command_snippet = if include_command {
        Some(build_report_command_snippet(
            args,
            &project_root,
            include_command_url,
        ))
    } else {
        None
    };

    let report = api_testing_core::graphql::report::GraphqlReport {
        report_date,
        case_name: case_name.to_string(),
        generated_at,
        endpoint_note,
        result_note,
        command_snippet,
        operation: op_content,
        variables_note,
        variables_json,
        response_note,
        response_lang,
        response_body,
    };

    let markdown = api_testing_core::graphql::report::render_graphql_report_markdown(&report);
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

fn response_has_meaningful_data_records(response: &serde_json::Value) -> bool {
    let data = response.get("data");
    let Some(data) = data else {
        return false;
    };
    if data.is_null() {
        return false;
    }

    const META_KEYS: &[&str] = &[
        "__typename",
        "pageinfo",
        "totalcount",
        "count",
        "cursor",
        "edges",
        "nodes",
        "hasnextpage",
        "haspreviouspage",
        "startcursor",
        "endcursor",
    ];

    #[derive(Debug, Clone)]
    enum PathElem {
        Key(String),
        Index,
    }

    fn is_meta_key(key: &str) -> bool {
        let k = key.trim().to_ascii_lowercase();
        META_KEYS.iter().any(|m| *m == k)
    }

    fn key_for_path(path: &[PathElem]) -> Option<String> {
        if path.is_empty() {
            return None;
        }
        match path.last().expect("non-empty") {
            PathElem::Key(k) => Some(k.clone()),
            PathElem::Index => match path.iter().rev().nth(1) {
                Some(PathElem::Key(k)) => Some(k.clone()),
                _ => None,
            },
        }
    }

    fn walk(value: &serde_json::Value, path: &mut Vec<PathElem>) -> bool {
        match value {
            serde_json::Value::Null => false,
            serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_) => {
                let Some(k) = key_for_path(path) else {
                    return false;
                };
                !is_meta_key(&k)
            }
            serde_json::Value::Array(values) => {
                for v in values.iter() {
                    path.push(PathElem::Index);
                    if walk(v, path) {
                        return true;
                    }
                    path.pop();
                }
                false
            }
            serde_json::Value::Object(map) => {
                for (k, v) in map {
                    path.push(PathElem::Key(k.clone()));
                    if walk(v, path) {
                        return true;
                    }
                    path.pop();
                }
                false
            }
        }
    }

    walk(data, &mut Vec::new())
}

fn build_report_command_snippet(
    args: &ReportArgs,
    project_root: &Path,
    include_command_url: bool,
) -> String {
    let op_arg = PathBuf::from(&args.op);
    let op_arg = if op_arg.is_absolute() {
        maybe_relpath(&op_arg, project_root)
    } else {
        args.op.clone()
    };

    let vars_arg = args
        .vars
        .as_deref()
        .and_then(trim_non_empty)
        .map(PathBuf::from);
    let vars_arg = vars_arg.map(|p| {
        if p.is_absolute() {
            maybe_relpath(&p, project_root)
        } else {
            p.to_string_lossy().to_string()
        }
    });

    let config_arg = args
        .config_dir
        .as_deref()
        .and_then(trim_non_empty)
        .map(PathBuf::from);
    let config_arg = config_arg.map(|p| {
        if p.is_absolute() {
            maybe_relpath(&p, project_root)
        } else {
            p.to_string_lossy().to_string()
        }
    });

    let mut out = String::new();
    out.push_str("api-gql call \\\n");
    if let Some(cfg) = config_arg {
        out.push_str(&format!("  --config-dir {} \\\n", shell_quote(&cfg)));
    }

    if let Some(url) = args.url.as_deref().and_then(trim_non_empty) {
        let value = if include_command_url {
            url
        } else {
            "<omitted>".to_string()
        };
        out.push_str(&format!("  --url {} \\\n", shell_quote(&value)));
    }
    if let Some(env) = args.env.as_deref().and_then(trim_non_empty)
        && args.url.as_deref().and_then(trim_non_empty).is_none()
    {
        out.push_str(&format!("  --env {} \\\n", shell_quote(&env)));
    }
    if let Some(jwt) = args.jwt.as_deref().and_then(trim_non_empty) {
        out.push_str(&format!("  --jwt {} \\\n", shell_quote(&jwt)));
    }

    out.push_str(&format!("  {} \\\n", shell_quote(&op_arg)));
    if let Some(vars) = vars_arg {
        out.push_str(&format!("  {} \\\n", shell_quote(&vars)));
    }
    out.push_str("| jq .\n");
    out
}
