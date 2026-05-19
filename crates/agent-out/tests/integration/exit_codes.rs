use std::path::Path;
use std::process::Command;

fn agent_out_bin() -> std::path::PathBuf {
    for key in ["CARGO_BIN_EXE_agent-out", "CARGO_BIN_EXE_agent_out"] {
        if let Ok(path) = std::env::var(key) {
            return path.into();
        }
    }

    let exe = std::env::current_exe().expect("current exe");
    let target_dir = exe
        .parent()
        .and_then(|path| path.parent())
        .expect("target dir");
    target_dir.join(format!("agent-out{}", std::env::consts::EXE_SUFFIX))
}

fn run_exit_code(dir: &Path, args: &[&str]) -> i32 {
    let output = Command::new(agent_out_bin())
        .args(args)
        .current_dir(dir)
        .output()
        .expect("agent-out command");
    output.status.code().unwrap_or(-1)
}

#[test]
fn unknown_subcommand_exits_usage() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    assert_eq!(run_exit_code(dir.path(), &["bogus-command"]), 64);
}

#[test]
fn missing_subcommand_exits_usage() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    assert_eq!(run_exit_code(dir.path(), &[]), 64);
}

#[test]
fn help_flag_exits_success() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    assert_eq!(run_exit_code(dir.path(), &["--help"]), 0);
}
