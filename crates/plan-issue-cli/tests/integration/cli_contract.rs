use std::path::PathBuf;

use clap::Parser;
use pretty_assertions::assert_eq;

use plan_issue_cli::cli::{Cli, OutputFormat};
use plan_issue_cli::commands::plan::LinkPrStatus;
use plan_issue_cli::commands::{Command, PrGrouping, SplitStrategy};

use crate::common;
#[test]
fn cli_help_lists_full_surface_for_live_and_local_bins() {
    let live = common::run_plan_issue(&["--help"]);
    assert_eq!(live.code, 0, "stderr: {}", live.stderr);

    for token in [
        "build-task-spec",
        "build-plan-task-spec",
        "start-plan",
        "status-plan",
        "link-pr",
        "ready-plan",
        "close-plan",
        "cleanup-worktrees",
        "start-sprint",
        "ready-sprint",
        "accept-sprint",
        "multi-sprint-guide",
        "-V, --version",
        "Usage paths",
        "plan-issue-local",
    ] {
        assert!(
            live.stdout.contains(token),
            "help output missing token `{token}`\n{}",
            live.stdout
        );
    }

    let local = common::run_plan_issue_local(&["--help"]);
    assert_eq!(local.code, 0, "stderr: {}", local.stderr);
    assert!(
        local.stdout.contains("plan-issue-local"),
        "{}",
        local.stdout
    );
    assert!(local.stdout.contains("Usage paths"), "{}", local.stdout);
    assert!(
        local.stdout.contains("Unsupported in plan-issue-local"),
        "{}",
        local.stdout
    );
    assert!(local.stdout.contains("Use instead"), "{}", local.stdout);
    assert!(
        local.stdout.contains("plan-issue <command>"),
        "{}",
        local.stdout
    );
}

