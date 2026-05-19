use agent_docs::run_with_args;
use std::ffi::OsString;

#[test]
fn unknown_subcommand_exits_usage() {
    let code = run_with_args([OsString::from("agent-docs"), OsString::from("bogus")]);
    assert_eq!(code, 64);
}

#[test]
fn missing_subcommand_exits_clap_help() {
    // No subcommand → clap renders help (exit 2).
    let code = run_with_args([OsString::from("agent-docs")]);
    assert!(matches!(code, 0 | 2), "unexpected exit code {code}");
}

#[test]
fn help_flag_exits_success() {
    let code = run_with_args([OsString::from("agent-docs"), OsString::from("--help")]);
    assert_eq!(code, 0);
}
