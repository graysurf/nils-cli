use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
struct JsonSuccessEnvelope<'a> {
    schema_version: &'a str,
    command: &'a str,
    status: &'a str,
    payload: &'a Value,
}

#[derive(Debug, Serialize)]
struct JsonErrorEnvelope<'a> {
    schema_version: &'a str,
    command: &'a str,
    status: &'a str,
    error: JsonError<'a>,
}

#[derive(Debug, Serialize)]
struct JsonError<'a> {
    code: &'a str,
    message: &'a str,
}

pub(crate) fn render_success(
    schema_version: &str,
    command: &str,
    payload: &Value,
) -> Result<String, String> {
    let envelope = JsonSuccessEnvelope {
        schema_version,
        command,
        status: "ok",
        payload,
    };
    serde_json::to_string(&envelope)
        .map_err(|err| format!("failed to serialize JSON output: {err}"))
}

pub(crate) fn render_error(
    schema_version: &str,
    command: &str,
    code: &str,
    message: &str,
) -> Result<String, String> {
    let envelope = JsonErrorEnvelope {
        schema_version,
        command,
        status: "error",
        error: JsonError { code, message },
    };
    serde_json::to_string(&envelope)
        .map_err(|err| format!("failed to serialize JSON error output: {err}"))
}

pub fn print_success(schema_version: &str, command: &str, payload: &Value) -> Result<(), String> {
    let rendered = render_success(schema_version, command, payload)?;
    println!("{rendered}");
    Ok(())
}

pub fn print_error(
    schema_version: &str,
    command: &str,
    code: &str,
    message: &str,
) -> Result<(), String> {
    let rendered = render_error(schema_version, command, code, message)?;
    println!("{rendered}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn success_envelope_carries_schema_command_status_and_payload() {
        let payload = json!({"item_count": 3});
        let rendered = render_success("cli.plan-issue.list.v1", "list", &payload).expect("render");
        assert_eq!(
            rendered,
            "{\"schema_version\":\"cli.plan-issue.list.v1\",\"command\":\"list\",\"status\":\"ok\",\"payload\":{\"item_count\":3}}"
        );
    }

    #[test]
    fn error_envelope_carries_schema_command_status_and_error_pair() {
        let rendered = render_error(
            "cli.plan-issue.list.v1",
            "list",
            "missing-issue",
            "issue not found",
        )
        .expect("render");
        assert_eq!(
            rendered,
            "{\"schema_version\":\"cli.plan-issue.list.v1\",\"command\":\"list\",\"status\":\"error\",\"error\":{\"code\":\"missing-issue\",\"message\":\"issue not found\"}}"
        );
    }

    #[test]
    fn error_envelope_does_not_emit_data_field() {
        let rendered = render_error("v1", "cmd", "bad-input", "x").expect("render");
        assert!(
            !rendered.contains("\"data\""),
            "must not carry data field: {rendered}"
        );
        assert!(
            !rendered.contains("\"ok\""),
            "must not carry ok field: {rendered}"
        );
    }
}
