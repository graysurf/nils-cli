use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Deserialize;
use serde_json::json;

use super::AgentCommandProfile;
use super::child_home::{self, ChildHome};

const REQUIRED_FLAGS: [&str; 5] = [
    "--ignore-user-config",
    "--ignore-rules",
    "--ephemeral",
    "--skip-git-repo-check",
    "--disable",
];
const REQUIRED_FEATURES: [&str; 8] = [
    "hooks",
    "plugins",
    "remote_plugin",
    "apps",
    "memories",
    "goals",
    "multi_agent",
    "workspace_dependencies",
];
const COMMIT_DISABLED_FEATURES: [&str; 2] = ["shell_tool", "unified_exec"];
const HOME_INSTRUCTION_SENTINEL: &str = "CODEX_CLI_AGENT_DOCTOR_HOME_INSTRUCTION_SENTINEL_7D70B264";
const PROJECT_INSTRUCTION_SENTINEL: &str =
    "CODEX_CLI_AGENT_DOCTOR_PROJECT_INSTRUCTION_SENTINEL_B28F1E67";
/// Entry names inside a Codex home that can define hooks or carry hook trust
/// state. The isolated runtime writes none of them, so their absence is a
/// checkable property of the private child home. Hook *execution* is not
/// observable from `agent doctor`: no non-model `codex` subcommand runs
/// config-defined lifecycle hooks, and doctor must not spend a model turn. The
/// execution-side guarantee is `--disable hooks`, reported separately as
/// `features.hooks` in the same payload.
const HOOK_SURFACE_ENTRIES: [&str; 3] = ["config.toml", "hooks.json", "hooks"];
/// Fixed descriptor of what `hook_isolation` actually asserts, so a JSON
/// consumer cannot read the boolean as proof that a hook was observed to not
/// run.
const HOOK_ISOLATION_METHOD: &str = "child-home-hook-surface";

struct IsolatedHome {
    home: ChildHome,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GeneratedCommitMessage {
    #[serde(rename = "type")]
    pub commit_type: String,
    pub scope: Option<String>,
    pub subject: String,
    #[serde(default)]
    pub body_bullets: Vec<String>,
}

impl IsolatedHome {
    fn create() -> Result<Self, String> {
        super::refresh_remote_auth_before_exec();
        Self::create_without_refresh()
    }

    fn create_without_refresh() -> Result<Self, String> {
        let original_home = child_home::original_codex_home();
        let auth_source = child_home::auth_source(&original_home);
        let home = ChildHome::create("codex-cli-agent-")
            .map_err(|error| format!("isolated-home-create-failed: {error}"))?;
        home.bridge_auth(auth_source.as_deref())
            .map_err(|error| format!("isolated-auth-bridge-unavailable: {error}"))?;
        Ok(Self { home })
    }

    fn path(&self) -> &Path {
        self.home.path()
    }
}

#[derive(Debug)]
struct CapabilityReport {
    flags: BTreeMap<&'static str, bool>,
    features: BTreeMap<&'static str, bool>,
}

impl CapabilityReport {
    fn ready(&self) -> bool {
        self.flags.values().all(|available| *available)
            && self.features.values().all(|available| *available)
    }

    fn missing(&self) -> String {
        let flags = self
            .flags
            .iter()
            .filter_map(|(name, available)| (!available).then_some(*name));
        let features = self
            .features
            .iter()
            .filter_map(|(name, available)| (!available).then_some(*name));
        flags.chain(features).collect::<Vec<_>>().join(", ")
    }
}

pub fn exec_isolated(prompt: &str, profile: AgentCommandProfile, stderr: &mut impl Write) -> i32 {
    if prompt.trim().is_empty() {
        let _ = writeln!(stderr, "codex-cli agent: missing prompt");
        return 1;
    }
    if let Err(message) = probe_capabilities(profile) {
        let _ = writeln!(stderr, "isolated-runtime-unsupported: {message}");
        return 1;
    }
    let home = match IsolatedHome::create() {
        Ok(home) => home,
        Err(message) => {
            let _ = writeln!(stderr, "{message}");
            return 1;
        }
    };
    let mut command = isolated_command(&home, profile);
    command
        .args(["--", prompt])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let result = match command.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(error) => {
            let _ = writeln!(
                stderr,
                "codex-cli agent: failed to run isolated codex: {error}"
            );
            1
        }
    };
    warn_if_auth_replaced(&home, stderr);
    result
}

