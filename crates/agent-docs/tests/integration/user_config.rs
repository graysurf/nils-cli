use std::fs;
use std::path::{Path, PathBuf};

use nils_test_support::{cmd, git};
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::common::{CliOutput, run_cli, write};

struct UserConfigEnv {
    _temp: TempDir,
    docs_home: PathBuf,
    project: PathBuf,
    xdg_config_home: PathBuf,
    home: PathBuf,
    private_catalog: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
struct RepositorySnapshot {
    porcelain: String,
    untracked: Vec<u8>,
    ignored: Vec<u8>,
    index: Vec<u8>,
    exclude: Vec<u8>,
}

impl UserConfigEnv {
    fn new() -> Self {
        let temp = TempDir::new().expect("create temp dir");
        let docs_home = temp.path().join("docs-home");
        let project = temp.path().join("project");
        let xdg_config_home = temp.path().join("xdg");
        let home = temp.path().join("home");
        let private_catalog = temp.path().join("private/project-policy.toml");
        for path in [&docs_home, &project, &xdg_config_home, &home] {
            fs::create_dir_all(path).expect("create fixture directory");
        }
        git::init_repo_at_with(
            &project,
            git::InitRepoOptions::new()
                .with_branch("main")
                .with_initial_commit(),
        );
        Self {
            _temp: temp,
            docs_home,
            project,
            xdg_config_home,
            home,
            private_catalog,
        }
    }

    #[cfg(unix)]
    fn new_with_aliased_root() -> Self {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().expect("create temp dir");
        let real_root = temp.path().join("real-root");
        let alias_root = temp.path().join("alias-root");
        fs::create_dir_all(&real_root).expect("create real fixture root");
        symlink(&real_root, &alias_root).expect("create fixture root alias");
        let docs_home = alias_root.join("docs-home");
        let project = alias_root.join("project");
        let xdg_config_home = alias_root.join("xdg");
        let home = alias_root.join("home");
        let private_catalog = alias_root.join("private/project-policy.toml");
        for path in [&docs_home, &project, &xdg_config_home, &home] {
            fs::create_dir_all(path).expect("create fixture directory through alias");
        }
        git::init_repo_at_with(
            &project,
            git::InitRepoOptions::new()
                .with_branch("main")
                .with_initial_commit(),
        );
        assert_ne!(alias_root, fs::canonicalize(&alias_root).unwrap());
        Self {
            _temp: temp,
            docs_home,
            project,
            xdg_config_home,
            home,
            private_catalog,
        }
    }

    fn run(&self, args: &[&str]) -> CliOutput {
        self.run_for_project(&self.project, args)
    }

    fn run_for_project(&self, project: &Path, args: &[&str]) -> CliOutput {
        let docs_home = self.docs_home.to_str().expect("utf-8 docs home");
        let project = project.to_str().expect("utf-8 project");
        let xdg = self
            .xdg_config_home
            .to_str()
            .expect("utf-8 XDG config home");
        let home = self.home.to_str().expect("utf-8 home");
        let mut full = vec!["--docs-home", docs_home, "--project-path", project];
        full.extend_from_slice(args);
        let options = cmd::CmdOptions::default()
            .with_cwd(Path::new(project))
            .with_env("XDG_CONFIG_HOME", xdg)
            .with_env("HOME", home)
            .with_env_remove("AGENT_DOCS_HOME");
        run_cli(&full, &options)
    }

    fn run_with_xdg_config_home(&self, xdg_config_home: &Path, args: &[&str]) -> CliOutput {
        let docs_home = self.docs_home.to_str().expect("utf-8 docs home");
        let project = self.project.to_str().expect("utf-8 project");
        let xdg = xdg_config_home.to_str().expect("utf-8 XDG config home");
        let home = self.home.to_str().expect("utf-8 home");
        let mut full = vec!["--docs-home", docs_home, "--project-path", project];
        full.extend_from_slice(args);
        let options = cmd::CmdOptions::default()
            .with_cwd(&self.project)
            .with_env("XDG_CONFIG_HOME", xdg)
            .with_env("HOME", home)
            .with_env_remove("AGENT_DOCS_HOME");
        run_cli(&full, &options)
    }

    fn run_with_home_fallback(&self, args: &[&str]) -> CliOutput {
        let docs_home = self.docs_home.to_str().expect("utf-8 docs home");
        let project = self.project.to_str().expect("utf-8 project");
        let home = self.home.to_str().expect("utf-8 home");
        let mut full = vec!["--docs-home", docs_home, "--project-path", project];
        full.extend_from_slice(args);
        let options = cmd::CmdOptions::default()
            .with_cwd(&self.project)
            .with_env("HOME", home)
            .with_env_remove("XDG_CONFIG_HOME")
            .with_env_remove("AGENT_DOCS_HOME");
        run_cli(&full, &options)
    }

    fn write_user_config(&self, body: &str) {
        write(&self.config_path(), body);
        set_mode(&self.config_path(), 0o600);
    }

    fn write_private_catalog(&self) {
        write(
            &self.private_catalog,
            r#"[[document]]
context = "project-dev"
scope = "project"
path = "PRIVATE_POLICY.md"
required = true
"#,
        );
        write(
            &self.project.join("PRIVATE_POLICY.md"),
            "# Private policy\n",
        );
        set_mode(&self.private_catalog, 0o600);
    }

    fn write_project_catalog(&self) {
        write(
            &self.project.join("AGENT_DOCS.toml"),
            r#"[[document]]
context = "project-dev"
scope = "project"
path = "README.md"
required = true
"#,
        );
        set_mode(&self.project.join("AGENT_DOCS.toml"), 0o644);
    }

    fn config_path(&self) -> PathBuf {
        self.xdg_config_home.join("agent-docs/config.toml")
    }

    fn status(&self) -> String {
        git::git(
            &self.project,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        )
    }

    fn repository_snapshot(&self) -> RepositorySnapshot {
        let porcelain = self.status();
        RepositorySnapshot {
            porcelain,
            untracked: fs::read(self.project.join("review-untracked.bin"))
                .expect("read untracked sentinel"),
            ignored: fs::read(self.project.join("review-ignored.bin"))
                .expect("read ignored sentinel"),
            index: fs::read(self.project.join(".git/index")).expect("read Git index"),
            exclude: fs::read(self.project.join(".git/info/exclude"))
                .expect("read Git exclude metadata"),
        }
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("set fixture mode");
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) {}

fn padded_toml(base: &str, size: usize) -> String {
    assert!(base.len() < size);
    let mut raw = base.to_string();
    raw.push('#');
    raw.push_str(&"x".repeat(size - raw.len()));
    assert_eq!(raw.len(), size);
    raw
}

fn assert_resolve_action(output: &CliOutput, action: &str, reason_code: &str) {
    assert_eq!(output.code, 0, "stderr={}", output.stderr);
    let json = output.json();
    assert_eq!(
        json["schema_version"],
        "cli.agent-docs.integration.resolve.v1"
    );
    assert!(json.get("command").is_none());
    assert_eq!(json["ok"], true);
    assert_eq!(json["data"]["action"], action);
    assert_eq!(json["data"]["reason_code"], reason_code);
    assert!(
        json["data"]["decision_fingerprint"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
}

fn assert_json_error(output: &CliOutput, code: i32, schema: &str, error_code: &str) {
    assert_eq!(output.code, code, "stderr={}", output.stderr);
    assert!(output.stderr.is_empty(), "{}", output.stderr);
    let json = output.json();
    assert_eq!(json["schema_version"], schema);
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], error_code);
}

fn assert_json_error_message_contains(
    output: &CliOutput,
    code: i32,
    schema: &str,
    error_code: &str,
    expected_message: &str,
) {
    assert_json_error(output, code, schema, error_code);
    assert!(
        output.json()["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains(expected_message)),
        "expected error message containing {expected_message:?}; stdout={}",
        output.stdout
    );
}

#[test]
fn integration_resolve_without_catalog_is_unmanaged() {
    let env = UserConfigEnv::new();
    let output = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert_resolve_action(&output, "unmanaged", "no-catalog");
    assert_eq!(output.json()["data"]["config_state"], "missing");
}

#[test]
fn decision_fingerprint_binds_product_fallback_and_selected_docs_home() {
    let env = UserConfigEnv::new();
    env.write_private_catalog();
    let enroll = env.run(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().unwrap(),
        "--apply",
        "--format",
        "json",
    ]);
    assert_eq!(enroll.code, 0, "stderr={}", enroll.stderr);

    let baseline = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    let baseline_fingerprint = baseline.json()["data"]["decision_fingerprint"]
        .as_str()
        .unwrap()
        .to_string();
    let codex = env.run(&[
        "integration",
        "resolve",
        "--product",
        "codex",
        "--format",
        "json",
    ]);
    assert_ne!(
        codex.json()["data"]["decision_fingerprint"],
        baseline_fingerprint
    );
    let local_only = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--worktree-fallback",
        "local-only",
        "--format",
        "json",
    ]);
    assert_ne!(
        local_only.json()["data"]["decision_fingerprint"],
        baseline_fingerprint
    );

    write(
        &env.docs_home.join("AGENT_DOCS.toml"),
        r#"[[document]]
context = "project-dev"
scope = "home"
path = "AGENT_HOME.md"
required = false
"#,
    );
    let changed_home = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert_ne!(
        changed_home.json()["data"]["decision_fingerprint"],
        baseline_fingerprint
    );

    let unmanaged = UserConfigEnv::new();
    let before = unmanaged.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    write(
        &unmanaged.docs_home.join("AGENT_DOCS.toml"),
        r#"[[document]]
context = "project-dev"
scope = "home"
path = "AGENT_HOME.md"
required = false
"#,
    );
    let after = unmanaged.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert_eq!(
        after.json()["data"]["decision_fingerprint"],
        before.json()["data"]["decision_fingerprint"]
    );
}

#[test]
fn config_enroll_is_dry_run_by_default() {
    let env = UserConfigEnv::new();
    env.write_private_catalog();
    let before = env.status();
    let output = env.run(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().expect("utf-8 catalog"),
        "--format",
        "json",
    ]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr);
    assert_eq!(output.json()["data"]["applied"], false);
    assert!(!env.config_path().exists());
    assert_eq!(env.status(), before);
}

