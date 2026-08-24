use std::process::{Command, Output};

use crate::error::HookError;
use crate::model::{NormalizedRequest, OperationEffectClass};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    NotRun,
    Unavailable,
}

#[derive(Debug)]
pub(crate) struct Outcome {
    pub(crate) status: Status,
    pub(crate) message: Option<String>,
}

pub(crate) fn run(
    _request: &NormalizedRequest,
    _raw: &[u8],
    _effect: OperationEffectClass,
    _run_child: &mut dyn FnMut(Command) -> Result<Output, HookError>,
) -> Result<Outcome, HookError> {
    if !managed_selector_present() {
        return Ok(Outcome {
            status: Status::NotRun,
            message: None,
        });
    }
    if crate::liveness::coordination_failure_mode().is_some() {
        return Ok(Outcome {
            status: Status::NotRun,
            message: None,
        });
    }
    Ok(Outcome {
        status: Status::Unavailable,
        message: Some(
            "managed DSH operation lifecycle requires Linux finish-line containment".to_string(),
        ),
    })
}

fn managed_selector_present() -> bool {
    [
        "AGENT_SESSION_ID",
        "AGENT_SESSION_CAPABILITY_FILE",
        "AGENT_SESSION_STATE_DIR",
        "AGENT_SESSION_RUNTIME_ID",
        "AGENT_SESSION_BIN",
    ]
    .iter()
    .any(|name| {
        std::env::var(name)
            .ok()
            .is_some_and(|value| !value.trim().is_empty())
    })
}
