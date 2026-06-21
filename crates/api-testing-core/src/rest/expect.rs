use anyhow::Context;

use crate::Result;
use crate::rest::runner::RestExecutedRequest;
use crate::rest::schema::RestRequest;

pub fn evaluate_main_response(request: &RestRequest, executed: &RestExecutedRequest) -> Result<()> {
    let status = executed.response.status;

    if let Some(expect) = &request.expect {
        if status != expect.status {
            anyhow::bail!("Expected HTTP status {} but got {}.", expect.status, status);
        }

        if let Some(expr) = &expect.jq {
            let body_json: serde_json::Value = serde_json::from_slice(&executed.response.body)
                .context("expect.jq requires a JSON response body")?;
            if !crate::jq::eval_exit_status(&body_json, expr).unwrap_or(false) {
                anyhow::bail!("expect.jq failed: {expr}");
            }
        }

        return Ok(());
    }

    if !(200..300).contains(&status) {
        anyhow::bail!(
            "HTTP request failed with status {status}: {} {}",
            executed.method,
            executed.url
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executed_with(status: u16, body: serde_json::Value) -> RestExecutedRequest {
        RestExecutedRequest {
            method: "GET".to_string(),
            url: "http://localhost:6700/health".to_string(),
            response: crate::http::HttpResponse {
                status,
                body: serde_json::to_vec(&body).unwrap(),
                content_type: Some("application/json".to_string()),
            },
        }
    }

    fn executed_with_raw_body(status: u16, body: &[u8], content_type: &str) -> RestExecutedRequest {
        RestExecutedRequest {
            method: "GET".to_string(),
            url: "http://localhost:6700/health".to_string(),
            response: crate::http::HttpResponse {
                status,
                body: body.to_vec(),
                content_type: Some(content_type.to_string()),
            },
        }
    }

    #[test]
    fn rest_expect_status_mismatch_fails() {
        let request = crate::rest::schema::parse_rest_request_json(serde_json::json!({
            "method": "GET",
            "path": "/health",
            "expect": { "status": 200 }
        }))
        .unwrap();
        let executed = executed_with(500, serde_json::json!({"ok": false}));
        let err = evaluate_main_response(&request, &executed).unwrap_err();
        assert!(
            err.to_string()
                .contains("Expected HTTP status 200 but got 500")
        );
    }

    #[test]
    fn rest_expect_jq_false_fails() {
        let request = crate::rest::schema::parse_rest_request_json(serde_json::json!({
            "method": "GET",
            "path": "/health",
            "expect": { "status": 200, "jq": ".ok == true" }
        }))
        .unwrap();
        let executed = executed_with(200, serde_json::json!({"ok": false}));
        let err = evaluate_main_response(&request, &executed).unwrap_err();
        assert!(err.to_string().contains("expect.jq failed"));
    }

    #[test]
    fn rest_expect_jq_non_json_body_reports_parse_error() {
        let request = crate::rest::schema::parse_rest_request_json(serde_json::json!({
            "method": "GET",
            "path": "/health",
            "expect": { "status": 200, "jq": ".ok == true" }
        }))
        .unwrap();
        // A non-JSON response body (e.g. an HTML error page) is a system error,
        // not an assertion failure. It must not be reported as `expect.jq failed`.
        let executed = executed_with_raw_body(200, b"<html>not json</html>", "text/html");
        let err = evaluate_main_response(&request, &executed).unwrap_err();
        let message = format!("{err:#}");
        assert!(
            message.contains("expect.jq requires a JSON response body"),
            "expected a JSON-body parse error, got: {message}"
        );
        assert!(
            !message.contains("expect.jq failed"),
            "a non-JSON body must not be conflated with a jq assertion failure: {message}"
        );
    }

    #[test]
    fn rest_expect_default_non_2xx_fails() {
        let request = crate::rest::schema::parse_rest_request_json(serde_json::json!({
            "method": "GET",
            "path": "/health"
        }))
        .unwrap();
        let executed = executed_with(404, serde_json::json!({"error": "no"}));
        let err = evaluate_main_response(&request, &executed).unwrap_err();
        assert!(
            err.to_string()
                .contains("HTTP request failed with status 404")
        );
    }
}
