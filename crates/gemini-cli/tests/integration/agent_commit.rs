use gemini_cli::agent;
use nils_common::process as shared_process;
use nils_test_support::{
    CwdGuard, EnvGuard, GlobalStateLock, StubBinDir, git as test_git, prepend_path,
};
use std::fs;
use std::path::Path;

fn set_env(lock: &GlobalStateLock, key: &str, value: impl AsRef<std::ffi::OsStr>) -> EnvGuard {
    let value = value.as_ref().to_string_lossy().into_owned();
    EnvGuard::set(lock, key, &value)
}

fn real_git_path() -> String {
    shared_process::find_in_path("git")
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|| panic!("git not found in PATH for tests"))
}

fn write_stub_git(stubs: &StubBinDir) {
    let git = real_git_path();
    let script = format!(
        r#"#!/bin/sh
exec "{git}" "$@"
"#
    );
    stubs.write_exe("git", &script);
}

fn write_stub_semantic_commit(stubs: &StubBinDir) {
    stubs.write_exe("semantic-commit", "#!/bin/sh\nexit 0\n");
}

fn write_stub_gemini(stubs: &StubBinDir) {
    let script = r#"#!/bin/sh
set -eu
out_dir="${GEMINI_STUB_OUT_DIR:?missing GEMINI_STUB_OUT_DIR}"
i=0
for arg in "$@"; do
  printf '%s' "$arg" > "$out_dir/arg-$i"
  i=$((i+1))
done
"#;
    stubs.write_exe("gemini", script);
}

fn init_repo(path: &Path) {
    test_git::init_repo_at_with(path, test_git::InitRepoOptions::new().without_branch());
}

#[test]
fn agent_commit_returns_1_when_git_missing() {
    let lock = GlobalStateLock::new();
    let _path = EnvGuard::set(&lock, "PATH", "");

    let options = agent::commit::CommitOptions {
        push: false,
        auto_stage: false,
        extra: Vec::new(),
    };
    let code = agent::commit::run(&options);
    assert_eq!(code, 1);
}

#[test]
fn agent_commit_semantic_mode_executes_gemini_with_template_and_push_note() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_repo(&repo);
    fs::write(repo.join("a.txt"), "hello").expect("write file");

    let zdotdir = dir.path().join("zdotdir");
    let prompts_dir = zdotdir.join("prompts");
    fs::create_dir_all(&prompts_dir).expect("prompts dir");
    fs::write(
        prompts_dir.join("semantic-commit-autostage.md"),
        "SEMANTIC_AUTOSTAGE\n",
    )
    .expect("write prompt template");

    let stubs = StubBinDir::new();
    write_stub_git(&stubs);
    write_stub_semantic_commit(&stubs);
    write_stub_gemini(&stubs);

    let out_dir = dir.path().join("out");
    fs::create_dir_all(&out_dir).expect("out dir");

    let _path = prepend_path(&lock, stubs.path());
    let _danger = EnvGuard::set(&lock, "GEMINI_ALLOW_DANGEROUS_ENABLED", "true");
    let _model = EnvGuard::set(&lock, "GEMINI_CLI_MODEL", "m-test");
    let _reasoning = EnvGuard::set(&lock, "GEMINI_CLI_REASONING", "low");
    let _zdotdir = set_env(&lock, "ZDOTDIR", zdotdir.as_os_str());
    let _out_dir = set_env(&lock, "GEMINI_STUB_OUT_DIR", out_dir.as_os_str());
    let _cwd = CwdGuard::set(&lock, &repo).expect("set cwd");

    let options = agent::commit::CommitOptions {
        push: true,
        auto_stage: true,
        extra: vec!["extra".to_string(), "words".to_string()],
    };
    let code = agent::commit::run(&options);
    assert_eq!(code, 0);

    let prompt = fs::read_to_string(out_dir.join("arg-0")).expect("prompt");
    assert!(prompt.contains("SEMANTIC_AUTOSTAGE"));
    assert!(prompt.contains("Furthermore, please push the committed changes"));
    assert!(prompt.contains("Additional instructions from user:"));
    assert!(prompt.contains("extra words"));

    let arg2 = fs::read_to_string(out_dir.join("arg-2")).expect("model");
    assert_eq!(arg2, "m-test");
}
