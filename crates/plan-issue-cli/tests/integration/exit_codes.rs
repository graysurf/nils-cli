use crate::common;

#[test]
fn unknown_subcommand_exits_usage() {
    let out = common::run_plan_issue_local(&["bogus"]);
    assert_eq!(out.code, 64);
}

#[test]
fn missing_required_args_exits_usage() {
    let out = common::run_plan_issue_local(&["start-sprint"]);
    assert_eq!(out.code, 64);
}

#[test]
fn help_flag_exits_success() {
    let out = common::run_plan_issue_local(&["--help"]);
    assert_eq!(out.code, 0);
}