pub fn generate_commit_message(
    prompt: &str,
    stderr: &mut impl Write,
) -> Result<GeneratedCommitMessage, String> {
    probe_capabilities(AgentCommandProfile::Commit)
        .map_err(|message| format!("isolated-runtime-unsupported: {message}"))?;
    let home = IsolatedHome::create()?;
    let schema_path = home.path().join("commit-message.schema.json");
    let output_path = home.path().join("commit-message.json");
    let schema = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["type", "scope", "subject", "body_bullets"],
        "properties": {
            "type": {"type": "string"},
            "scope": {"type": ["string", "null"]},
            "subject": {"type": "string"},
            "body_bullets": {"type": "array", "items": {"type": "string"}}
        }
    });
    fs::write(
        &schema_path,
        serde_json::to_vec(&schema).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("isolated-home-create-failed: {error}"))?;

    let mut command = isolated_command(&home, AgentCommandProfile::Commit);
    command
        .args(["--output-schema"])
        .arg(&schema_path)
        .args(["--output-last-message"])
        .arg(&output_path)
        .args(["--", prompt])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    let status = command.status().map_err(|error| {
        format!("codex-cli agent commit: failed to run isolated codex: {error}")
    })?;
    warn_if_auth_replaced(&home, stderr);
    if !status.success() {
        return Err(format!(
            "codex-cli agent commit: model message generation failed (exit code: {})",
            status.code().unwrap_or(1)
        ));
    }
    let metadata = fs::metadata(&output_path)
        .map_err(|error| format!("codex-cli agent commit: missing model output: {error}"))?;
    if metadata.len() > 64 * 1024 {
        return Err("codex-cli agent commit: model output exceeds 64 KiB".to_string());
    }
    let bytes = fs::read(&output_path)
        .map_err(|error| format!("codex-cli agent commit: unreadable model output: {error}"))?;
    let message: GeneratedCommitMessage = serde_json::from_slice(&bytes)
        .map_err(|error| format!("codex-cli agent commit: invalid model output: {error}"))?;
    validate_generated_message(&message)?;
    Ok(message)
}

pub fn doctor_isolated(json_output: bool) -> i32 {
    let capabilities = capability_report(AgentCommandProfile::Commit);
    let (isolated_home, auth_bridge, instruction_isolation, hook_isolation) =
        match IsolatedHome::create_without_refresh() {
            Ok(home) => {
                let (instructions, hooks) = doctor_sentinel_probe(&home);
                (true, true, instructions, hooks)
            }
            Err(_) => (false, false, false, false),
        };
    let ready = capabilities.ready()
        && isolated_home
        && auth_bridge
        && instruction_isolation
        && hook_isolation;
    if json_output {
        let result = json!({
            "schema_version": "cli.codex-cli.agent.doctor.v1",
            "ok": true,
            "data": {
                "ready": ready,
                "flags": capabilities.flags,
                "features": capabilities.features,
                "isolated_home": isolated_home,
                "auth_bridge": auth_bridge,
                "instruction_isolation": instruction_isolation,
                "hook_isolation": hook_isolation,
                "hook_isolation_method": HOOK_ISOLATION_METHOD
            }
        });
        println!("{}", serde_json::to_string(&result).expect("doctor JSON"));
    } else if ready {
        println!("codex-cli agent isolated runtime: ready");
    } else {
        println!("codex-cli agent isolated runtime: unavailable");
    }
    if ready { 0 } else { 1 }
}

fn probe_capabilities(profile: AgentCommandProfile) -> Result<(), String> {
    let report = capability_report(profile);
    if report.ready() {
        Ok(())
    } else {
        Err(format!("missing capabilities: {}", report.missing()))
    }
}

fn capability_report(profile: AgentCommandProfile) -> CapabilityReport {
    let help_words = child_home::codex_exec_help_words();
    let flags = REQUIRED_FLAGS
        .iter()
        .map(|flag| (*flag, help_words.contains(*flag)))
        .collect::<BTreeMap<_, _>>();

    let available_features = child_home::codex_feature_names();
    let mut required = REQUIRED_FEATURES.to_vec();
    if profile == AgentCommandProfile::Commit {
        required.extend(COMMIT_DISABLED_FEATURES);
    }
    let features = required
        .into_iter()
        .map(|feature| (feature, available_features.contains(feature)))
        .collect::<BTreeMap<_, _>>();
    CapabilityReport { flags, features }
}