#[test]
fn config_reason_accepts_500_bytes_and_rejects_501() {
    let env = UserConfigEnv::new();
    let accepted = "a".repeat(500);
    let accepted_output = env.run(&[
        "config", "exclude", "--reason", &accepted, "--format", "json",
    ]);
    assert_eq!(accepted_output.code, 0, "stderr={}", accepted_output.stderr);

    let rejected = "a".repeat(501);
    let rejected_output = env.run(&[
        "config", "exclude", "--reason", &rejected, "--format", "json",
    ]);
    assert_ne!(rejected_output.code, 0);
    assert_eq!(
        rejected_output.json()["error"]["code"],
        "config-operation-failed"
    );
    assert!(
        rejected_output.json()["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("at most 500 bytes"))
    );
}

#[test]
fn user_config_and_private_catalog_enforce_one_megabyte_limit() {
    const LIMIT: usize = 1024 * 1024;

    let env = UserConfigEnv::new();
    let catalog = padded_toml(
        r#"[[document]]
context = "project-dev"
scope = "project"
path = "PRIVATE_POLICY.md"
required = true
"#,
        LIMIT,
    );
    write(&env.private_catalog, &catalog);
    set_mode(&env.private_catalog, 0o600);
    let accepted_catalog = env.run(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert_eq!(
        accepted_catalog.code, 0,
        "stderr={}",
        accepted_catalog.stderr
    );

    write(&env.private_catalog, &format!("{catalog}x"));
    set_mode(&env.private_catalog, 0o600);
    let oversized_catalog = env.run(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert_ne!(oversized_catalog.code, 0);
    assert!(
        oversized_catalog.json()["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("1048576-byte size limit"))
    );

    let config_env = UserConfigEnv::new();
    let config = padded_toml("schema_version = 1\n", LIMIT);
    config_env.write_user_config(&config);
    let accepted_config = config_env.run(&["config", "list", "--format", "json"]);
    assert_eq!(accepted_config.code, 0, "stderr={}", accepted_config.stderr);

    fs::write(config_env.config_path(), format!("{config}x")).unwrap();
    let oversized_config = config_env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert_resolve_action(&oversized_config, "unmanaged", "no-catalog");
    assert_eq!(
        oversized_config.json()["data"]["config_state"],
        "unreadable"
    );
    assert!(
        oversized_config.json()["data"]["diagnostics"][0]["message"]
            .as_str()
            .is_some_and(|message| message.contains("1048576-byte size limit"))
    );
}

#[test]
fn all_config_mutators_leave_target_repository_unchanged() {
    let env = UserConfigEnv::new();
    env.write_private_catalog();
    fs::write(
        env.project.join("review-untracked.bin"),
        b"untracked\0sentinel\xffbytes",
    )
    .unwrap();
    fs::write(
        env.project.join("review-ignored.bin"),
        b"ignored\0sentinel\xfe bytes",
    )
    .unwrap();
    let exclude_path = env.project.join(".git/info/exclude");
    let mut exclude = fs::read(&exclude_path).unwrap();
    exclude.extend_from_slice(b"\nreview-ignored.bin\n");
    fs::write(&exclude_path, exclude).unwrap();
    let before = env.repository_snapshot();
    let catalog = env.private_catalog.to_str().expect("utf-8 catalog");

    for args in [
        &["config", "enroll", "--catalog", catalog, "--format", "json"][..],
        &[
            "config",
            "enroll",
            "--catalog",
            catalog,
            "--apply",
            "--format",
            "json",
        ][..],
        &["config", "exclude", "--format", "json"][..],
        &["config", "exclude", "--apply", "--format", "json"][..],
        &["config", "remove", "--format", "json"][..],
        &["config", "remove", "--apply", "--format", "json"][..],
    ] {
        let output = env.run(args);
        assert_eq!(output.code, 0, "stderr={}", output.stderr);
        assert_eq!(env.repository_snapshot(), before, "args={args:?}");
    }
}

#[test]
fn config_enroll_apply_selects_private_catalog_without_repo_mutation() {
    let env = UserConfigEnv::new();
    env.write_private_catalog();
    let before = env.status();
    let output = env.run(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().expect("utf-8 catalog"),
        "--apply",
        "--format",
        "json",
    ]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr);
    assert_eq!(output.json()["data"]["applied"], true);
    assert!(env.config_path().is_file());
    assert_eq!(env.status(), before);

    let resolved = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert_resolve_action(&resolved, "integrate", "user-enrollment");
    assert_eq!(
        resolved.json()["data"]["selected_catalog"]["origin"],
        "user"
    );
    assert_eq!(
        resolved.json()["data"]["matched_selector"]["kind"],
        "project-path"
    );
}

#[test]
fn config_exclude_overrides_repository_catalog() {
    let env = UserConfigEnv::new();
    env.write_project_catalog();
    let output = env.run(&["config", "exclude", "--apply", "--format", "json"]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr);

    let resolved = env.run(&[
        "integration",
        "resolve",
        "--product",
        "codex",
        "--format",
        "json",
    ]);
    assert_resolve_action(&resolved, "exclude", "user-exclusion");
    assert_eq!(
        resolved.json()["data"]["selected_catalog"],
        serde_json::Value::Null
    );
}

#[test]
fn private_and_repository_catalogs_block_instead_of_merging() {
    const REPOSITORY_CONTENT: &str = "# Repository-only policy marker\n";

    let env = UserConfigEnv::new();
    env.write_private_catalog();
    let enroll = env.run(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().expect("utf-8 catalog"),
        "--apply",
        "--format",
        "json",
    ]);
    assert_eq!(enroll.code, 0, "stderr={}", enroll.stderr);
    write(
        &env.project.join("AGENT_DOCS.toml"),
        r#"[[document]]
context = "project-dev"
scope = "project"
path = "REPOSITORY_ONLY.md"
required = true
"#,
    );
    write(&env.project.join("REPOSITORY_ONLY.md"), REPOSITORY_CONTENT);

    let resolved = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert_resolve_action(&resolved, "block", "catalog-conflict");
    assert_eq!(
        resolved.json()["data"]["selected_catalog"],
        serde_json::Value::Null
    );
    assert!(!resolved.stdout.contains("REPOSITORY_ONLY.md"));
    assert!(!resolved.stdout.contains(REPOSITORY_CONTENT.trim()));
}

#[test]
fn repository_catalog_integrates_without_user_config() {
    let env = UserConfigEnv::new();
    env.write_project_catalog();

    let resolved = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert_resolve_action(&resolved, "integrate", "repository-catalog");
    assert_eq!(
        resolved.json()["data"]["selected_catalog"]["origin"],
        "repository"
    );
}

#[test]
fn home_dot_config_is_used_when_xdg_config_home_is_unset() {
    let env = UserConfigEnv::new();
    env.write_private_catalog();
    let output = env.run_with_home_fallback(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().expect("utf-8 catalog"),
        "--apply",
        "--format",
        "json",
    ]);

    assert_eq!(output.code, 0, "stderr={}", output.stderr);
    assert!(env.home.join(".config/agent-docs/config.toml").is_file());
    assert!(!env.config_path().exists());
}

#[test]
fn relative_xdg_config_home_is_rejected() {
    let env = UserConfigEnv::new();
    let docs_home = env.docs_home.to_str().expect("utf-8 docs home");
    let project = env.project.to_str().expect("utf-8 project");
    let args = [
        "--docs-home",
        docs_home,
        "--project-path",
        project,
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ];
    let options = cmd::CmdOptions::default()
        .with_cwd(&env.project)
        .with_env("XDG_CONFIG_HOME", "relative-config")
        .with_env("HOME", env.home.to_str().expect("utf-8 home"))
        .with_env_remove("AGENT_DOCS_HOME");
    let output = run_cli(&args, &options);

    assert_ne!(output.code, 0);
    assert!(output.stderr.is_empty(), "{}", output.stderr);
    let json = output.json();
    assert_eq!(
        json["schema_version"],
        "cli.agent-docs.integration.resolve.v1"
    );
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "integration-resolve-failed");
    assert!(
        json["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("XDG_CONFIG_HOME"))
    );
}

#[test]
fn config_parse_errors_honor_json_format() {
    let env = UserConfigEnv::new();
    let output = env.run(&["config", "enroll", "--format", "json"]);

    assert_eq!(output.code, 64, "stderr={}", output.stderr);
    assert!(output.stderr.is_empty(), "{}", output.stderr);
    let json = output.json();
    assert_eq!(json["schema_version"], "cli.agent-docs.error.v1");
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "parse-error");
}

#[test]
fn user_config_semantic_usage_errors_honor_json_format() {
    let env = UserConfigEnv::new();
    let unsupported = env.run(&["audit", "--user-config", "--format", "json"]);
    assert_json_error(
        &unsupported,
        64,
        "cli.agent-docs.error.v1",
        "invalid-user-config-command",
    );

    for args in [
        &[
            "preflight",
            "--intent",
            "project-dev",
            "--user-config",
            "--format",
            "json",
        ][..],
        &["explain", "--user-config", "--format", "json"][..],
        &["list", "--user-config", "--format", "json"][..],
    ] {
        let output = env.run(args);
        assert_json_error(
            &output,
            64,
            "cli.agent-docs.error.v1",
            "user-config-requires-product",
        );
    }
}

#[test]
fn config_user_correctable_errors_exit_three() {
    let env = UserConfigEnv::new();
    let reason = "x".repeat(501);
    let output = env.run(&["config", "exclude", "--reason", &reason, "--format", "json"]);

    assert_json_error(
        &output,
        3,
        "cli.agent-docs.config.exclude.v1",
        "config-operation-failed",
    );
}

#[test]
fn session_catalog_errors_exit_three() {
    let env = UserConfigEnv::new();
    write(&env.project.join("AGENT_DOCS.toml"), "[[document]\n");
    let state_home = env.home.join("catalog-error-state");
    let output = env.run(&[
        "session",
        "activate",
        "--session-id",
        "catalog-error",
        "--product",
        "claude",
        "--state-home",
        state_home.to_str().unwrap(),
        "--intent",
        "project-dev",
        "--format",
        "json",
    ]);

    assert_json_error(
        &output,
        3,
        "cli.agent-docs.session.activate.v1",
        "catalog-load-failed",
    );
}

#[test]
fn session_runtime_errors_exit_four() {
    let env = UserConfigEnv::new();
    env.write_project_catalog();
    write(&env.project.join("README.md"), "# Project\n");
    let state_home = env.home.join("runtime-error-state");
    let state = state_home.to_str().unwrap();
    let activate = env.run(&[
        "session",
        "activate",
        "--session-id",
        "runtime-error",
        "--product",
        "claude",
        "--state-home",
        state,
        "--intent",
        "project-dev",
        "--format",
        "json",
    ]);
    assert_eq!(activate.code, 0, "stderr={}", activate.stderr);
    let record_path = state_home.join(activate.json()["data"]["record_file"].as_str().unwrap());
    fs::write(record_path, b"{not-json").unwrap();

    let status = env.run(&[
        "session",
        "status",
        "--session-id",
        "runtime-error",
        "--product",
        "claude",
        "--state-home",
        state,
        "--format",
        "json",
    ]);
    assert_json_error(
        &status,
        4,
        "cli.agent-docs.session.status.v1",
        "record-parse-failed",
    );
}

