#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};
use sha2::{Digest, Sha256};

pub struct Fixture {
    _temp: tempfile::TempDir,
    pub root: PathBuf,
    pub home: PathBuf,
    pub config_home: PathBuf,
    pub state_home: PathBuf,
    pub session_state: PathBuf,
    pub config: PathBuf,
    pub policy: PathBuf,
}

impl Fixture {
    pub fn new(policy: &str) -> Self {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let root = temp.path().to_path_buf();
        let home = root.join("home");
        let config_home = root.join("config");
        let state_home = root.join("state");
        let session_state = root.join("session-state");
        let config_dir = config_home.join("agent-hook");
        let policy_dir = root.join("data/agent-hook/policies/current");
        fs::create_dir_all(&config_dir).expect("config dir");
        fs::create_dir_all(&policy_dir).expect("policy dir");
        fs::create_dir_all(&state_home).expect("state dir");
        fs::create_dir_all(&session_state).expect("session state dir");
        fs::create_dir_all(&home).expect("home");
        let policy_path = policy_dir.join("policy.toml");
        fs::write(&policy_path, policy).expect("policy");
        let digest = sha256(policy.as_bytes());
        let config_path = config_dir.join("config.toml");
        fs::write(
            &config_path,
            format!(
                "schema_version = \"agent-hook.config.v1\"\n\n[policy]\npath = {}\ndigest = \"{}\"\n",
                toml_string(&policy_path),
                digest,
            ),
        )
        .expect("config");
        Self {
            _temp: temp,
            root,
            home,
            config_home,
            state_home,
            session_state,
            config: config_path,
            policy: policy_path,
        }
    }

    pub fn run(&self, args: &[&str], stdin: Option<&str>) -> CmdOutput {
        self.run_with_env(args, stdin, &[])
    }

    pub fn run_with_env(
        &self,
        args: &[&str],
        stdin: Option<&str>,
        envs: &[(&str, &str)],
    ) -> CmdOutput {
        let options = CmdOptions::new()
            .with_cwd(&self.root)
            .with_env_remove("CODEX_HOME")
            .with_env("HOME", self.home.to_str().expect("home UTF-8"))
            .with_env(
                "XDG_CONFIG_HOME",
                self.config_home.to_str().expect("config UTF-8"),
            )
            .with_env(
                "XDG_STATE_HOME",
                self.state_home.to_str().expect("state UTF-8"),
            )
            .with_env(
                "AGENT_SESSION_STATE_DIR",
                self.session_state.to_str().expect("session state UTF-8"),
            )
            .with_envs(envs);
        let options = if let Some(stdin) = stdin {
            options.with_stdin_str(stdin)
        } else {
            options.with_stdin_bytes(&[])
        };
        run_resolved("agent-hook", args, &options)
    }

    pub fn set_private(path: &Path) {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("private mode");
    }
}

pub fn sha256(bytes: &[u8]) -> String {
    let hash = Sha256::digest(bytes);
    format!(
        "sha256:{}",
        hash.iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

pub fn toml_string(path: &Path) -> String {
    toml::Value::String(path.to_string_lossy().into_owned()).to_string()
}

pub fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64
}
