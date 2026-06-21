use anyhow::Context;

use crate::Result;
use crate::grpc::runner::GrpcExecutedRequest;
use crate::grpc::schema::GrpcRequest;

pub fn evaluate_main_response(request: &GrpcRequest, executed: &GrpcExecutedRequest) -> Result<()> {
    let Some(expect) = request.expect.as_ref() else {
        return Ok(());
    };

    if let Some(status) = expect.status
        && executed.grpc_status != status
    {
        anyhow::bail!(
            "gRPC expect.status failed: expected {}, got {}",
            status,
            executed.grpc_status
        );
    }

    if let Some(expr) = expect.jq.as_deref() {
        let json: serde_json::Value = serde_json::from_slice(&executed.response_body)
            .context("gRPC expect.jq requires a JSON response body")?;
        if !crate::jq::eval_exit_status(&json, expr).unwrap_or(false) {
            anyhow::bail!("gRPC expect.jq failed: {expr}");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grpc::schema::{GrpcExpect, GrpcRequest};

    fn base_request(expect: Option<GrpcExpect>) -> GrpcRequest {
        GrpcRequest {
            method: "health.HealthService/Check".to_string(),
            body: serde_json::json!({}),
            metadata: Vec::new(),
            proto: None,
            import_paths: Vec::new(),
            plaintext: true,
            authority: None,
            timeout_seconds: None,
            expect,
            raw: serde_json::json!({}),
        }
    }

    fn base_executed(body: serde_json::Value) -> GrpcExecutedRequest {
        GrpcExecutedRequest {
            target: "127.0.0.1:50051".to_string(),
            method: "health.HealthService/Check".to_string(),
            grpc_status: 0,
            response_body: serde_json::to_vec(&body).unwrap(),
            stderr: String::new(),
        }
    }

    #[test]
    fn grpc_expect_accepts_matching_status_and_jq() {
        let request = base_request(Some(GrpcExpect {
            status: Some(0),
            jq: Some(".ok == true".to_string()),
        }));
        let executed = base_executed(serde_json::json!({"ok": true}));
        evaluate_main_response(&request, &executed).unwrap();
    }

    #[test]
    fn grpc_expect_rejects_mismatched_status() {
        let request = base_request(Some(GrpcExpect {
            status: Some(2),
            jq: None,
        }));
        let executed = base_executed(serde_json::json!({"ok": true}));
        let err = evaluate_main_response(&request, &executed).unwrap_err();
        assert!(format!("{err:#}").contains("expect.status failed"));
    }

    #[test]
    fn grpc_expect_rejects_failed_jq() {
        let request = base_request(Some(GrpcExpect {
            status: Some(0),
            jq: Some(".ok == false".to_string()),
        }));
        let executed = base_executed(serde_json::json!({"ok": true}));
        let err = evaluate_main_response(&request, &executed).unwrap_err();
        assert!(format!("{err:#}").contains("expect.jq failed"));
    }

    #[test]
    fn grpc_expect_accepts_when_no_expect_block() {
        // A gRPC request without an `expect` block carries no assertions and
        // must succeed (parity with `websocket_expect_accepts_empty_when_no_expect`).
        let request = base_request(None);
        let executed = base_executed(serde_json::json!({"ok": true}));
        evaluate_main_response(&request, &executed).unwrap();
    }
}
