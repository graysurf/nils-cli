use std::io::Write;
use std::path::{Path, PathBuf};

use crate::{Result, auth_env::CliAuthSource, cli_util, history};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestCallHistoryAuth<'a> {
    None,
    HeaderOnly {
        key: &'a str,
        value: &'a str,
    },
    HeaderAndFlag {
        header_key: &'a str,
        header_value: &'a str,
        flag_name: &'a str,
        flag_value: &'a str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestCallHistoryFlag<'a> {
    pub name: &'a str,
    pub value: Option<&'a str>,
    pub quote_value: bool,
}

impl<'a> RequestCallHistoryFlag<'a> {
    pub const fn option(name: &'a str, value: &'a str) -> Self {
        Self {
            name,
            value: Some(value),
            quote_value: true,
        }
    }

    pub const fn raw(name: &'a str, value: &'a str) -> Self {
        Self {
            name,
            value: Some(value),
            quote_value: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestCallHistoryRecord<'a> {
    pub stamp: &'a str,
    pub exit_code: i32,
    pub setup_dir: &'a Path,
    pub invocation_dir: &'a Path,
    pub command_name: &'a str,
    pub endpoint_label_used: &'a str,
    pub endpoint_value_used: &'a str,
    pub log_url: bool,
    pub auth: RequestCallHistoryAuth<'a>,
    pub request_arg: &'a str,
    pub extra_flags: &'a [RequestCallHistoryFlag<'a>],
}

#[derive(Debug, Clone, Copy)]
pub struct RequestCallHistoryAppend<'a> {
    pub enabled: bool,
    pub history_writer: &'a history::HistoryWriter,
    pub exit_code: i32,
    pub setup_dir: &'a Path,
    pub invocation_dir: &'a Path,
    pub command_name: &'a str,
    pub endpoint_label_used: &'a str,
    pub endpoint_value_used: &'a str,
    pub log_url: bool,
    pub auth_source: &'a CliAuthSource,
    pub token_name_for_log: &'a str,
    pub request_arg: &'a str,
    pub extra_flags: &'a [RequestCallHistoryFlag<'a>],
    pub warning_label: &'a str,
}

pub fn resolve_history_file<F>(
    cwd: &Path,
    config_dir: Option<&Path>,
    file_override_arg: Option<&str>,
    env_override_var: &str,
    resolve_setup_dir: F,
    default_filename: &str,
) -> Result<PathBuf>
where
    F: FnOnce(&Path, Option<&Path>) -> Result<PathBuf>,
{
    let setup_dir = resolve_setup_dir(cwd, config_dir)?;
    let file_override = file_override_arg
        .and_then(cli_util::trim_non_empty)
        .or_else(|| {
            std::env::var(env_override_var)
                .ok()
                .and_then(|s| cli_util::trim_non_empty(&s))
        });
    let file_override = file_override.as_deref().map(Path::new);

    Ok(history::resolve_history_file(
        &setup_dir,
        file_override,
        default_filename,
    ))
}

pub fn run_history_command(
    history_file: &Path,
    tail: Option<u32>,
    command_only: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    if !history_file.is_file() {
        let _ = writeln!(stderr, "History file not found: {}", history_file.display());
        return 1;
    }

    let records = match history::read_records(history_file) {
        Ok(v) => v,
        Err(err) => {
            let _ = writeln!(stderr, "{err}");
            return 1;
        }
    };
    if records.is_empty() {
        return 3;
    }

    let n = tail.unwrap_or(1).max(1) as usize;
    let start = records.len().saturating_sub(n);
    for record in &records[start..] {
        if command_only && record.starts_with('#') {
            let trimmed = record
                .split_once('\n')
                .map(|(_first, rest)| rest)
                .unwrap_or_default();
            let _ = stdout.write_all(trimmed.as_bytes());
            if trimmed.is_empty() {
                let _ = stdout.write_all(b"\n\n");
            }
        } else {
            let _ = stdout.write_all(record.as_bytes());
        }
    }

    0
}