#[test]
fn bound_catalog_failures_honor_each_command_json_contract() {
    let env = UserConfigEnv::new();
    let exclude = env.run(&["config", "exclude", "--apply", "--format", "json"]);
    assert_eq!(exclude.code, 0, "stderr={}", exclude.stderr);

    for (args, schema) in [
        (
            &[
                "preflight",
                "--intent",
                "project-dev",
                "--product",
                "claude",
                "--user-config",
                "--format",
                "json",
            ][..],
            "cli.agent-docs.preflight.v2",
        ),
        (
            &[
                "explain",
                "--product",
                "claude",
                "--user-config",
                "--format",
                "json",
            ][..],
            "cli.agent-docs.explain.v1",
        ),
        (
            &[
                "list",
                "--product",
                "claude",
                "--user-config",
                "--format",
                "json",
            ][..],
            "cli.agent-docs.list.v1",
        ),
    ] {
        let output = env.run(args);
        assert_json_error(&output, 3, schema, "integration-catalog-not-selected");
    }
}

#[test]
fn invalid_user_config_never_grants_exclusion() {
    let env = UserConfigEnv::new();
    env.write_project_catalog();
    env.write_user_config(
        r#"schema_version = 1
unknown = true

[[project]]
match = "project-path"
path = "/not-used"
mode = "exclude"
"#,
    );

    let resolved = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert_resolve_action(&resolved, "integrate", "repository-catalog");
    let json = resolved.json();
    assert_eq!(json["data"]["config_state"], "invalid");
    assert_eq!(
        json["data"]["diagnostics"][0]["code"],
        "invalid-user-config"
    );
}

#[test]
fn ambiguous_matching_rules_return_typed_block() {
    let env = UserConfigEnv::new();
    let project = fs::canonicalize(&env.project).expect("canonical project");
    env.write_user_config(&format!(
        r#"schema_version = 1

[[project]]
match = "project-path"
path = "{}"
mode = "exclude"

[[project]]
match = "project-path"
path = "{}"
mode = "exclude"
"#,
        project.display(),
        project.display()
    ));

    let resolved = env.run(&[
        "integration",
        "resolve",
        "--product",
        "codex",
        "--format",
        "json",
    ]);
    assert_resolve_action(&resolved, "block", "ambiguous-user-rule");
}

#[test]
fn invalid_selected_catalog_blocks_enrollment() {
    let env = UserConfigEnv::new();
    let project = fs::canonicalize(&env.project).expect("canonical project");
    let missing = env.private_catalog.with_file_name("missing.toml");
    env.write_user_config(&format!(
        r#"schema_version = 1

[[project]]
match = "project-path"
path = "{}"
mode = "enroll"
catalog = "{}"
"#,
        project.display(),
        missing.display()
    ));

    let resolved = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert_resolve_action(&resolved, "block", "selected-catalog-unavailable");
}

#[cfg(unix)]
#[test]
fn insecure_or_moved_selected_catalog_blocks_enrollment() {
    use std::os::unix::fs::PermissionsExt;

    let env = UserConfigEnv::new();
    env.write_private_catalog();
    let enrolled = env.run(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().unwrap(),
        "--apply",
        "--format",
        "json",
    ]);
    assert_eq!(enrolled.code, 0, "stderr={}", enrolled.stderr);

    fs::set_permissions(&env.private_catalog, fs::Permissions::from_mode(0o660)).unwrap();
    let insecure = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert_resolve_action(&insecure, "block", "selected-catalog-unavailable");

    fs::set_permissions(&env.private_catalog, fs::Permissions::from_mode(0o600)).unwrap();
    fs::rename(
        &env.private_catalog,
        env.private_catalog.with_file_name("moved.toml"),
    )
    .unwrap();
    let moved = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert_resolve_action(&moved, "block", "selected-catalog-unavailable");
}

#[cfg(unix)]
#[test]
fn applied_config_has_private_directory_and_file_modes() {
    use std::os::unix::fs::PermissionsExt;

    let env = UserConfigEnv::new();
    env.write_private_catalog();
    let output = env.run(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().expect("utf-8 catalog"),
        "--apply",
        "--format",
        "json",
    ]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr);

    let config = env.config_path();
    assert_eq!(
        fs::metadata(config.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(config).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[cfg(unix)]
#[test]
fn group_writable_config_never_grants_exclusion() {
    use std::os::unix::fs::PermissionsExt;

    let env = UserConfigEnv::new();
    env.write_project_catalog();
    let project = fs::canonicalize(&env.project).expect("canonical project");
    env.write_user_config(&format!(
        r#"schema_version = 1

[[project]]
match = "project-path"
path = "{}"
mode = "exclude"
"#,
        project.display()
    ));
    fs::set_permissions(env.config_path(), fs::Permissions::from_mode(0o660))
        .expect("make config insecure");

    let resolved = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert_resolve_action(&resolved, "integrate", "repository-catalog");
    assert_eq!(resolved.json()["data"]["config_state"], "insecure");
}

#[test]
fn exact_project_rule_does_not_match_independent_clone() {
    let env = UserConfigEnv::new();
    let clone = env.project.parent().unwrap().join("independent-clone");
    let source = env.project.to_str().expect("utf-8 source");
    let target = clone.to_str().expect("utf-8 target");
    git::git(
        env.project.parent().unwrap(),
        &["clone", "-q", source, target],
    );

    let exclude = env.run(&["config", "exclude", "--apply", "--format", "json"]);
    assert_eq!(exclude.code, 0, "stderr={}", exclude.stderr);

    let resolved = env.run_for_project(
        &clone,
        &[
            "integration",
            "resolve",
            "--product",
            "claude",
            "--format",
            "json",
        ],
    );
    assert_resolve_action(&resolved, "unmanaged", "no-catalog");
}

#[test]
fn all_worktrees_rule_matches_linked_worktree_only_within_clone() {
    let env = UserConfigEnv::new();
    let linked = env.project.parent().unwrap().join("linked-worktree");
    git::worktree_add_branch(&env.project, &linked, "linked");

    let exclude = env.run(&[
        "config",
        "exclude",
        "--all-worktrees",
        "--apply",
        "--format",
        "json",
    ]);
    assert_eq!(exclude.code, 0, "stderr={}", exclude.stderr);

    let resolved = env.run_for_project(
        &linked,
        &[
            "integration",
            "resolve",
            "--product",
            "claude",
            "--format",
            "json",
        ],
    );
    assert_resolve_action(&resolved, "exclude", "user-exclusion");
    assert_eq!(
        resolved.json()["data"]["matched_selector"]["kind"],
        "git-common-dir"
    );
}

#[test]
fn private_catalog_linked_subdirectory_fallback_uses_only_equivalent_primary_path() {
    const PRIMARY_NESTED: &str = "PRIVATE_PRIMARY_NESTED_POLICY_2a";
    const PRIMARY_ROOT: &str = "PRIVATE_PRIMARY_ROOT_DECOY_2a";
    const LINKED_ROOT: &str = "PRIVATE_LINKED_ROOT_DECOY_2a";

    let env = UserConfigEnv::new();
    let linked = env.project.parent().unwrap().join("linked-worktree");
    git::worktree_add_branch(&env.project, &linked, "linked");
    let nested = linked.join("nested");
    fs::create_dir_all(&nested).expect("create linked nested project path");
    fs::create_dir_all(env.project.join("nested")).expect("create primary nested project path");
    write(
        &env.private_catalog,
        r#"[[document]]
context = "project-dev"
scope = "project"
path = "POLICY.md"
required = true
"#,
    );
    set_mode(&env.private_catalog, 0o600);
    write(&env.project.join("POLICY.md"), PRIMARY_ROOT);
    write(&env.project.join("nested/POLICY.md"), PRIMARY_NESTED);
    write(&linked.join("POLICY.md"), LINKED_ROOT);

    let enrolled = env.run(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().expect("utf-8 catalog"),
        "--all-worktrees",
        "--apply",
        "--format",
        "json",
    ]);
    assert_eq!(enrolled.code, 0, "stderr={}", enrolled.stderr);

    let out = env.run_for_project(
        &nested,
        &[
            "--user-config",
            "preflight",
            "--intent",
            "project-dev",
            "--product",
            "claude",
            "--strict",
            "--format",
            "json",
        ],
    );

    assert_eq!(out.code, 0, "stdout={} stderr={}", out.stdout, out.stderr);
    let document = &out.json()["documents"][0];
    assert_eq!(
        document["path"],
        env.project
            .join("nested/POLICY.md")
            .to_str()
            .expect("utf-8 primary nested policy")
    );
    assert_eq!(document["content"], PRIMARY_NESTED);
    assert!(!out.stdout.contains(PRIMARY_ROOT));
    assert!(!out.stdout.contains(LINKED_ROOT));
}

#[test]
fn config_show_list_and_remove_preserve_unrelated_comments() {
    let env = UserConfigEnv::new();
    env.write_private_catalog();
    let enroll = env.run(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().expect("utf-8 catalog"),
        "--reason",
        "local private policy",
        "--apply",
        "--format",
        "json",
    ]);
    assert_eq!(enroll.code, 0, "stderr={}", enroll.stderr);
    let original = fs::read_to_string(env.config_path()).expect("read config");
    fs::write(
        env.config_path(),
        format!("# keep this comment\n{original}"),
    )
    .expect("add comment");

    let show = env.run(&["config", "show", "--format", "json"]);
    assert_eq!(show.code, 0, "stderr={}", show.stderr);
    assert_eq!(show.json()["data"]["entries"].as_array().unwrap().len(), 1);
    let list = env.run(&["config", "list", "--format", "json"]);
    assert_eq!(list.code, 0, "stderr={}", list.stderr);
    assert_eq!(list.json()["data"]["entries"].as_array().unwrap().len(), 1);

    let dry_run = env.run(&["config", "remove", "--format", "json"]);
    assert_eq!(dry_run.code, 0, "stderr={}", dry_run.stderr);
    assert_eq!(dry_run.json()["data"]["applied"], false);
    assert!(
        fs::read_to_string(env.config_path())
            .unwrap()
            .contains("# keep this comment")
    );

    let remove = env.run(&["config", "remove", "--apply", "--format", "json"]);
    assert_eq!(remove.code, 0, "stderr={}", remove.stderr);
    let updated = fs::read_to_string(env.config_path()).expect("read updated config");
    assert!(updated.contains("# keep this comment"));
    assert!(!updated.contains("[[project]]"));
}

