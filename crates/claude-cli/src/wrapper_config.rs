use nils_common::env as shared_env;

pub(crate) const RUNTIME_ENV: &str = "CLAUDE_CLI_AGENT_RUNTIME";
pub(crate) const MODEL_ENV: &str = "CLAUDE_CLI_MODEL";
pub(crate) const EFFORT_ENV: &str = "CLAUDE_CLI_EFFORT";
pub(crate) const NO_PERSISTENCE_ENV: &str = "CLAUDE_CLI_NO_SESSION_PERSISTENCE";
pub(crate) const MAX_MODEL_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeMode {
    Safe,
    Inherited,
}

impl RuntimeMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::Inherited => "inherited",
        }
    }
}

pub(crate) fn resolve_runtime(explicit: Option<RuntimeMode>) -> Result<RuntimeMode, String> {
    if let Some(runtime) = explicit {
        return Ok(runtime);
    }
    match shared_env::env_non_empty(RUNTIME_ENV) {
        None => Ok(RuntimeMode::Safe),
        Some(value) => parse_runtime(&value),
    }
}

pub(crate) fn resolve_model(explicit: Option<&str>) -> Result<Option<String>, String> {
    explicit
        .map(str::to_string)
        .or_else(|| shared_env::env_non_empty(MODEL_ENV))
        .map(|value| validate_model(&value))
        .transpose()
}

pub(crate) fn resolve_effort(explicit: Option<&str>) -> Result<Option<String>, String> {
    explicit
        .map(str::to_string)
        .or_else(|| shared_env::env_non_empty(EFFORT_ENV))
        .map(|value| validate_effort(&value))
        .transpose()
}

pub(crate) fn resolve_no_persistence() -> Result<bool, String> {
    match shared_env::env_non_empty(NO_PERSISTENCE_ENV) {
        None => Ok(true),
        Some(value) => parse_bool(&value),
    }
}

pub(crate) fn validate_model(value: &str) -> Result<String, String> {
    if value.trim().is_empty()
        || value.len() > MAX_MODEL_BYTES
        || value.chars().any(char::is_control)
    {
        Err(format!(
            "model must be non-empty, at most {MAX_MODEL_BYTES} bytes, and contain no control characters"
        ))
    } else {
        Ok(value.to_string())
    }
}

pub(crate) fn validate_effort(value: &str) -> Result<String, String> {
    let lowered = value.trim().to_ascii_lowercase();
    if matches!(
        lowered.as_str(),
        "low" | "medium" | "high" | "xhigh" | "max"
    ) {
        Ok(lowered)
    } else {
        Err(format!(
            "effort must be low|medium|high|xhigh|max (got: {value})"
        ))
    }
}

pub(crate) fn parse_runtime(value: &str) -> Result<RuntimeMode, String> {
    let lowered = value.trim().to_ascii_lowercase();
    match lowered.as_str() {
        "safe" => Ok(RuntimeMode::Safe),
        "inherited" => Ok(RuntimeMode::Inherited),
        _ => Err(format!(
            "agent-runtime must be safe|inherited (got: {value})"
        )),
    }
}

pub(crate) fn parse_bool(value: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(format!(
            "no-session-persistence must be true|false (got: {value})"
        )),
    }
}