fn doctor_sentinel_probe(home: &IsolatedHome) -> (bool, bool) {
    let Ok(probe_root) = tempfile::Builder::new()
        .prefix("codex-cli-agent-doctor-")
        .tempdir()
    else {
        return (false, false);
    };
    let ambient_home = probe_root.path().join("ambient-home");
    let ambient_codex_home = ambient_home.join(".codex");
    let project = probe_root.path().join("project");
    if fs::create_dir_all(&ambient_codex_home).is_err()
        || fs::create_dir_all(&project).is_err()
        || fs::write(
            ambient_codex_home.join("AGENTS.md"),
            HOME_INSTRUCTION_SENTINEL,
        )
        .is_err()
        || fs::write(project.join("AGENTS.md"), PROJECT_INSTRUCTION_SENTINEL).is_err()
    {
        return (false, false);
    }

    // Check the hook surface before the probe turn so a home builder that ever
    // started projecting ambient hook configuration is caught even if the turn
    // itself fails.
    if !hook_surface_absent(home.path()) {
        return (false, false);
    }

    let mut command = Command::new("codex");
    command.args(["-c", "project_doc_max_bytes=0"]);
    for feature in REQUIRED_FEATURES {
        command.args(["--disable", feature]);
    }
    for feature in COMMIT_DISABLED_FEATURES {
        command.args(["--disable", feature]);
    }
    command
        .args([
            "debug",
            "prompt-input",
            "codex-cli agent doctor isolation probe",
        ])
        .current_dir(&project)
        .env("HOME", &ambient_home)
        .env("CODEX_HOME", home.path())
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    child_home::remove_control_environment(&mut command);
    let Ok(output) = command.output() else {
        return (false, false);
    };
    if !output.status.success() {
        return (false, false);
    }
    let instruction_isolation = !output
        .stdout
        .windows(HOME_INSTRUCTION_SENTINEL.len())
        .any(|window| window == HOME_INSTRUCTION_SENTINEL.as_bytes())
        && !output
            .stdout
            .windows(PROJECT_INSTRUCTION_SENTINEL.len())
            .any(|window| window == PROJECT_INSTRUCTION_SENTINEL.as_bytes());
    // Re-check after the turn so anything the child materialized in its private
    // home is caught too, not only what the home builder wrote.
    let hook_isolation = hook_surface_absent(home.path());
    (instruction_isolation, hook_isolation)
}

/// Report whether the child home carries no hook definition or hook trust
/// surface. Uses `symlink_metadata` so a dangling symlink planted at one of
/// those names counts as present instead of silently passing.
fn hook_surface_absent(home: &Path) -> bool {
    HOOK_SURFACE_ENTRIES
        .iter()
        .all(|entry| fs::symlink_metadata(home.join(entry)).is_err())
}

fn isolated_command(home: &IsolatedHome, profile: AgentCommandProfile) -> Command {
    let mut command = Command::new("codex");
    command
        .args(["--ask-for-approval", "never", "exec"])
        .args(REQUIRED_FLAGS[..4].iter().copied());
    for feature in REQUIRED_FEATURES {
        command.args(["--disable", feature]);
    }
    if profile == AgentCommandProfile::Commit {
        for feature in COMMIT_DISABLED_FEATURES {
            command.args(["--disable", feature]);
        }
    }
    let model = std::env::var("CODEX_CLI_MODEL").unwrap_or_else(|_| {
        crate::provider_profile::CODEX_PROVIDER_PROFILE
            .defaults
            .model
            .to_string()
    });
    let reasoning = std::env::var("CODEX_CLI_REASONING").unwrap_or_else(|_| {
        crate::provider_profile::CODEX_PROVIDER_PROFILE
            .defaults
            .reasoning
            .to_string()
    });
    let sandbox = match profile {
        AgentCommandProfile::Prompt => "workspace-write",
        AgentCommandProfile::Advice
        | AgentCommandProfile::Knowledge
        | AgentCommandProfile::Commit => "read-only",
    };
    command
        .args(["-c", "project_doc_max_bytes=0"])
        .args(["--model", model.as_str()])
        .args(["-c", &format!("model_reasoning_effort=\"{reasoning}\"")])
        .args(["--sandbox", sandbox])
        .env("CODEX_HOME", home.path());
    child_home::remove_control_environment(&mut command);
    command
}

fn validate_generated_message(message: &GeneratedCommitMessage) -> Result<(), String> {
    const TYPES: [&str; 11] = [
        "build", "chore", "ci", "docs", "feat", "fix", "perf", "refactor", "revert", "style",
        "test",
    ];
    if !TYPES.contains(&message.commit_type.as_str()) {
        return Err("codex-cli agent commit: invalid commit type".to_string());
    }
    if let Some(scope) = &message.scope
        && (scope.trim().is_empty()
            || scope.len() > 64
            || scope
                .chars()
                .any(|ch| ch.is_whitespace() || ch == '(' || ch == ')'))
    {
        return Err("codex-cli agent commit: invalid commit scope".to_string());
    }
    let subject = message.subject.trim();
    if subject.is_empty()
        || subject != message.subject
        || subject.len() > 100
        || subject.contains(['\r', '\n'])
    {
        return Err("codex-cli agent commit: invalid commit subject".to_string());
    }
    let header_len = message.commit_type.len()
        + message.scope.as_ref().map_or(0, |scope| scope.len() + 2)
        + 2
        + subject.len();
    if header_len > 100 {
        return Err("codex-cli agent commit: commit header exceeds 100 characters".to_string());
    }
    if message.body_bullets.len() > 20
        || message.body_bullets.iter().any(|bullet| {
            bullet.trim().is_empty()
                || bullet != bullet.trim()
                || bullet.len() > 500
                || bullet.contains(['\r', '\n'])
        })
    {
        return Err("codex-cli agent commit: invalid body bullet".to_string());
    }
    Ok(())
}

fn warn_if_auth_replaced(home: &IsolatedHome, stderr: &mut impl Write) {
    child_home::warn_if_auth_replaced(home.path(), stderr);
}