#[test]
fn explicit_user_config_uses_project_semantics_and_rejects_stale_binding() {
    let env = UserConfigEnv::new();
    env.write_private_catalog();
    let enroll = env.run(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().expect("utf-8 catalog"),
        "--apply",
        "--format",
        "json",
    ]);
    assert_eq!(enroll.code, 0, "stderr={}", enroll.stderr);
    let resolved = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert_resolve_action(&resolved, "integrate", "user-enrollment");
    let resolved_json = resolved.json();
    assert_eq!(resolved_json["data"]["selected_catalog"]["origin"], "user");
    let fingerprint = resolved_json["data"]["decision_fingerprint"]
        .as_str()
        .unwrap()
        .to_string();

    let preflight = env.run(&[
        "preflight",
        "--intent",
        "project-dev",
        "--product",
        "claude",
        "--user-config",
        "--integration-fingerprint",
        &fingerprint,
        "--format",
        "json",
    ]);
    assert_eq!(preflight.code, 0, "stderr={}", preflight.stderr);
    let json = preflight.json();
    assert_eq!(json["schema_version"], "agent-docs.preflight.v2");
    assert_eq!(json["documents"][0]["source"], "project");
    assert_eq!(json["documents"][0]["status"], "present");
    assert!(
        json["documents"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("project/PRIVATE_POLICY.md")
    );

    for mut args in [vec!["explain", "--intent", "project-dev"], vec!["list"]] {
        args.extend([
            "--product",
            "claude",
            "--user-config",
            "--integration-fingerprint",
            &fingerprint,
            "--format",
            "json",
        ]);
        let output = env.run(&args);
        assert_eq!(output.code, 0, "args={args:?} stderr={}", output.stderr);
        let output_json = output.json();
        let documents = output_json["documents"].as_array().unwrap();
        assert_eq!(documents.len(), 1, "args={args:?} json={output_json}");
        assert_eq!(documents[0]["source"], "project");
        assert!(
            documents[0]["path"]
                .as_str()
                .unwrap()
                .ends_with("project/PRIVATE_POLICY.md")
        );
    }

    write(
        &env.private_catalog,
        r#"[[document]]
context = "project-dev"
scope = "project"
path = "PRIVATE_POLICY.md"
required = false
"#,
    );
    let stale = env.run(&[
        "preflight",
        "--intent",
        "project-dev",
        "--product",
        "claude",
        "--user-config",
        "--integration-fingerprint",
        &fingerprint,
        "--format",
        "json",
    ]);
    assert_json_error(
        &stale,
        65,
        "cli.agent-docs.preflight.v2",
        "stale-integration-decision",
    );
}

#[test]
fn unrelated_registry_entry_does_not_stale_bound_session() {
    let env = UserConfigEnv::new();
    env.write_private_catalog();
    let enroll = env.run(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().expect("utf-8 catalog"),
        "--apply",
        "--format",
        "json",
    ]);
    assert_eq!(enroll.code, 0, "stderr={}", enroll.stderr);
    let resolved = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    let fingerprint = resolved.json()["data"]["decision_fingerprint"]
        .as_str()
        .unwrap()
        .to_string();
    let state_home = env.home.join("state");
    let state = state_home.to_str().expect("utf-8 state home");
    let activate = env.run(&[
        "session",
        "activate",
        "--session-id",
        "private-session",
        "--product",
        "claude",
        "--state-home",
        state,
        "--intent",
        "project-dev",
        "--user-config",
        "--integration-fingerprint",
        &fingerprint,
        "--format",
        "json",
    ]);
    assert_eq!(activate.code, 0, "stderr={}", activate.stderr);

    let unrelated = env.project.parent().unwrap().join("unrelated-project");
    fs::create_dir_all(&unrelated).expect("create unrelated project");
    let unrelated = fs::canonicalize(unrelated).expect("canonical unrelated project");
    let mut config = fs::read_to_string(env.config_path()).expect("read config");
    config.push_str(&format!(
        r#"
[[project]]
match = "project-path"
path = "{}"
mode = "exclude"
"#,
        unrelated.display()
    ));
    fs::write(env.config_path(), config).expect("append unrelated entry");
    let unchanged = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert_eq!(
        unchanged.json()["data"]["decision_fingerprint"],
        fingerprint
    );
    let status = env.run(&[
        "session",
        "status",
        "--session-id",
        "private-session",
        "--product",
        "claude",
        "--state-home",
        state,
        "--user-config",
        "--integration-fingerprint",
        &fingerprint,
        "--format",
        "json",
    ]);
    assert_eq!(status.code, 0, "stderr={}", status.stderr);
    let verify = env.run(&[
        "session",
        "verify",
        "--session-id",
        "private-session",
        "--product",
        "claude",
        "--state-home",
        state,
        "--require-intent",
        "project-dev",
        "--user-config",
        "--integration-fingerprint",
        &fingerprint,
        "--format",
        "json",
    ]);
    assert_eq!(verify.code, 0, "stderr={}", verify.stderr);

    write(
        &env.private_catalog,
        r#"[[document]]
context = "project-dev"
scope = "project"
path = "PRIVATE_POLICY.md"
required = false
"#,
    );
    let changed = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    let changed_fingerprint = changed.json()["data"]["decision_fingerprint"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(changed_fingerprint, fingerprint);
    let stale = env.run(&[
        "session",
        "verify",
        "--session-id",
        "private-session",
        "--product",
        "claude",
        "--state-home",
        state,
        "--require-intent",
        "project-dev",
        "--user-config",
        "--integration-fingerprint",
        &changed_fingerprint,
        "--format",
        "json",
    ]);
    assert_eq!(stale.code, 65, "stderr={}", stale.stderr);
    assert_eq!(stale.json()["error"]["code"], "stale-integration-decision");
}

#[test]
fn unsupported_user_config_schema_falls_back_without_granting_policy() {
    let env = UserConfigEnv::new();
    env.write_user_config("schema_version = 2\n");
    let resolved = env.run(&[
        "integration",
        "resolve",
        "--product",
        "hermes",
        "--format",
        "json",
    ]);
    assert_resolve_action(&resolved, "unmanaged", "no-catalog");
    assert_eq!(resolved.json()["data"]["config_state"], "invalid");
}

#[cfg(unix)]
#[test]
fn symlinked_user_config_never_grants_exclusion() {
    use std::os::unix::fs::symlink;

    let env = UserConfigEnv::new();
    env.write_project_catalog();
    let project = fs::canonicalize(&env.project).expect("canonical project");
    let target = env.xdg_config_home.join("config-target.toml");
    write(
        &target,
        &format!(
            r#"schema_version = 1

[[project]]
match = "project-path"
path = "{}"
mode = "exclude"
"#,
            project.display()
        ),
    );
    set_mode(&target, 0o600);
    fs::create_dir_all(env.config_path().parent().unwrap()).expect("create config dir");
    symlink(target, env.config_path()).expect("link user config");

    let resolved = env.run(&[
        "integration",
        "resolve",
        "--product",
        "codex",
        "--format",
        "json",
    ]);
    assert_resolve_action(&resolved, "integrate", "repository-catalog");
    assert_eq!(resolved.json()["data"]["config_state"], "insecure");
}

#[test]
fn enrollment_rejects_catalog_inside_target_worktree() {
    let env = UserConfigEnv::new();
    let local_catalog = env.project.join("private.toml");
    write(&local_catalog, "");
    set_mode(&local_catalog, 0o600);
    let before = env.status();
    let output = env.run(&[
        "config",
        "enroll",
        "--catalog",
        local_catalog.to_str().expect("utf-8 local catalog"),
        "--apply",
        "--format",
        "json",
    ]);
    assert_ne!(output.code, 0);
    assert!(!env.config_path().exists());
    assert_eq!(env.status(), before);
}

