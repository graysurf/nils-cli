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
    let before = env.status();
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
        assert_eq!(env.status(), before, "args={args:?}");
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
    env.write_project_catalog();

    let resolved = env.run(&[
        "integration",
        "resolve",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert_resolve_action(&resolved, "block", "catalog-conflict");
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
    assert!(
        json["documents"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("project/PRIVATE_POLICY.md")
    );

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
    let options = cmd::CmdOptions::default()
        .with_cwd(&target)
        .with_env("XDG_CONFIG_HOME", env.xdg_config_home.to_str().unwrap())
        .with_env("HOME", env.home.to_str().unwrap())
        .with_env("GIT_DIR", env.project.join(".git").to_str().unwrap())
        .with_env("GIT_WORK_TREE", target.to_str().unwrap())
        .with_env("GIT_COMMON_DIR", env.project.join(".git").to_str().unwrap());
    let resolved = run_cli(&args, &options);

    assert_resolve_action(&resolved, "unmanaged", "no-catalog");
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
fn session_status_checks_an_explicit_integration_fingerprint() {
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
    let contended = env.run(&["config", "exclude", "--apply", "--format", "json"]);
    assert_ne!(contended.code, 0);
    // SAFETY: `flock` observes the same valid descriptor.
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) }, 0);
    drop(lock);

    let recovered = env.run(&["config", "exclude", "--apply", "--format", "json"]);
    assert_eq!(recovered.code, 0, "stderr={}", recovered.stderr);
}
