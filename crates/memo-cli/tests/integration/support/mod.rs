use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};
use std::fs;
use std::path::{Path, PathBuf};

#[allow(dead_code)]
pub fn test_db_path(name: &str) -> PathBuf {
    let dir = tempfile::tempdir().expect("tempdir should be created");
    dir.keep().join(format!("{name}.db"))
}

#[allow(dead_code)]
pub fn parse_json_stdout(output: &CmdOutput) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).expect("stdout should be valid JSON")
}

#[allow(dead_code)]
pub fn fixture_json(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    let raw = fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!("failed to read fixture {}: {err}", path.display());
    });
    serde_json::from_str(&raw).unwrap_or_else(|err| {
        panic!("failed to parse fixture {}: {err}", path.display());
    })
}

#[allow(dead_code)]
pub fn run_memo_cli(db_path: &Path, args: &[&str], stdin: Option<&str>) -> CmdOutput {
    run_memo_cli_with_env(db_path, args, stdin, &[])
}

#[allow(dead_code)]
pub fn run_memo_cli_with_env(
    db_path: &Path,
    args: &[&str],
    stdin: Option<&str>,
    envs: &[(&str, &str)],
) -> CmdOutput {
    let db = db_path.display().to_string();
    let mut argv = vec!["--db".to_string(), db];
    argv.extend(args.iter().map(|arg| (*arg).to_string()));
    let argv_ref: Vec<&str> = argv.iter().map(|arg| arg.as_str()).collect();

    let mut options = CmdOptions::new();
    for (key, value) in envs {
        options = options.with_env(key, value);
    }
    if let Some(input) = stdin {
        options = options.with_stdin_str(input);
    }

    run_resolved("memo-cli", &argv_ref, &options)
}