pub fn append_request_call_history_best_effort(
    spec: RequestCallHistoryAppend<'_>,
    stderr: &mut dyn Write,
) {
    if !spec.enabled {
        return;
    }

    let stamp = cli_util::history_timestamp_now().unwrap_or_default();
    let auth = match spec.auth_source {
        CliAuthSource::TokenProfile => RequestCallHistoryAuth::HeaderAndFlag {
            header_key: "token",
            header_value: spec.token_name_for_log,
            flag_name: "token",
            flag_value: spec.token_name_for_log,
        },
        CliAuthSource::EnvFallback { env_name } => RequestCallHistoryAuth::HeaderOnly {
            key: "auth",
            value: env_name,
        },
        CliAuthSource::None => RequestCallHistoryAuth::None,
    };

    let record = build_request_call_history_record(RequestCallHistoryRecord {
        stamp: &stamp,
        exit_code: spec.exit_code,
        setup_dir: spec.setup_dir,
        invocation_dir: spec.invocation_dir,
        command_name: spec.command_name,
        endpoint_label_used: spec.endpoint_label_used,
        endpoint_value_used: spec.endpoint_value_used,
        log_url: spec.log_url,
        auth,
        request_arg: spec.request_arg,
        extra_flags: spec.extra_flags,
    });

    if let Err(err) = spec.history_writer.append(&record) {
        let warning_label = spec.warning_label.trim();
        let warning_label = if warning_label.is_empty() {
            spec.command_name
        } else {
            warning_label
        };
        let _ = writeln!(
            stderr,
            "warning: failed to append {warning_label} history: {err}"
        );
    }
}

pub fn build_request_call_history_record(spec: RequestCallHistoryRecord<'_>) -> String {
    build_request_call_history_record_with_extra_args(spec, &[])
}

pub fn build_request_call_history_record_with_extra_args(
    spec: RequestCallHistoryRecord<'_>,
    extra_args: &[&str],
) -> String {
    let setup_rel = cli_util::maybe_relpath(spec.setup_dir, spec.invocation_dir);
    let config_rel = cli_util::shell_quote(&setup_rel);
    let request_rel = relative_cli_arg(spec.request_arg, spec.invocation_dir);

    let mut record = String::new();
    record.push_str(&format!(
        "# {} exit={} setup_dir={setup_rel}",
        spec.stamp, spec.exit_code
    ));

    if !spec.endpoint_label_used.is_empty() {
        if spec.endpoint_label_used == "url" && !spec.log_url {
            record.push_str(" url=<omitted>");
        } else {
            record.push_str(&format!(
                " {}={}",
                spec.endpoint_label_used, spec.endpoint_value_used
            ));
        }
    }

    match spec.auth {
        RequestCallHistoryAuth::None => {}
        RequestCallHistoryAuth::HeaderOnly { key, value } => {
            if !value.is_empty() {
                record.push_str(&format!(" {key}={value}"));
            }
        }
        RequestCallHistoryAuth::HeaderAndFlag {
            header_key,
            header_value,
            ..
        } => {
            if !header_value.is_empty() {
                record.push_str(&format!(" {header_key}={header_value}"));
            }
        }
    }

    record.push('\n');
    record.push_str(&format!("{} call \\\n", spec.command_name));
    record.push_str(&format!("  --config-dir {config_rel} \\\n"));

    if spec.endpoint_label_used == "env" && !spec.endpoint_value_used.is_empty() {
        record.push_str(&format!(
            "  --env {} \\\n",
            cli_util::shell_quote(spec.endpoint_value_used)
        ));
    } else if spec.endpoint_label_used == "url"
        && !spec.endpoint_value_used.is_empty()
        && spec.log_url
    {
        record.push_str(&format!(
            "  --url {} \\\n",
            cli_util::shell_quote(spec.endpoint_value_used)
        ));
    }

    if let RequestCallHistoryAuth::HeaderAndFlag {
        flag_name,
        flag_value,
        ..
    } = spec.auth
        && !flag_value.is_empty()
    {
        record.push_str(&format!(
            "  --{flag_name} {} \\\n",
            cli_util::shell_quote(flag_value)
        ));
    }

    for flag in spec.extra_flags {
        match flag.value {
            Some(value) => {
                let rendered_value = if flag.quote_value {
                    cli_util::shell_quote(value)
                } else {
                    value.to_string()
                };
                record.push_str(&format!("  --{} {} \\\n", flag.name, rendered_value));
            }
            None => {
                record.push_str(&format!("  --{} \\\n", flag.name));
            }
        }
    }

    record.push_str(&format!("  {} \\\n", cli_util::shell_quote(&request_rel)));
    for arg in extra_args {
        let rel = relative_cli_arg(arg, spec.invocation_dir);
        record.push_str(&format!("  {} \\\n", cli_util::shell_quote(&rel)));
    }
    record.push_str("| jq .\n\n");
    record
}