#[test]
fn enrollment_rejects_catalog_in_sibling_worktree_and_common_dir() {
    let env = UserConfigEnv::new();
    let linked = env
        .project
        .parent()
        .unwrap()
        .join("linked-catalog-worktree");
    git::worktree_add_branch(&env.project, &linked, "linked-catalog");
    let sibling_catalog = linked.join("private.toml");
    write(&sibling_catalog, "");
    set_mode(&sibling_catalog, 0o600);

    let common_catalog = env.project.join(".git/private.toml");
    write(&common_catalog, "");
    set_mode(&common_catalog, 0o600);

    for catalog in [&sibling_catalog, &common_catalog] {
        let output = env.run(&[
            "config",
            "enroll",
            "--catalog",
            catalog.to_str().expect("utf-8 catalog"),
            "--apply",
            "--format",
            "json",
        ]);
        assert_ne!(output.code, 0, "catalog={}", catalog.display());
        assert!(
            output.json()["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("outside every target Git worktree")),
            "catalog={} stdout={}",
            catalog.display(),
            output.stdout
        );
        assert!(!env.config_path().exists());
    }
}

#[test]
fn ambient_git_environment_cannot_spoof_common_dir_identity() {
    let env = UserConfigEnv::new();
    let exclude = env.run(&[
        "config",
        "exclude",
        "--all-worktrees",
        "--apply",
        "--format",
        "json",
    ]);
    assert_eq!(exclude.code, 0, "stderr={}", exclude.stderr);

    let target = env.project.parent().unwrap().join("spoof-target");
    fs::create_dir_all(&target).unwrap();
    git::init_repo_at_with(
        &target,
        git::InitRepoOptions::new()
            .with_branch("main")
            .with_initial_commit(),
    );
    let args = [
        "--docs-home",
        env.docs_home.to_str().unwrap(),
        "--project-path",
        target.to_str().unwrap(),
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ];
    let poisoned_config = env.home.join("poisoned-gitconfig");
    fs::write(&poisoned_config, "[core]\n\tbare = true\n").unwrap();
    let options = cmd::CmdOptions::default()
        .with_cwd(&target)
        .with_env("XDG_CONFIG_HOME", env.xdg_config_home.to_str().unwrap())
        .with_env("HOME", env.home.to_str().unwrap())
        .with_env("GIT_DIR", env.project.join(".git").to_str().unwrap())
        .with_env("GIT_WORK_TREE", target.to_str().unwrap())
        .with_env("GIT_COMMON_DIR", env.project.join(".git").to_str().unwrap())
        .with_env("GIT_CONFIG_COUNT", "1")
        .with_env("GIT_CONFIG_KEY_0", "core.bare")
        .with_env("GIT_CONFIG_VALUE_0", "true")
        .with_env("GIT_CONFIG_PARAMETERS", "'core.bare=true'")
        .with_env("GIT_CONFIG_GLOBAL", poisoned_config.to_str().unwrap())
        .with_env("GIT_CONFIG_SYSTEM", poisoned_config.to_str().unwrap())
        .with_env("GIT_CONFIG_NOSYSTEM", "0");
    let resolved = run_cli(&args, &options);

    assert_resolve_action(&resolved, "unmanaged", "no-catalog");
}

#[derive(Clone, Copy, Debug)]
enum GitPoisonChannel {
    Parameters,
    Count,
    Global,
    System,
    Directory,
    WorkTree,
    CommonDir,
}

impl GitPoisonChannel {
    const ALL: [Self; 7] = [
        Self::Parameters,
        Self::Count,
        Self::Global,
        Self::System,
        Self::Directory,
        Self::WorkTree,
        Self::CommonDir,
    ];
}

struct GitPoisonFixture {
    repository: PathBuf,
    config: PathBuf,
}

fn git_poison_fixture(env: &UserConfigEnv) -> GitPoisonFixture {
    let repository = env.project.parent().unwrap().join("git-poison");
    fs::create_dir_all(&repository).unwrap();
    git::init_repo_at_with(
        &repository,
        git::InitRepoOptions::new()
            .with_branch("main")
            .with_initial_commit(),
    );
    let config = env.home.join("git-poison-config");
    fs::write(
        &config,
        format!("[core]\nworktree = {}\n", repository.display()),
    )
    .unwrap();
    GitPoisonFixture { repository, config }
}

fn run_with_git_poison(
    env: &UserConfigEnv,
    project: &Path,
    args: &[&str],
    channel: GitPoisonChannel,
    poison: &GitPoisonFixture,
) -> CliOutput {
    let docs_home = env.docs_home.to_str().unwrap();
    let project_arg = project.to_str().unwrap();
    let xdg = env.xdg_config_home.to_str().unwrap();
    let home = env.home.to_str().unwrap();
    let mut full = vec!["--docs-home", docs_home, "--project-path", project_arg];
    full.extend_from_slice(args);
    let mut options = cmd::CmdOptions::default()
        .with_cwd(project)
        .with_env("XDG_CONFIG_HOME", xdg)
        .with_env("HOME", home)
        .with_env_remove("AGENT_DOCS_HOME");
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_NOSYSTEM",
    ] {
        options = options.with_env_remove(name);
    }
    options = match channel {
        GitPoisonChannel::Parameters => options.with_env(
            "GIT_CONFIG_PARAMETERS",
            &format!("'core.worktree={}'", poison.repository.display()),
        ),
        GitPoisonChannel::Count => options
            .with_env("GIT_CONFIG_COUNT", "1")
            .with_env("GIT_CONFIG_KEY_0", "core.worktree")
            .with_env("GIT_CONFIG_VALUE_0", poison.repository.to_str().unwrap()),
        GitPoisonChannel::Global => {
            options.with_env("GIT_CONFIG_GLOBAL", poison.config.to_str().unwrap())
        }
        GitPoisonChannel::System => options
            .with_env("GIT_CONFIG_SYSTEM", poison.config.to_str().unwrap())
            .with_env("GIT_CONFIG_NOSYSTEM", "0"),
        GitPoisonChannel::Directory => {
            options.with_env("GIT_DIR", poison.repository.join(".git").to_str().unwrap())
        }
        GitPoisonChannel::WorkTree => {
            options.with_env("GIT_WORK_TREE", poison.repository.to_str().unwrap())
        }
        GitPoisonChannel::CommonDir => options.with_env(
            "GIT_COMMON_DIR",
            poison.repository.join(".git").to_str().unwrap(),
        ),
    };
    run_cli(&full, &options)
}

#[test]
fn all_worktrees_exclusion_uses_real_identity_under_each_git_poison_channel() {
    for channel in GitPoisonChannel::ALL {
        let env = UserConfigEnv::new();
        let linked = env.project.parent().unwrap().join("linked-worktree");
        git::worktree_add_branch(&env.project, &linked, "linked");
        let poison = git_poison_fixture(&env);

        let excluded = run_with_git_poison(
            &env,
            &env.project,
            &[
                "config",
                "exclude",
                "--all-worktrees",
                "--apply",
                "--format",
                "json",
            ],
            channel,
            &poison,
        );
        assert_eq!(
            excluded.code, 0,
            "channel={channel:?} stderr={}",
            excluded.stderr
        );

        let resolved = run_with_git_poison(
            &env,
            &linked,
            &[
                "integration",
                "resolve",
                "--product",
                "claude",
                "--format",
                "json",
            ],
            channel,
            &poison,
        );
        assert_resolve_action(&resolved, "exclude", "user-exclusion");
        assert_eq!(
            resolved.json()["data"]["matched_selector"]["kind"],
            "git-common-dir",
            "channel={channel:?} stdout={}",
            resolved.stdout
        );
    }
}

#[test]
fn all_worktrees_enrollment_uses_real_identity_under_each_git_poison_channel() {
    for channel in GitPoisonChannel::ALL {
        let env = UserConfigEnv::new();
        env.write_private_catalog();
        let linked = env.project.parent().unwrap().join("linked-worktree");
        git::worktree_add_branch(&env.project, &linked, "linked");
        let poison = git_poison_fixture(&env);

        let enrolled = run_with_git_poison(
            &env,
            &env.project,
            &[
                "config",
                "enroll",
                "--catalog",
                env.private_catalog.to_str().unwrap(),
                "--all-worktrees",
                "--apply",
                "--format",
                "json",
            ],
            channel,
            &poison,
        );
        assert_eq!(
            enrolled.code, 0,
            "channel={channel:?} stderr={}",
            enrolled.stderr
        );

        let resolved = run_with_git_poison(
            &env,
            &linked,
            &[
                "integration",
                "resolve",
                "--product",
                "claude",
                "--format",
                "json",
            ],
            channel,
            &poison,
        );
        assert_resolve_action(&resolved, "integrate", "user-enrollment");
        assert_eq!(
            resolved.json()["data"]["matched_selector"]["kind"],
            "git-common-dir",
            "channel={channel:?} stdout={}",
            resolved.stdout
        );
        assert_eq!(
            resolved.json()["data"]["selected_catalog"]["origin"],
            "user"
        );
    }
}

#[test]
fn private_catalog_rejects_document_paths_outside_project() {
    for path in ["../outside.md", "/outside.md"] {
        let env = UserConfigEnv::new();
        write(
            &env.private_catalog,
            &format!(
                r#"[[document]]
context = "project-dev"
scope = "project"
path = "{path}"
required = true
"#,
            ),
        );
        set_mode(&env.private_catalog, 0o600);

        let output = env.run(&[
            "config",
            "enroll",
            "--catalog",
            env.private_catalog.to_str().unwrap(),
            "--apply",
            "--format",
            "json",
        ]);
        assert_ne!(output.code, 0, "path={path} stdout={}", output.stdout);
        assert!(!env.config_path().exists(), "path={path}");
    }
}

#[cfg(unix)]
#[test]
fn private_document_symlink_cannot_emit_content_outside_project() {
    use std::os::unix::fs::symlink;

    let env = UserConfigEnv::new();
    env.write_private_catalog();
    let enroll = env.run(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().unwrap(),
        "--apply",
        "--format",
        "json",
    ]);
    assert_eq!(enroll.code, 0, "stderr={}", enroll.stderr);
    let resolved = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    let fingerprint = resolved.json()["data"]["decision_fingerprint"]
        .as_str()
        .unwrap()
        .to_string();

    let outside = env.project.parent().unwrap().join("outside-secret.md");
    write(&outside, "DO NOT EMIT THIS\n");
    fs::remove_file(env.project.join("PRIVATE_POLICY.md")).unwrap();
    symlink(&outside, env.project.join("PRIVATE_POLICY.md")).unwrap();

    let preflight = env.run(&[
        "preflight",
        "--intent",
        "project-dev",
        "--product",
        "claude",
        "--user-config",
        "--integration-fingerprint",
        &fingerprint,
        "--format",
        "json",
    ]);
    assert_eq!(preflight.code, 0, "stderr={}", preflight.stderr);
    let document = &preflight.json()["documents"][0];
    assert_eq!(document["status"], "missing");
    assert!(document.get("content").is_none());
    assert!(!preflight.stdout.contains("DO NOT EMIT THIS"));
}

#[test]
fn stale_unrelated_selector_does_not_disable_matching_rule() {
    let env = UserConfigEnv::new();
    let project = fs::canonicalize(&env.project).unwrap();
    let missing = env.project.parent().unwrap().join("moved-away-project");
    env.write_user_config(&format!(
        r#"schema_version = 1

[[project]]
match = "project-path"
path = "{}"
mode = "exclude"

[[project]]
match = "project-path"
path = "{}"
mode = "exclude"
"#,
        missing.display(),
        project.display()
    ));

    let resolved = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert_resolve_action(&resolved, "exclude", "user-exclusion");
    assert_eq!(resolved.json()["data"]["config_state"], "valid");
}

#[test]
fn unrelated_registry_creation_does_not_change_repository_fingerprint() {
    let env = UserConfigEnv::new();
    env.write_project_catalog();
    let initial = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert_resolve_action(&initial, "integrate", "repository-catalog");
    let fingerprint = initial.json()["data"]["decision_fingerprint"]
        .as_str()
        .unwrap()
        .to_string();

    let unrelated = env.project.parent().unwrap().join("unrelated-existing");
    fs::create_dir_all(&unrelated).unwrap();
    let unrelated = fs::canonicalize(unrelated).unwrap();
    env.write_user_config(&format!(
        r#"schema_version = 1

[[project]]
match = "project-path"
path = "{}"
mode = "exclude"
"#,
        unrelated.display()
    ));
    let unchanged = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);

    assert_eq!(
        unchanged.json()["data"]["decision_fingerprint"],
        fingerprint
    );
}

#[test]
fn malformed_docs_home_blocks_private_enrollment() {
    let env = UserConfigEnv::new();
    env.write_private_catalog();
    write(&env.docs_home.join("AGENT_DOCS.toml"), "not = [valid");
    let enroll = env.run(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().unwrap(),
        "--apply",
        "--format",
        "json",
    ]);
    assert_eq!(enroll.code, 0, "stderr={}", enroll.stderr);

    let resolved = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert_resolve_action(&resolved, "block", "effective-catalog-invalid");
    assert_eq!(
        resolved.json()["data"]["diagnostics"][0]["code"],
        "docs-home-catalog-invalid"
    );
}

#[cfg(unix)]
#[test]
fn symlinked_xdg_parent_cannot_redirect_config_writes_into_project() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let env = UserConfigEnv::new();
    let redirected = env.project.join("redirected-config");
    fs::create_dir_all(&redirected).unwrap();
    let original_mode = fs::metadata(&redirected).unwrap().permissions().mode() & 0o777;
    fs::remove_dir(&env.xdg_config_home).unwrap();
    symlink(&redirected, &env.xdg_config_home).unwrap();
    let before = env.status();

    let output = env.run(&["config", "exclude", "--apply", "--format", "json"]);

    assert_ne!(output.code, 0);
    assert!(!redirected.join("agent-docs/config.toml").exists());
    assert_eq!(
        fs::metadata(&redirected).unwrap().permissions().mode() & 0o777,
        original_mode
    );
    assert_eq!(env.status(), before);
}

