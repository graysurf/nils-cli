use gemini_cli::prompts;
use nils_test_support::{EnvGuard, GlobalStateLock};
use prompts::PromptTemplateError;
use std::fs;
fn set_env(lock: &GlobalStateLock, key: &str, value: impl AsRef<std::ffi::OsStr>) -> EnvGuard {
    let value = value.as_ref().to_string_lossy().into_owned();
    EnvGuard::set(lock, key, &value)
}

#[test]
fn prompts_resolve_uses_feature_dir_fallback_when_zdotdir_prompts_missing() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let zdotdir = dir.path().join("zdotdir");
    fs::create_dir_all(&zdotdir).expect("zdotdir");

    let script_dir = dir.path().join("scripts");
    let feature_prompts = script_dir.join("_features").join("gemini").join("prompts");
    fs::create_dir_all(&feature_prompts).expect("feature prompts");
    fs::write(
        feature_prompts.join("actionable-advice.md"),
        "Hello $ARGUMENTS\n",
    )
    .expect("write template");

    let _zdotdir_env = set_env(&lock, "ZDOTDIR", zdotdir.as_os_str());
    let _script_env = set_env(&lock, "ZSH_SCRIPT_DIR", script_dir.as_os_str());

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
    let feature_dir = script_dir.join("_features").join("gemini");
    fs::create_dir_all(&feature_dir).expect("feature dir");

    let _zdotdir_env = set_env(&lock, "ZDOTDIR", zdotdir.as_os_str());
    let _script_env = set_env(&lock, "ZSH_SCRIPT_DIR", script_dir.as_os_str());

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

    let _zdotdir_env = set_env(&lock, "ZDOTDIR", zdotdir.as_os_str());
    let _script_env = set_env(&lock, "ZSH_SCRIPT_DIR", script_dir.as_os_str());

    let err = prompts::read_template("anything").expect_err("missing prompts dir");
    assert!(matches!(err, PromptTemplateError::PromptsDirNotFound));
}

#[test]
fn prompts_read_template_errors_when_template_missing() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let zdotdir = dir.path().join("zdotdir");
    let prompts_dir = zdotdir.join("prompts");
    fs::create_dir_all(&prompts_dir).expect("prompts dir");

    let _zdotdir_env = set_env(&lock, "ZDOTDIR", zdotdir.as_os_str());

    let err = prompts::read_template("missing-template").expect_err("missing template");
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

    #[cfg(unix)]
    fs::write(&template, [0xFF]).expect("write invalid template");

    #[cfg(not(unix))]
    fs::write(&template, "hi").expect("write template");

    let _zdotdir_env = set_env(&lock, "ZDOTDIR", zdotdir.as_os_str());

    #[cfg(unix)]
    {
        let err = prompts::read_template("unreadable").expect_err("unreadable template");
        match err {
            PromptTemplateError::ReadFailed { path } => assert_eq!(path, template),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[cfg(not(unix))]
    {
        let (path, content) = prompts::read_template("unreadable").expect("read template");
        assert_eq!(path, template);
        assert_eq!(content, "hi");
    }
}
