use clap::ValueEnum;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum AgentRuntimeMode {
    Isolated,
    Inherited,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentCommandProfile {
    Prompt,
    Advice,
    Knowledge,
    Commit,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AgentRuntimeOptions {
    pub runtime: Option<AgentRuntimeMode>,
    pub ephemeral: bool,
}

impl AgentRuntimeOptions {
    pub fn resolve(self) -> Result<AgentRuntimeMode, String> {
        if let Some(runtime) = self.runtime {
            return Ok(runtime);
        }
        match std::env::var("CODEX_CLI_AGENT_RUNTIME") {
            Ok(value) if value == "isolated" => Ok(AgentRuntimeMode::Isolated),
            Ok(value) if value == "inherited" => Ok(AgentRuntimeMode::Inherited),
            Ok(value) => Err(format!(
                "invalid CODEX_CLI_AGENT_RUNTIME value '{value}' (expected isolated or inherited)"
            )),
            Err(std::env::VarError::NotPresent) => Ok(AgentRuntimeMode::Isolated),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err("CODEX_CLI_AGENT_RUNTIME must be valid UTF-8".to_string())
            }
        }
    }
}
