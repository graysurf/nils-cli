use serde_json::Value;

pub(crate) fn render_success(
    schema_version: &str,
    command: &str,
    payload: &Value,
) -> Result<String, String> {
    let payload_json = serde_json::to_string(payload)
        .map_err(|err| format!("failed to serialize payload: {err}"))?;
    Ok(format!(
        "schema_version: {schema_version}\ncommand: {command}\nstatus: ok\npayload: {payload_json}"
    ))
}

pub(crate) fn render_error(
    schema_version: &str,
    command: &str,
    code: &str,
    message: &str,
) -> String {
    format!(
        "schema_version: {schema_version}\ncommand: {command}\nstatus: error\ncode: {code}\nmessage: {message}"
    )
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
    eprintln!("{}", render_error(schema_version, command, code, message));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn success_renders_schema_command_status_payload_lines() {
        let payload = json!({"item_count": 3});
        let rendered = render_success("cli.plan-issue.list.v1", "list", &payload).expect("render");
        assert_eq!(
            rendered,
            "schema_version: cli.plan-issue.list.v1\ncommand: list\nstatus: ok\npayload: {\"item_count\":3}"
        );
    }

    #[test]
    fn error_renders_schema_command_status_code_message_lines() {
        let rendered = render_error(
            "cli.plan-issue.list.v1",
            "list",
            "missing-issue",
            "issue not found",
        );
        assert_eq!(
            rendered,
            "schema_version: cli.plan-issue.list.v1\ncommand: list\nstatus: error\ncode: missing-issue\nmessage: issue not found"
        );
    }
}
