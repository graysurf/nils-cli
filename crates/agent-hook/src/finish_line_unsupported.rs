use std::path::Path;

use serde_json::{Value, json};

use crate::error::HookError;

pub(crate) const CONTAINED_RUNNER_ARG: &str = "__finish-line-contained-runner";

pub enum Operation {
    Open,
    Begin,
    Run,
    Register,
    Admit,
    Observe,
    Verdict,
    Stop,
    Status,
    Quiesce,
    Release,
}

pub struct Outcome {
    pub data: Value,
    pub text: String,
    pub exit_code: i32,
}

pub fn run(_state_root: &Path, _operation: Operation) -> Result<Outcome, HookError> {
    Err(unsupported_host())
}

pub(crate) fn exec_contained_runner() -> ! {
    eprintln!("agent-hook: finish-line contained runner is unsupported on this platform");
    std::process::exit(203)
}

fn unsupported_host() -> HookError {
    HookError::unavailable_with(
        "finish-line-containment-unavailable",
        "authoritative finish-line execution requires Linux systemd cgroup containment",
        json!({
            "retryable": true,
            "next_action": "verify the local resource and retry the exact request once",
            "recovery": {
                "kind": "bounded-retry",
                "max_attempts": 1,
            },
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_host_is_a_typed_fail_closed_boundary() {
        let error = unsupported_host();
        assert_eq!(error.code, "finish-line-containment-unavailable");
        assert_eq!(error.exit_code, 69);
        assert_eq!(
            error
                .details
                .as_deref()
                .and_then(|value| value["retryable"].as_bool()),
            Some(true)
        );
    }
}
