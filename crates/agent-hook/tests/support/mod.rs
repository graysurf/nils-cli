#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
        let root = fs::canonicalize(temp.path()).expect("canonical tempdir");
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
        Self::set_private(&policy_path);
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
        Self::set_private(&config_path);
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

pub fn target_binding_digest(path: &Path) -> String {
    let mut start = path;
    while !start.is_dir() {
        start = start.parent().expect("target ancestor");
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(start)
        .args(["rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .expect("git lookup");
    let binding_root = if output.status.success() {
        PathBuf::from(
            std::str::from_utf8(&output.stdout)
                .expect("git root UTF-8")
                .trim(),
        )
    } else {
        start.to_path_buf()
    };
    let canonical = fs::canonicalize(&binding_root).expect("canonical binding root");
    let metadata = fs::metadata(&canonical).expect("binding metadata");
    let mut material = b"agent-hook.target-binding.v2\0".to_vec();
    material.extend_from_slice(path.as_os_str().as_encoded_bytes());
    material.push(0);
    material.extend_from_slice(canonical.as_os_str().as_encoded_bytes());
    material.extend_from_slice(&metadata.dev().to_le_bytes());
    material.extend_from_slice(&metadata.ino().to_le_bytes());
    sha256(&material)
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