#[test]
fn bound_session_status_re_resolves_current_integration_fingerprint() {
    let env = UserConfigEnv::new();
    env.write_private_catalog();
    let enroll = env.run(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().unwrap(),
        "--apply",
        "--format",
        "json",
    ]);
    assert_eq!(enroll.code, 0, "stderr={}", enroll.stderr);
    let resolved = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    let fingerprint = resolved.json()["data"]["decision_fingerprint"]
        .as_str()
        .unwrap()
        .to_string();
    let state_home = env.home.join("session-status-state");
    let state = state_home.to_str().unwrap();
    let activate = env.run(&[
        "session",
        "activate",
        "--session-id",
        "status-binding",
        "--product",
        "claude",
        "--state-home",
        state,
        "--intent",
        "project-dev",
        "--user-config",
        "--integration-fingerprint",
        &fingerprint,
        "--format",
        "json",
    ]);
    assert_eq!(activate.code, 0, "stderr={}", activate.stderr);

    write(
        &env.private_catalog,
        r#"[[document]]
context = "project-dev"
scope = "project"
path = "PRIVATE_POLICY.md"
required = false
"#,
    );
    let changed = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    let changed_fingerprint = changed.json()["data"]["decision_fingerprint"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(changed_fingerprint, fingerprint);

    for extra in [
        vec!["--integration-fingerprint", fingerprint.as_str()],
        Vec::new(),
    ] {
        let mut args = vec![
            "session",
            "status",
            "--session-id",
            "status-binding",
            "--product",
            "claude",
            "--state-home",
            state,
            "--user-config",
        ];
        args.extend(extra);
        args.extend(["--format", "json"]);
        let stale = env.run(&args);
        assert_json_error(
            &stale,
            65,
            "cli.agent-docs.session.status.v1",
            "stale-integration-decision",
        );
    }

    let stale_fingerprint = "0".repeat(64);
    let stale = env.run(&[
        "session",
        "status",
        "--session-id",
        "status-binding",
        "--product",
        "claude",
        "--state-home",
        state,
        "--user-config",
        "--integration-fingerprint",
        &stale_fingerprint,
        "--format",
        "json",
    ]);
    assert_eq!(stale.code, 65, "stderr={}", stale.stderr);
    assert_eq!(stale.json()["error"]["code"], "stale-integration-decision");
}

#[test]
fn explicit_reactivation_replaces_v1_session_record() {
    let env = UserConfigEnv::new();
    env.write_private_catalog();
    let enroll = env.run(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().unwrap(),
        "--apply",
        "--format",
        "json",
    ]);
    assert_eq!(enroll.code, 0, "stderr={}", enroll.stderr);
    let resolved = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    let fingerprint = resolved.json()["data"]["decision_fingerprint"]
        .as_str()
        .unwrap()
        .to_string();
    let state_home = env.home.join("v1-session-state");
    let state = state_home.to_str().unwrap();
    let activation_args = [
        "session",
        "activate",
        "--session-id",
        "v1-reactivation",
        "--product",
        "claude",
        "--state-home",
        state,
        "--intent",
        "project-dev",
        "--user-config",
        "--integration-fingerprint",
        &fingerprint,
        "--format",
        "json",
    ];
    let activate = env.run(&activation_args);
    assert_eq!(activate.code, 0, "stderr={}", activate.stderr);
    let record_path = state_home.join(activate.json()["data"]["record_file"].as_str().unwrap());
    let mut previous: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&record_path).unwrap()).unwrap();
    previous["schema"] = "agent-docs.session.v1".into();
    previous
        .as_object_mut()
        .unwrap()
        .remove("integration_fingerprint");
    previous["active_intents"]["removed-intent"] = "superseded-fingerprint".into();
    fs::write(&record_path, serde_json::to_vec_pretty(&previous).unwrap()).unwrap();

    let status = env.run(&[
        "session",
        "status",
        "--session-id",
        "v1-reactivation",
        "--product",
        "claude",
        "--state-home",
        state,
        "--format",
        "json",
    ]);
    assert_eq!(status.code, 65, "stderr={}", status.stderr);
    assert_eq!(status.json()["error"]["code"], "unsupported-record");

    let reactivate = env.run(&activation_args);
    assert_eq!(reactivate.code, 0, "stderr={}", reactivate.stderr);
    let current: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(record_path).unwrap()).unwrap();
    assert_eq!(current["schema"], "agent-docs.session.v2");
    assert_eq!(current["integration_fingerprint"], fingerprint);
    let active_intents = current["active_intents"].as_object().unwrap();
    assert_eq!(
        active_intents
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["project-dev"]
    );

    let status = env.run(&[
        "session",
        "status",
        "--session-id",
        "v1-reactivation",
        "--product",
        "claude",
        "--state-home",
        state,
        "--format",
        "json",
    ]);
    assert_eq!(status.code, 0, "stderr={}", status.stderr);
    assert_eq!(
        status.json()["data"]["active_intents"],
        serde_json::json!(["project-dev"])
    );
}

#[test]
fn future_session_reactivation_fails_stale_without_rewriting_record() {
    let env = UserConfigEnv::new();
    env.write_project_catalog();
    write(&env.project.join("README.md"), "# Project\n");
    let state_home = env.home.join("future-session-state");
    let state = state_home.to_str().unwrap();
    let activation_args = [
        "session",
        "activate",
        "--session-id",
        "future-reactivation",
        "--product",
        "claude",
        "--state-home",
        state,
        "--intent",
        "project-dev",
        "--format",
        "json",
    ];
    let activate = env.run(&activation_args);
    assert_eq!(activate.code, 0, "stderr={}", activate.stderr);
    let record_path = state_home.join(activate.json()["data"]["record_file"].as_str().unwrap());
    let mut future: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&record_path).unwrap()).unwrap();
    future["schema"] = "agent-docs.session.v3".into();
    future["future_only"] = "future-record-marker".into();
    let future_bytes = serde_json::to_vec_pretty(&future).unwrap();
    fs::write(&record_path, &future_bytes).unwrap();

    let rejected = env.run(&activation_args);

    assert_json_error(
        &rejected,
        65,
        "cli.agent-docs.session.activate.v1",
        "unsupported-record",
    );
    assert_eq!(fs::read(record_path).unwrap(), future_bytes);
}

#[cfg(unix)]
#[test]
fn config_lock_is_descriptor_held_and_crash_recoverable() {
    use std::fs::OpenOptions;
    use std::os::fd::AsRawFd;

    let env = UserConfigEnv::new();
    let first = env.run(&["config", "exclude", "--apply", "--format", "json"]);
    assert_eq!(first.code, 0, "stderr={}", first.stderr);
    let lock_path = env.config_path().parent().unwrap().join("config.lock");
    assert!(lock_path.is_file());

    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    // SAFETY: `flock` observes the valid descriptor owned by `lock`.
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
    let before = fs::read(env.config_path()).unwrap();
    let contended = env.run(&["config", "exclude", "--apply", "--format", "json"]);
    assert_json_error_message_contains(
        &contended,
        4,
        "cli.agent-docs.config.exclude.v1",
        "config-operation-failed",
        "acquire config lock",
    );
    assert_eq!(fs::read(env.config_path()).unwrap(), before);
    // SAFETY: `flock` observes the same valid descriptor.
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) }, 0);
    drop(lock);

    let recovered = env.run(&["config", "exclude", "--apply", "--format", "json"]);
    assert_eq!(recovered.code, 0, "stderr={}", recovered.stderr);
}

#[cfg(unix)]
#[test]
fn hardlinked_config_lock_is_rejected_without_mutating_its_target() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let env = UserConfigEnv::new();
    let first = env.run(&["config", "exclude", "--apply", "--format", "json"]);
    assert_eq!(first.code, 0, "stderr={}", first.stderr);
    let config_before = fs::read(env.config_path()).unwrap();
    let lock_path = env.config_path().parent().unwrap().join("config.lock");
    let target = env
        .config_path()
        .parent()
        .unwrap()
        .join("unrelated-lock-target");
    fs::remove_file(&lock_path).unwrap();
    fs::write(&target, b"leave this file unchanged").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    fs::hard_link(&target, &lock_path).unwrap();

    let blocked = env.run(&["config", "exclude", "--apply", "--format", "json"]);

    assert_json_error_message_contains(
        &blocked,
        4,
        "cli.agent-docs.config.exclude.v1",
        "config-operation-failed",
        "must not have hardlinks",
    );
    assert_eq!(fs::read(env.config_path()).unwrap(), config_before);
    assert_eq!(fs::read(&target).unwrap(), b"leave this file unchanged");
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o644
    );
    assert_eq!(fs::metadata(&target).unwrap().nlink(), 2);
}

#[cfg(not(unix))]
fn write_config_lock_marker(env: &UserConfigEnv, pid: u32, created_unix_seconds: u64) {
    let lock_path = env.config_path().parent().unwrap().join("config.lock");
    fs::write(
        lock_path,
        serde_json::to_vec(&serde_json::json!({
            "schema": "agent-docs.config-lock.v1",
            "pid": pid,
            "created_unix_seconds": created_unix_seconds,
            "nonce": "0123456789abcdef",
        }))
        .unwrap(),
    )
    .unwrap();
}

#[cfg(not(unix))]
#[test]
fn config_lock_reclaims_demonstrably_stale_owner() {
    let env = UserConfigEnv::new();
    env.write_private_catalog();
    let first = env.run(&["config", "exclude", "--apply", "--format", "json"]);
    assert_eq!(first.code, 0, "stderr={}", first.stderr);
    write_config_lock_marker(&env, 0, 0);

    let recovered = env.run(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().unwrap(),
        "--apply",
        "--format",
        "json",
    ]);
    assert_eq!(recovered.code, 0, "stderr={}", recovered.stderr);
    assert!(
        fs::read_to_string(env.config_path())
            .unwrap()
            .contains("mode = \"enroll\"")
    );
}

#[cfg(not(unix))]
#[test]
fn config_lock_preserves_live_owner_and_config_bytes() {
    use std::time::{SystemTime, UNIX_EPOCH};

    let env = UserConfigEnv::new();
    env.write_private_catalog();
    let first = env.run(&["config", "exclude", "--apply", "--format", "json"]);
    assert_eq!(first.code, 0, "stderr={}", first.stderr);
    let before = fs::read(env.config_path()).unwrap();
    let created_unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    write_config_lock_marker(&env, std::process::id(), created_unix_seconds);

    let contended = env.run(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().unwrap(),
        "--apply",
        "--format",
        "json",
    ]);
    assert_json_error_message_contains(
        &contended,
        4,
        "cli.agent-docs.config.enroll.v1",
        "config-operation-failed",
        "lock acquisition",
    );
    assert_eq!(fs::read(env.config_path()).unwrap(), before);
}