#[test]
fn cli_parse_contract_link_pr_supports_task_and_status_targeting() {
    let cli = Cli::try_parse_from([
        "plan-issue",
        "link-pr",
        "--body-file",
        "issue-body.md",
        "--task",
        "S2T1",
        "--pr",
        "https://github.com/sympoies/nils-cli/pull/221",
        "--status",
        "blocked",
    ])
    .expect("parse link-pr");

    cli.validate().expect("validation");

    match &cli.command {
        Command::LinkPr(args) => {
            assert_eq!(args.body_file, Some(PathBuf::from("issue-body.md")));
            assert_eq!(args.task.as_deref(), Some("S2T1"));
            assert_eq!(args.sprint, None);
            assert_eq!(args.pr_group, None);
            assert_eq!(args.pr, "https://github.com/sympoies/nils-cli/pull/221");
            assert_eq!(args.status, LinkPrStatus::Blocked);
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parse_contract_build_task_spec_accepts_per_spring_alias() {
    let cli = Cli::try_parse_from([
        "plan-issue",
        "build-task-spec",
        "--plan",
        "crates/plan-issue-cli/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md",
        "--sprint",
        "2",
        "--pr-grouping",
        "per-spring",
    ])
    .expect("parse build-task-spec");

    assert_eq!(
        cli.resolve_output_format().expect("output format"),
        OutputFormat::Text
    );
    cli.validate().expect("validation");

    match &cli.command {
        Command::BuildTaskSpec(args) => {
            assert_eq!(
                args.plan,
                PathBuf::from(
                    "crates/plan-issue-cli/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md"
                )
            );
            assert_eq!(args.sprint, 2);
            assert_eq!(args.grouping.pr_grouping, Some(PrGrouping::PerSprint));
            assert_eq!(args.grouping.default_pr_grouping, None);
            assert_eq!(args.grouping.strategy, SplitStrategy::Deterministic);
            assert!(args.grouping.pr_group.is_empty());
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parse_contract_start_sprint_parses_typed_group_mapping() {
    let cli = Cli::try_parse_from([
        "plan-issue",
        "start-sprint",
        "--plan",
        "crates/plan-issue-cli/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md",
        "--issue",
        "217",
        "--sprint",
        "2",
        "--strategy",
        "auto",
        "--default-pr-grouping",
        "group",
        "--pr-group",
        "S2T1=s2-core",
        "--pr-group",
        "Task 2.2=s2-core",
    ])
    .expect("parse start-sprint");

    cli.validate().expect("validation");

    match &cli.command {
        Command::StartSprint(args) => {
            assert_eq!(args.issue, 217);
            assert_eq!(args.sprint, 2);
            assert_eq!(args.grouping.pr_grouping, None);
            assert_eq!(args.grouping.default_pr_grouping, Some(PrGrouping::Group));
            assert_eq!(args.grouping.strategy, SplitStrategy::Auto);
            assert_eq!(args.grouping.pr_group.len(), 2);
            assert_eq!(args.grouping.pr_group[0].task, "S2T1");
            assert_eq!(args.grouping.pr_group[0].group, "s2-core");
            assert_eq!(args.grouping.pr_group[1].task, "Task 2.2");
            assert_eq!(args.grouping.pr_group[1].group, "s2-core");
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parse_contract_start_plan_auto_accepts_default_grouping() {
    let cli = Cli::try_parse_from([
        "plan-issue",
        "start-plan",
        "--plan",
        "plan.md",
        "--strategy",
        "auto",
        "--default-pr-grouping",
        "group",
    ])
    .expect("parse start-plan");

    cli.validate().expect("validation");

    match &cli.command {
        Command::StartPlan(args) => {
            assert_eq!(args.grouping.pr_grouping, None);
            assert_eq!(args.grouping.default_pr_grouping, Some(PrGrouping::Group));
            assert_eq!(args.grouping.strategy, SplitStrategy::Auto);
            assert!(args.grouping.pr_group.is_empty());
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parse_contract_ready_sprint_auto_accepts_default_grouping() {
    let cli = Cli::try_parse_from([
        "plan-issue",
        "ready-sprint",
        "--plan",
        "plan.md",
        "--issue",
        "217",
        "--sprint",
        "2",
        "--strategy",
        "auto",
        "--default-pr-grouping",
        "group",
    ])
    .expect("parse ready-sprint");

    cli.validate().expect("validation");

    match &cli.command {
        Command::ReadySprint(args) => {
            assert_eq!(args.grouping.pr_grouping, None);
            assert_eq!(args.grouping.default_pr_grouping, Some(PrGrouping::Group));
            assert_eq!(args.grouping.strategy, SplitStrategy::Auto);
            assert!(args.grouping.pr_group.is_empty());
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parse_contract_accept_sprint_auto_accepts_default_grouping() {
    let cli = Cli::try_parse_from([
        "plan-issue",
        "accept-sprint",
        "--plan",
        "plan.md",
        "--issue",
        "217",
        "--sprint",
        "2",
        "--strategy",
        "auto",
        "--default-pr-grouping",
        "group",
        "--approved-comment-url",
        "https://example.invalid/review",
    ])
    .expect("parse accept-sprint");

    cli.validate().expect("validation");

    match &cli.command {
        Command::AcceptSprint(args) => {
            assert_eq!(args.grouping.pr_grouping, None);
            assert_eq!(args.grouping.default_pr_grouping, Some(PrGrouping::Group));
            assert_eq!(args.grouping.strategy, SplitStrategy::Auto);
            assert!(args.grouping.pr_group.is_empty());
        }
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_conflict_rules_reject_pr_group_without_group_mode() {
    let cli = Cli::try_parse_from([
        "plan-issue",
        "build-plan-task-spec",
        "--plan",
        "plan.md",
        "--pr-grouping",
        "per-sprint",
        "--pr-group",
        "S2T1=s2-core",
    ])
    .expect("parse should succeed before semantic validation");

    let err = cli.validate().expect_err("semantic validation should fail");
    assert_eq!(err.code, "invalid-pr-grouping");
    assert!(err.message.contains("only valid"), "{}", err.message);
}

#[test]
fn cli_conflict_rules_require_pr_group_mapping_for_group_mode() {
    let cli = Cli::try_parse_from([
        "plan-issue",
        "build-plan-task-spec",
        "--plan",
        "plan.md",
        "--pr-grouping",
        "group",
    ])
    .expect("parse should succeed before semantic validation");

    let err = cli.validate().expect_err("semantic validation should fail");
    assert_eq!(err.code, "invalid-pr-grouping");
    assert!(err.message.contains("with --strategy deterministic"));
}

#[test]
fn cli_conflict_rules_auto_allows_no_pr_group_mapping() {
    let cli = Cli::try_parse_from([
        "plan-issue",
        "build-plan-task-spec",
        "--plan",
        "plan.md",
        "--strategy",
        "auto",
        "--default-pr-grouping",
        "group",
    ])
    .expect("parse should succeed before semantic validation");

    cli.validate().expect("auto should allow empty --pr-group");
}

#[test]
fn cli_conflict_rules_reject_pr_grouping_with_auto() {
    let cli = Cli::try_parse_from([
        "plan-issue",
        "build-plan-task-spec",
        "--plan",
        "plan.md",
        "--strategy",
        "auto",
        "--pr-grouping",
        "group",
    ])
    .expect("parse should succeed before semantic validation");

    let err = cli.validate().expect_err("semantic validation should fail");
    assert_eq!(err.code, "invalid-pr-grouping");
    assert!(err.message.contains("cannot be used with --strategy auto"));
}

#[test]
fn cli_conflict_rules_reject_default_pr_grouping_with_deterministic() {
    let cli = Cli::try_parse_from([
        "plan-issue",
        "build-plan-task-spec",
        "--plan",
        "plan.md",
        "--pr-grouping",
        "group",
        "--default-pr-grouping",
        "group",
    ])
    .expect("parse should succeed before semantic validation");

    let err = cli.validate().expect_err("semantic validation should fail");
    assert_eq!(err.code, "invalid-pr-grouping");
    assert!(err.message.contains("only valid when --strategy auto"));
}

#[test]
fn cli_conflict_rules_reject_summary_and_summary_file_together() {
    let err = Cli::try_parse_from([
        "plan-issue",
        "ready-plan",
        "--issue",
        "217",
        "--summary",
        "done",
        "--summary-file",
        "summary.md",
    ])
    .expect_err("clap should reject conflicting args");

    let rendered = err.to_string();
    assert!(
        rendered.contains("cannot be used with") || rendered.contains("conflicts with"),
        "{rendered}"
    );
}