fn relative_cli_arg(arg: &str, invocation_dir: &Path) -> String {
    let path = Path::new(arg);
    if path.is_absolute() {
        cli_util::maybe_relpath(path, invocation_dir)
    } else {
        arg.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        RequestCallHistoryAuth, RequestCallHistoryFlag, RequestCallHistoryRecord,
        build_request_call_history_record, build_request_call_history_record_with_extra_args,
    };
    use pretty_assertions::assert_eq;
    use std::path::Path;

    #[test]
    fn request_call_history_renders_env_token_command() {
        let record = build_request_call_history_record(RequestCallHistoryRecord {
            stamp: "2026-03-06T10:00:00Z",
            exit_code: 0,
            setup_dir: Path::new("/tmp/ws/setup/rest"),
            invocation_dir: Path::new("/tmp/ws"),
            command_name: "api-rest",
            endpoint_label_used: "env",
            endpoint_value_used: "local",
            log_url: true,
            auth: RequestCallHistoryAuth::HeaderAndFlag {
                header_key: "token",
                header_value: "default",
                flag_name: "token",
                flag_value: "default",
            },
            request_arg: "requests/health.request.json",
            extra_flags: &[],
        });

        assert_eq!(
            record,
            concat!(
                "# 2026-03-06T10:00:00Z exit=0 setup_dir=setup/rest env=local token=default\n",
                "api-rest call \\\n",
                "  --config-dir 'setup/rest' \\\n",
                "  --env 'local' \\\n",
                "  --token 'default' \\\n",
                "  'requests/health.request.json' \\\n",
                "| jq .\n\n",
            )
        );
    }

    #[test]
    fn request_call_history_omits_logged_url_and_rewrites_absolute_request_path() {
        let record = build_request_call_history_record(RequestCallHistoryRecord {
            stamp: "2026-03-06T10:00:00Z",
            exit_code: 7,
            setup_dir: Path::new("/tmp/ws/setup/grpc"),
            invocation_dir: Path::new("/tmp/ws"),
            command_name: "api-grpc",
            endpoint_label_used: "url",
            endpoint_value_used: "127.0.0.1:50051",
            log_url: false,
            auth: RequestCallHistoryAuth::HeaderOnly {
                key: "auth",
                value: "ACCESS_TOKEN",
            },
            request_arg: "/tmp/ws/requests/health.grpc.json",
            extra_flags: &[],
        });

        assert_eq!(
            record,
            concat!(
                "# 2026-03-06T10:00:00Z exit=7 setup_dir=setup/grpc url=<omitted> auth=ACCESS_TOKEN\n",
                "api-grpc call \\\n",
                "  --config-dir 'setup/grpc' \\\n",
                "  'requests/health.grpc.json' \\\n",
                "| jq .\n\n",
            )
        );
    }

    #[test]
    fn request_call_history_appends_extra_flags_before_request_arg() {
        let extra_flags = [RequestCallHistoryFlag::raw("format", "json")];
        let record = build_request_call_history_record(RequestCallHistoryRecord {
            stamp: "2026-03-06T10:00:00Z",
            exit_code: 0,
            setup_dir: Path::new("/tmp/ws/setup/websocket"),
            invocation_dir: Path::new("/tmp/ws"),
            command_name: "api-websocket",
            endpoint_label_used: "",
            endpoint_value_used: "",
            log_url: true,
            auth: RequestCallHistoryAuth::None,
            request_arg: "requests/health.ws.json",
            extra_flags: &extra_flags,
        });

        assert_eq!(
            record,
            concat!(
                "# 2026-03-06T10:00:00Z exit=0 setup_dir=setup/websocket\n",
                "api-websocket call \\\n",
                "  --config-dir 'setup/websocket' \\\n",
                "  --format json \\\n",
                "  'requests/health.ws.json' \\\n",
                "| jq .\n\n",
            )
        );
    }

    #[test]
    fn request_call_history_appends_extra_positional_args_after_request_arg() {
        let extra_args = ["vars.json"];
        let record = build_request_call_history_record_with_extra_args(
            RequestCallHistoryRecord {
                stamp: "2026-03-06T10:00:00Z",
                exit_code: 0,
                setup_dir: Path::new("/tmp/ws/setup/graphql"),
                invocation_dir: Path::new("/tmp/ws"),
                command_name: "api-gql",
                endpoint_label_used: "url",
                endpoint_value_used: "https://api.example/graphql",
                log_url: true,
                auth: RequestCallHistoryAuth::HeaderAndFlag {
                    header_key: "jwt",
                    header_value: "admin",
                    flag_name: "jwt",
                    flag_value: "admin",
                },
                request_arg: "q.graphql",
                extra_flags: &[],
            },
            &extra_args,
        );

        assert_eq!(
            record,
            concat!(
                "# 2026-03-06T10:00:00Z exit=0 setup_dir=setup/graphql url=https://api.example/graphql jwt=admin\n",
                "api-gql call \\\n",
                "  --config-dir 'setup/graphql' \\\n",
                "  --url 'https://api.example/graphql' \\\n",
                "  --jwt 'admin' \\\n",
                "  'q.graphql' \\\n",
                "  'vars.json' \\\n",
                "| jq .\n\n",
            )
        );
    }
}
