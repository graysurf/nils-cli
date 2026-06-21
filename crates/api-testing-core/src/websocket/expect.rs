use anyhow::Context;

use crate::Result;
use crate::websocket::runner::WebsocketExecutedRequest;
use crate::websocket::schema::{WebsocketExpect, WebsocketRequest};

pub fn evaluate_text_expect(expect: &WebsocketExpect, text: &str, label: &str) -> Result<()> {
    if let Some(contains) = expect.text_contains.as_deref()
        && !text.contains(contains)
    {
        anyhow::bail!("{label} textContains failed: expected to contain '{contains}'");
    }

    if let Some(expr) = expect.jq.as_deref() {
        let json: serde_json::Value = serde_json::from_str(text)
            .with_context(|| format!("{label} jq requires a JSON response text"))?;
        if !crate::jq::eval_exit_status(&json, expr).unwrap_or(false) {
            anyhow::bail!("{label} jq failed: {expr}");
        }
    }

    Ok(())
}

pub fn evaluate_main_response(
    request: &WebsocketRequest,
    executed: &WebsocketExecutedRequest,
) -> Result<()> {
    let Some(expect) = request.expect.as_ref() else {
        return Ok(());
    };

    let last = executed.last_received.as_deref().ok_or_else(|| {
        anyhow::anyhow!("websocket expect requires at least one received message")
    })?;

    evaluate_text_expect(expect, last, "websocket expect")
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn request_with_expect(expect: Option<WebsocketExpect>) -> WebsocketRequest {
        WebsocketRequest {
            url: None,
            headers: Vec::new(),
            connect_timeout_seconds: None,
            steps: Vec::new(),
            expect,
            raw: serde_json::json!({}),
        }
    }

    fn executed_with_text(text: Option<&str>) -> WebsocketExecutedRequest {
        WebsocketExecutedRequest {
            target: "ws://127.0.0.1:9001/ws".to_string(),
            transcript: Vec::new(),
            last_received: text.map(ToString::to_string),
        }
    }

    #[test]
    fn websocket_expect_accepts_matching_text_contains_and_jq() {
        let expect = WebsocketExpect {
            jq: Some(".ok == true".to_string()),
            text_contains: Some("ok".to_string()),
        };
        let request = request_with_expect(Some(expect));
        let executed = executed_with_text(Some("{\"ok\":true}"));
        evaluate_main_response(&request, &executed).unwrap();
    }

    #[test]
    fn websocket_expect_rejects_missing_receive_text() {
        let expect = WebsocketExpect {
            jq: None,
            text_contains: Some("ok".to_string()),
        };
        let request = request_with_expect(Some(expect));
        let executed = executed_with_text(None);
        let err = evaluate_main_response(&request, &executed).unwrap_err();
        assert!(format!("{err:#}").contains("at least one received message"));
    }

    #[test]
    fn websocket_expect_rejects_failed_jq() {
        let err = evaluate_text_expect(
            &WebsocketExpect {
                jq: Some(".ok == false".to_string()),
                text_contains: None,
            },
            "{\"ok\":true}",
            "test",
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("jq failed"));
    }

    #[test]
    fn websocket_expect_jq_non_json_text_reports_parse_error() {
        // A non-JSON received text is a system error, not an assertion failure.
        // It must surface as a JSON-parse error, not be conflated with `jq failed`
        // (parity with the gRPC and REST evaluators).
        let err = evaluate_text_expect(
            &WebsocketExpect {
                jq: Some(".ok == true".to_string()),
                text_contains: None,
            },
            "plain-text-not-json",
            "test",
        )
        .unwrap_err();
        let message = format!("{err:#}");
        assert!(
            message.contains("jq requires a JSON response text"),
            "expected a JSON-text parse error, got: {message}"
        );
        assert!(
            !message.contains("jq failed"),
            "a non-JSON text must not be conflated with a jq assertion failure: {message}"
        );
    }

    #[test]
    fn websocket_expect_rejects_missing_text_contains() {
        let err = evaluate_text_expect(
            &WebsocketExpect {
                jq: None,
                text_contains: Some("needle".to_string()),
            },
            "haystack",
            "test",
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("textContains failed"));
    }

    #[test]
    fn websocket_expect_accepts_empty_when_no_expect() {
        let request = request_with_expect(None);
        let executed = executed_with_text(None);
        let result = evaluate_main_response(&request, &executed);
        assert_eq!(result.is_ok(), true);
    }
}
