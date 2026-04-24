use std::path::Path;
use std::process::Output;

use nils_test_support::cmd::run_resolved_in_dir_with_stdin_str;
use nils_test_support::fs::{write_executable_in_dir, write_text_in_dir};
use nils_test_support::git::init_repo_main;
#[allow(unused_imports)]
pub use nils_test_support::git::{git, git_output};

#[allow(dead_code)]
pub fn init_repo() -> tempfile::TempDir {
    init_repo_main()
}

#[allow(dead_code)]
pub fn write_file(dir: &Path, name: &str, contents: &str) {
    write_text_in_dir(dir, name, contents);
}

#[allow(dead_code)]
pub fn run_semantic_commit_output(
    dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    input: Option<&str>,
) -> Output {
    let output = run_resolved_in_dir_with_stdin_str("semantic-commit", dir, args, envs, input);
    output.into_output()
}

#[allow(dead_code)]
pub fn write_executable(dir: &Path, rel: &str, contents: &str) {
    write_executable_in_dir(dir, rel, contents);
}