#[cfg(not(unix))]
#[test]
fn malformed_config_lock_metadata_is_never_reclaimed() {
    let env = UserConfigEnv::new();
    env.write_private_catalog();
    let first = env.run(&["config", "exclude", "--apply", "--format", "json"]);
    assert_eq!(first.code, 0, "stderr={}", first.stderr);
    let before = fs::read(env.config_path()).unwrap();
    let lock_path = env.config_path().parent().unwrap().join("config.lock");
    fs::write(
        &lock_path,
        serde_json::to_vec(&serde_json::json!({
            "schema": "agent-docs.config-lock.v1",
            "pid": 0,
            "created_unix_seconds": 0,
            "nonce": "0123456789abcdef",
            "unexpected": true,
        }))
        .unwrap(),
    )
    .unwrap();

    let blocked = env.run(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().unwrap(),
        "--apply",
        "--format",
        "json",
    ]);

    assert_json_error_message_contains(
        &blocked,
        4,
        "cli.agent-docs.config.enroll.v1",
        "config-operation-failed",
        "config lock metadata is invalid",
    );
    assert_eq!(fs::read(env.config_path()).unwrap(), before);
    assert!(lock_path.is_file());
}

#[cfg(unix)]
#[test]
fn benign_symlink_ancestor_allows_config_and_private_catalog() {
    let env = UserConfigEnv::new_with_aliased_root();
    env.write_private_catalog();

    let enrolled = env.run(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().unwrap(),
        "--apply",
        "--format",
        "json",
    ]);
    assert_eq!(enrolled.code, 0, "stderr={}", enrolled.stderr);
    assert_eq!(
        enrolled.json()["data"]["config_path"],
        env.config_path().to_str().unwrap()
    );

    let resolved = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert_resolve_action(&resolved, "integrate", "user-enrollment");
    assert_eq!(
        resolved.json()["data"]["selected_catalog"]["origin"],
        "user"
    );
}

#[cfg(unix)]
#[test]
fn dangling_user_config_parent_symlink_fails_config_list_and_show() {
    use std::os::unix::fs::symlink;

    let env = UserConfigEnv::new();
    let parent = env.config_path().parent().unwrap().to_path_buf();
    let target = env.xdg_config_home.join("missing-config-directory");
    symlink(&target, parent).unwrap();

    for command in ["list", "show"] {
        let output = env.run(&["config", command, "--format", "json"]);
        assert_json_error_message_contains(
            &output,
            3,
            &format!("cli.agent-docs.config.{command}.v1"),
            "config-operation-failed",
            "config parent contains an unresolved symlink",
        );
    }
}

#[cfg(unix)]
#[test]
fn dangling_user_config_symlinks_fail_config_list_and_show() {
    use std::os::unix::fs::symlink;

    let env = UserConfigEnv::new();
    let target = env.xdg_config_home.join("missing-config.toml");
    fs::create_dir_all(env.config_path().parent().unwrap()).unwrap();
    symlink(&target, env.config_path()).unwrap();

    for command in ["list", "show"] {
        let output = env.run(&["config", command, "--format", "json"]);
        assert_json_error_message_contains(
            &output,
            3,
            &format!("cli.agent-docs.config.{command}.v1"),
            "config-operation-failed",
            "final path must not be a symlink",
        );
    }
}

#[cfg(unix)]
#[test]
fn final_private_catalog_symlink_is_rejected_without_registry_mutation() {
    use std::os::unix::fs::symlink;

    let env = UserConfigEnv::new();
    let target = env.private_catalog.with_file_name("real-policy.toml");
    write(
        &target,
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"PRIVATE_POLICY.md\"\n",
    );
    set_mode(&target, 0o600);
    fs::create_dir_all(env.private_catalog.parent().unwrap()).unwrap();
    symlink(&target, &env.private_catalog).unwrap();

    let output = env.run(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().unwrap(),
        "--apply",
        "--format",
        "json",
    ]);
    assert_json_error_message_contains(
        &output,
        3,
        "cli.agent-docs.config.enroll.v1",
        "config-operation-failed",
        "final path must not be a symlink",
    );
    assert!(!env.config_path().exists());
}

fn private_catalog_scope_is_rejected_before_and_after_enrollment(scope: &str) {
    let source_line = format!("scope = \"{scope}\"");
    let env = UserConfigEnv::new();
    write(
        &env.private_catalog,
        &format!(
            "[[document]]\ncontext = \"project-dev\"\n{source_line}\npath = \"PRIVATE_POLICY.md\"\nrequired = true\n"
        ),
    );
    set_mode(&env.private_catalog, 0o600);

    let output = env.run(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().unwrap(),
        "--apply",
        "--format",
        "json",
    ]);
    assert_json_error_message_contains(
        &output,
        3,
        "cli.agent-docs.config.enroll.v1",
        "config-operation-failed",
        "private/user catalog documents require project scope",
    );
    assert!(!output.stdout.contains(&source_line));
    assert!(!output.stderr.contains(&source_line));
    assert!(!env.config_path().exists(), "scope={scope}");

    env.write_private_catalog();
    let enrolled = env.run(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().unwrap(),
        "--apply",
        "--format",
        "json",
    ]);
    assert_eq!(enrolled.code, 0, "stderr={}", enrolled.stderr);
    write(
        &env.private_catalog,
        &format!(
            "[[document]]\ncontext = \"project-dev\"\n{source_line}\npath = \"PRIVATE_POLICY.md\"\nrequired = true\n"
        ),
    );
    set_mode(&env.private_catalog, 0o600);

    let drifted = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert_resolve_action(&drifted, "block", "selected-catalog-invalid");
    assert!(!drifted.stdout.contains(&source_line));
    assert!(!drifted.stderr.contains(&source_line));
}

#[test]
fn private_catalog_rejects_home_document_scope_before_and_after_enrollment() {
    private_catalog_scope_is_rejected_before_and_after_enrollment("home");
}

#[test]
fn private_catalog_rejects_global_document_scope_before_and_after_enrollment() {
    private_catalog_scope_is_rejected_before_and_after_enrollment("global");
}

#[test]
fn ordinary_catalogs_larger_than_private_limit_remain_supported() {
    const LIMIT: usize = 1024 * 1024;

    let repository = UserConfigEnv::new();
    write(&repository.project.join("README.md"), "# Project\n");
    write(
        &repository.project.join("AGENT_DOCS.toml"),
        &padded_toml(
            "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"README.md\"\nrequired = true\n",
            LIMIT + 1,
        ),
    );
    let repository_resolve = repository.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert_resolve_action(&repository_resolve, "integrate", "repository-catalog");

    let docs_home = UserConfigEnv::new();
    docs_home.write_project_catalog();
    write(&docs_home.project.join("README.md"), "# Project\n");
    write(
        &docs_home.docs_home.join("AGENT_DOCS.toml"),
        &padded_toml(
            "[[document]]\ncontext = \"project-dev\"\nscope = \"home\"\npath = \"AGENT_HOME.md\"\nrequired = false\n",
            LIMIT + 1,
        ),
    );
    let docs_home_resolve = docs_home.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert_resolve_action(&docs_home_resolve, "integrate", "repository-catalog");
}

#[test]
fn oversized_private_required_document_is_bounded_and_unsatisfied() {
    const LIMIT: usize = 1024 * 1024;
    const SECRET_SUFFIX: &str = "PRIVATE_OVERSIZE_SUFFIX_SECRET_7f2d";

    let env = UserConfigEnv::new();
    env.write_private_catalog();
    let enroll = env.run(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().unwrap(),
        "--apply",
        "--format",
        "json",
    ]);
    assert_eq!(enroll.code, 0, "stderr={}", enroll.stderr);
    let resolved = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    let fingerprint = resolved.json()["data"]["decision_fingerprint"]
        .as_str()
        .unwrap()
        .to_string();
    let mut oversized = vec![b'x'; LIMIT + 1];
    oversized.extend_from_slice(SECRET_SUFFIX.as_bytes());
    fs::write(env.project.join("PRIVATE_POLICY.md"), oversized).unwrap();

    let preflight = env.run(&[
        "preflight",
        "--intent",
        "project-dev",
        "--strict",
        "--product",
        "claude",
        "--user-config",
        "--integration-fingerprint",
        &fingerprint,
        "--format",
        "json",
    ]);
    assert_eq!(
        preflight.code, 1,
        "stdout={} stderr={}",
        preflight.stdout, preflight.stderr
    );
    assert!(!preflight.stdout.contains(SECRET_SUFFIX));
    assert!(!preflight.stderr.contains(SECRET_SUFFIX));
    assert!(preflight.json()["documents"][0].get("content").is_none());
}

#[test]
fn private_document_at_one_megabyte_remains_supported() {
    const LIMIT: usize = 1024 * 1024;

    let env = UserConfigEnv::new();
    env.write_private_catalog();
    fs::write(env.project.join("PRIVATE_POLICY.md"), vec![b'x'; LIMIT]).unwrap();
    let enroll = env.run(&[
        "config",
        "enroll",
        "--catalog",
        env.private_catalog.to_str().unwrap(),
        "--apply",
        "--format",
        "json",
    ]);
    assert_eq!(enroll.code, 0, "stderr={}", enroll.stderr);
    let resolved = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    let fingerprint = resolved.json()["data"]["decision_fingerprint"]
        .as_str()
        .unwrap()
        .to_string();
    let preflight = env.run(&[
        "preflight",
        "--intent",
        "project-dev",
        "--strict",
        "--product",
        "claude",
        "--user-config",
        "--integration-fingerprint",
        &fingerprint,
        "--format",
        "json",
    ]);
    assert_eq!(
        preflight.code, 0,
        "stdout={} stderr={}",
        preflight.stdout, preflight.stderr
    );
    assert_eq!(preflight.json()["documents"][0]["status"], "present");
    assert_eq!(
        preflight.json()["documents"][0]["content"]
            .as_str()
            .unwrap()
            .len(),
        LIMIT
    );
}

