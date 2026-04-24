use codex_cli::prompts::{self, PromptTemplateError};
use nils_test_support::{EnvGuard, GlobalStateLock};
use pretty_assertions::assert_eq;
use std::fs;
use std::path::Path;

fn set_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path).expect("meta").permissions();
        perms.set_mode(mode);
        fs::set_permissions(path, perms).expect("chmod");
    }
}

#[test]
fn prompts_resolve_uses_feature_dir_fallback_when_zdotdir_prompts_missing() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let zdotdir = dir.path().join("zdotdir");
    fs::create_dir_all(&zdotdir).expect("zdotdir");

    let script_dir = dir.path().join("scripts");
    let feature_prompts = script_dir.join("_features").join("codex").join("prompts");
    fs::create_dir_all(&feature_prompts).expect("feature prompts");
    fs::write(
        feature_prompts.join("actionable-advice.md"),
        "Hello $ARGUMENTS\n",
    )
    .expect("write template");

    let zdotdir_str = zdotdir.to_string_lossy().to_string();
    let script_dir_str = script_dir.to_string_lossy().to_string();
    let _zdotdir_env = EnvGuard::set(&lock, "ZDOTDIR", &zdotdir_str);
    let _script_env = EnvGuard::set(&lock, "ZSH_SCRIPT_DIR", &script_dir_str);

    let resolved = prompts::resolve_prompts_dir().expect("resolve prompts dir");
    assert_eq!(resolved, feature_prompts);

    let (path, content) = prompts::read_template("actionable-advice").expect("read template");
    assert_eq!(path, feature_prompts.join("actionable-advice.md"));
    assert!(content.contains("Hello $ARGUMENTS"));
}

#[test]
fn prompts_resolve_returns_none_when_no_prompts_dirs_exist() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let zdotdir = dir.path().join("zdotdir");
    fs::create_dir_all(&zdotdir).expect("zdotdir");

    let script_dir = dir.path().join("scripts");
    let feature_dir = script_dir.join("_features").join("codex");
    fs::create_dir_all(&feature_dir).expect("feature dir");

    let zdotdir_str = zdotdir.to_string_lossy().to_string();
    let script_dir_str = script_dir.to_string_lossy().to_string();
    let _zdotdir_env = EnvGuard::set(&lock, "ZDOTDIR", &zdotdir_str);
    let _script_env = EnvGuard::set(&lock, "ZSH_SCRIPT_DIR", &script_dir_str);

    assert!(prompts::resolve_prompts_dir().is_none());
}

#[test]
fn prompts_read_template_errors_when_prompts_dir_missing() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let zdotdir = dir.path().join("zdotdir");
    fs::create_dir_all(&zdotdir).expect("zdotdir");
    let script_dir = dir.path().join("scripts");
    fs::create_dir_all(&script_dir).expect("scripts dir");

    let zdotdir_str = zdotdir.to_string_lossy().to_string();
    let script_dir_str = script_dir.to_string_lossy().to_string();
    let _zdotdir_env = EnvGuard::set(&lock, "ZDOTDIR", &zdotdir_str);
    let _script_env = EnvGuard::set(&lock, "ZSH_SCRIPT_DIR", &script_dir_str);

    let err = prompts::read_template("anything").unwrap_err();
    assert!(matches!(err, PromptTemplateError::PromptsDirNotFound));
}

#[test]
fn prompts_read_template_errors_when_template_missing() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let zdotdir = dir.path().join("zdotdir");
    let prompts_dir = zdotdir.join("prompts");
    fs::create_dir_all(&prompts_dir).expect("prompts dir");

    let zdotdir_str = zdotdir.to_string_lossy().to_string();
    let _zdotdir_env = EnvGuard::set(&lock, "ZDOTDIR", &zdotdir_str);

    let err = prompts::read_template("missing-template").unwrap_err();
    match err {
        PromptTemplateError::TemplateMissing { path } => {
            assert!(path.ends_with("missing-template.md"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn prompts_read_template_errors_when_template_unreadable() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let zdotdir = dir.path().join("zdotdir");
    let prompts_dir = zdotdir.join("prompts");
    fs::create_dir_all(&prompts_dir).expect("prompts dir");
    let template = prompts_dir.join("unreadable.md");
    fs::write(&template, "hi").expect("write template");
    set_mode(&template, 0o000);

    let zdotdir_str = zdotdir.to_string_lossy().to_string();
    let _zdotdir_env = EnvGuard::set(&lock, "ZDOTDIR", &zdotdir_str);

    let err = prompts::read_template("unreadable").unwrap_err();
    match err {
        PromptTemplateError::ReadFailed { path } => assert_eq!(path, template),
        other => panic!("unexpected error: {other:?}"),
    }
}