#[test]
fn config_inside_target_git_roots_is_insecure_and_cannot_grant() {
    for relative_xdg in ["private-xdg", ".git/private-xdg"] {
        let env = UserConfigEnv::new();
        env.write_project_catalog();
        write(&env.project.join("README.md"), "# Project\n");
        let xdg = env.project.join(relative_xdg);
        let config = xdg.join("agent-docs/config.toml");
        write(
            &config,
            &format!(
                "schema_version = 1\n\n[[project]]\nmatch = \"project-path\"\npath = \"{}\"\nmode = \"exclude\"\n",
                fs::canonicalize(&env.project).unwrap().display()
            ),
        );
        set_mode(&config, 0o600);

        let output = env.run_with_xdg_config_home(
            &xdg,
            &[
                "integration",
                "resolve",
                "--product",
                "claude",
                "--format",
                "json",
            ],
        );
        assert_resolve_action(&output, "integrate", "repository-catalog");
        assert_eq!(output.json()["data"]["config_state"], "insecure");
        assert_eq!(
            output.json()["data"]["matched_selector"],
            serde_json::Value::Null
        );
    }
}

#[test]
fn rendered_config_over_one_megabyte_is_rejected_without_mutation() {
    const LIMIT: usize = 1024 * 1024;

    for apply in [false, true] {
        let env = UserConfigEnv::new();
        let original = padded_toml("schema_version = 1\n", LIMIT);
        env.write_user_config(&original);
        let mut args = vec!["config", "exclude"];
        if apply {
            args.push("--apply");
        }
        args.extend(["--format", "json"]);

        let output = env.run(&args);
        assert_json_error_message_contains(
            &output,
            3,
            "cli.agent-docs.config.exclude.v1",
            "config-operation-failed",
            "1048576-byte rendered-config limit",
        );
        assert_eq!(fs::read(env.config_path()).unwrap(), original.as_bytes());
    }
}

#[test]
fn malformed_user_config_diagnostics_redact_source_lines() {
    const CONFIG_SECRET: &str = "CONFIG_SOURCE_SECRET_9c72";

    let config_env = UserConfigEnv::new();
    config_env.write_user_config(&format!("schema_version = 1\n{CONFIG_SECRET} = [\n"));
    let config_output = config_env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert_resolve_action(&config_output, "unmanaged", "no-catalog");
    assert_eq!(config_output.json()["data"]["config_state"], "invalid");
    assert!(!config_output.stdout.contains(CONFIG_SECRET));
    assert!(!config_output.stderr.contains(CONFIG_SECRET));
}

#[test]
fn malformed_private_catalog_enrollment_uses_stable_error_and_redacts_source_lines() {
    const CATALOG_SECRET: &str = "CATALOG_SOURCE_SECRET_4a13";

    let catalog_env = UserConfigEnv::new();
    write(
        &catalog_env.private_catalog,
        &format!("[[document]]\n{CATALOG_SECRET} = [\n"),
    );
    set_mode(&catalog_env.private_catalog, 0o600);
    let catalog_output = catalog_env.run(&[
        "config",
        "enroll",
        "--catalog",
        catalog_env.private_catalog.to_str().unwrap(),
        "--apply",
        "--format",
        "json",
    ]);
    assert_json_error_message_contains(
        &catalog_output,
        3,
        "cli.agent-docs.config.enroll.v1",
        "config-operation-failed",
        "invalid private catalog TOML",
    );
    assert!(!catalog_output.stdout.contains(CATALOG_SECRET));
    assert!(!catalog_output.stderr.contains(CATALOG_SECRET));
    assert!(!catalog_env.config_path().exists());
}

#[test]
fn malformed_selected_private_catalog_diagnostics_redact_source_lines() {
    const CATALOG_SECRET: &str = "CATALOG_SOURCE_SECRET_4a13";

    let selected_env = UserConfigEnv::new();
    selected_env.write_private_catalog();
    let enroll = selected_env.run(&[
        "config",
        "enroll",
        "--catalog",
        selected_env.private_catalog.to_str().unwrap(),
        "--apply",
        "--format",
        "json",
    ]);
    assert_eq!(enroll.code, 0, "stderr={}", enroll.stderr);
    write(
        &selected_env.private_catalog,
        &format!("[[document]]\n{CATALOG_SECRET} = [\n"),
    );
    set_mode(&selected_env.private_catalog, 0o600);
    let selected_output = selected_env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert_resolve_action(&selected_output, "block", "selected-catalog-invalid");
    assert!(!selected_output.stdout.contains(CATALOG_SECRET));
    assert!(!selected_output.stderr.contains(CATALOG_SECRET));
}

#[test]
fn non_regular_private_catalog_and_document_fail_deterministically() {
    let catalog_env = UserConfigEnv::new();
    fs::create_dir_all(&catalog_env.private_catalog).unwrap();
    let catalog_output = catalog_env.run(&[
        "config",
        "enroll",
        "--catalog",
        catalog_env.private_catalog.to_str().unwrap(),
        "--apply",
        "--format",
        "json",
    ]);
    assert_json_error(
        &catalog_output,
        3,
        "cli.agent-docs.config.enroll.v1",
        "config-operation-failed",
    );
    assert!(!catalog_env.config_path().exists());

    let document_env = UserConfigEnv::new();
    document_env.write_private_catalog();
    let enroll = document_env.run(&[
        "config",
        "enroll",
        "--catalog",
        document_env.private_catalog.to_str().unwrap(),
        "--apply",
        "--format",
        "json",
    ]);
    assert_eq!(enroll.code, 0, "stderr={}", enroll.stderr);
    let resolved = document_env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    let fingerprint = resolved.json()["data"]["decision_fingerprint"]
        .as_str()
        .unwrap()
        .to_string();
    fs::remove_file(document_env.project.join("PRIVATE_POLICY.md")).unwrap();
    fs::create_dir(document_env.project.join("PRIVATE_POLICY.md")).unwrap();
    let preflight = document_env.run(&[
        "preflight",
        "--intent",
        "project-dev",
        "--strict",
        "--product",
        "claude",
        "--user-config",
        "--integration-fingerprint",
        &fingerprint,
        "--format",
        "json",
    ]);
    assert_eq!(
        preflight.code, 1,
        "stdout={} stderr={}",
        preflight.stdout, preflight.stderr
    );
    assert_eq!(preflight.json()["documents"][0]["status"], "missing");
    assert!(preflight.json()["documents"][0].get("content").is_none());
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn non_utf_selectors_fail_before_config_mutation() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    for all_worktrees in [false, true] {
        let temp = TempDir::new().unwrap();
        let project = temp
            .path()
            .join(OsString::from_vec(b"non-utf-project-\xff".to_vec()));
        let docs_home = temp.path().join("docs-home");
        let home = temp.path().join("home");
        let xdg = temp.path().join("xdg");
        for path in [&project, &docs_home, &home, &xdg] {
            fs::create_dir_all(path).unwrap();
        }
        git::init_repo_at_with(
            &project,
            git::InitRepoOptions::new()
                .with_branch("main")
                .with_initial_commit(),
        );
        let mut args = vec![
            OsString::from("--docs-home"),
            docs_home.as_os_str().to_os_string(),
            OsString::from("--project-path"),
            project.as_os_str().to_os_string(),
            OsString::from("config"),
            OsString::from("exclude"),
        ];
        if all_worktrees {
            args.push(OsString::from("--all-worktrees"));
        }
        args.extend([
            OsString::from("--apply"),
            OsString::from("--format"),
            OsString::from("json"),
        ]);
        let arg_refs = args.iter().map(OsString::as_os_str).collect::<Vec<_>>();
        let options = cmd::CmdOptions::default()
            .with_cwd(&project)
            .with_env("HOME", home.to_str().unwrap())
            .with_env("XDG_CONFIG_HOME", xdg.to_str().unwrap())
            .with_env_remove("AGENT_DOCS_HOME");

        let output = super::common::run_cli_os(&arg_refs, &options);
        assert_json_error(
            &output,
            3,
            "cli.agent-docs.config.exclude.v1",
            "config-operation-failed",
        );
        let config_dir = xdg.join("agent-docs");
        assert!(!config_dir.join("config.toml").exists());
        assert!(!config_dir.join("config.lock").exists());
    }
}

#[cfg(unix)]
#[test]
fn non_utf_config_destination_fails_before_filesystem_mutation() {
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStringExt;

    let temp = TempDir::new().unwrap();
    let docs_home = temp.path().join("docs-home");
    let project = temp.path().join("project");
    let home = temp.path().join("home");
    let xdg = temp
        .path()
        .join(OsString::from_vec(b"non-utf-xdg-\xff".to_vec()));
    for path in [&docs_home, &project, &home] {
        fs::create_dir_all(path).unwrap();
    }
    git::init_repo_at_with(
        &project,
        git::InitRepoOptions::new()
            .with_branch("main")
            .with_initial_commit(),
    );
    let output = run_cli(
        &[
            "--docs-home",
            docs_home.to_str().unwrap(),
            "--project-path",
            project.to_str().unwrap(),
            "config",
            "exclude",
            "--apply",
            "--format",
            "json",
        ],
        &cmd::CmdOptions::default()
            .with_cwd(&project)
            .with_env("HOME", home.to_str().unwrap())
            .with_env_os(OsStr::new("XDG_CONFIG_HOME"), xdg.as_os_str())
            .with_env_remove("AGENT_DOCS_HOME"),
    );

    assert_json_error(
        &output,
        3,
        "cli.agent-docs.config.exclude.v1",
        "config-operation-failed",
    );
    assert!(!xdg.join("agent-docs").exists());
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn non_utf_private_catalog_identity_fails_before_config_mutation() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let env = UserConfigEnv::new();
    let catalog = env
        .private_catalog
        .parent()
        .unwrap()
        .join(OsString::from_vec(b"policy-\xfe.toml".to_vec()));
    write(
        &catalog,
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"PRIVATE_POLICY.md\"\nrequired = true\n",
    );
    write(&env.project.join("PRIVATE_POLICY.md"), "# Private\n");
    set_mode(&catalog, 0o600);
    let args = [
        OsString::from("--docs-home"),
        env.docs_home.as_os_str().to_os_string(),
        OsString::from("--project-path"),
        env.project.as_os_str().to_os_string(),
        OsString::from("config"),
        OsString::from("enroll"),
        OsString::from("--catalog"),
        catalog.as_os_str().to_os_string(),
        OsString::from("--apply"),
        OsString::from("--format"),
        OsString::from("json"),
    ];
    let arg_refs = args.iter().map(OsString::as_os_str).collect::<Vec<_>>();
    let options = cmd::CmdOptions::default()
        .with_cwd(&env.project)
        .with_env("HOME", env.home.to_str().unwrap())
        .with_env("XDG_CONFIG_HOME", env.xdg_config_home.to_str().unwrap())
        .with_env_remove("AGENT_DOCS_HOME");

    let output = super::common::run_cli_os(&arg_refs, &options);
    assert_json_error(
        &output,
        3,
        "cli.agent-docs.config.enroll.v1",
        "config-operation-failed",
    );
    assert!(!env.config_path().exists());
    assert!(
        !env.config_path()
            .parent()
            .unwrap()
            .join("config.lock")
            .exists()
    );
}
