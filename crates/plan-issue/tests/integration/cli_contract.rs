use std::path::PathBuf;

use clap::Parser;
use pretty_assertions::assert_eq;

use plan_issue::cli::{Cli, OutputFormat};
use plan_issue::commands::plan::LinkPrStatus;
use plan_issue::commands::record::{
    LifecycleCommentKind, RecordCommand, RecordProfile, TaskLedgerDisplay,
};
use plan_issue::commands::{Command, PrGrouping, SplitStrategy};

use crate::common;
#[test]
fn cli_help_lists_full_surface_for_live_and_local_bins() {
    let live = common::run_plan_issue(&["--help"]);
    assert_eq!(live.code, 0, "stderr: {}", live.stderr_text());

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
        // Task 1.5
        "resolve-approval",
        "record",
        "-V, --version",
        "USAGE PATHS",
        "plan-issue-local",
    ] {
        assert!(
            live.stdout_text().contains(token),
            "help output missing token `{token}`\n{}",
            live.stdout_text()
        );
    }

    let local = common::run_plan_issue_local(&["--help"]);
    assert_eq!(local.code, 0, "stderr: {}", local.stderr_text());
    assert!(
        local.stdout_text().contains("plan-issue-local"),
        "{}",
        local.stdout_text()
    );
    assert!(
        local.stdout_text().contains("USAGE PATHS"),
        "{}",
        local.stdout_text()
    );
    assert!(
        local
            .stdout_text()
            .contains("UNSUPPORTED IN PLAN-ISSUE-LOCAL"),
        "{}",
        local.stdout_text()
    );
    assert!(
        local.stdout_text().contains("USE INSTEAD"),
        "{}",
        local.stdout_text()
    );
    assert!(
        local.stdout_text().contains("plan-issue <command>"),
        "{}",
        local.stdout_text()
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
        "crates/plan-issue/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md",
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
                    "crates/plan-issue/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md"
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
        "crates/plan-issue/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md",
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

// --- Sprint 1: plan-issue-lifecycle-v3 record surface ---

#[test]
fn cli_parse_contract_record_open_accepts_plan_bundle() {
    let cli = Cli::try_parse_from([
        "plan-issue",
        "record",
        "open",
        "--bundle",
        "docs/plans/plan-issue-lifecycle-v3",
        "--profile",
        "tracking",
    ])
    .expect("parse record open");

    cli.validate().expect("validation");

    match &cli.command {
        Command::Record(args) => match &args.command {
            RecordCommand::Open(open) => {
                assert_eq!(open.profile, RecordProfile::Tracking);
                assert_eq!(
                    open.bundle.as_deref(),
                    Some(PathBuf::from("docs/plans/plan-issue-lifecycle-v3").as_path()),
                );
                assert!(open.source_file.is_none());
                assert!(open.plan_file.is_none());
                assert!(open.execution_state_file.is_none());
                assert!(!open.allow_dirty);
                assert!(open.fixture.is_none());
            }
            other => panic!("unexpected record subcommand: {other:?}"),
        },
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parse_contract_record_open_accepts_explicit_bundle_paths() {
    let cli = Cli::try_parse_from([
        "plan-issue",
        "record",
        "open",
        "--source-file",
        "docs/plans/example/example-discussion-source.md",
        "--plan-file",
        "docs/plans/example/example-plan.md",
        "--execution-state-file",
        "docs/plans/example/example-execution-state.md",
        "--title",
        "Example Plan",
        "--allow-dirty",
    ])
    .expect("parse record open with explicit files");

    cli.validate().expect("validation");

    match &cli.command {
        Command::Record(args) => match &args.command {
            RecordCommand::Open(open) => {
                assert_eq!(
                    open.source_file,
                    Some(PathBuf::from(
                        "docs/plans/example/example-discussion-source.md"
                    ))
                );
                assert_eq!(
                    open.plan_file,
                    Some(PathBuf::from("docs/plans/example/example-plan.md"))
                );
                assert_eq!(
                    open.execution_state_file,
                    Some(PathBuf::from(
                        "docs/plans/example/example-execution-state.md"
                    ))
                );
                assert_eq!(open.title.as_deref(), Some("Example Plan"));
                assert!(open.allow_dirty);
            }
            other => panic!("unexpected record subcommand: {other:?}"),
        },
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parse_contract_record_attach_accepts_existing_issue_bundle() {
    let cli = Cli::try_parse_from([
        "plan-issue",
        "record",
        "attach",
        "--issue",
        "69",
        "--bundle",
        "docs/plans/support-matrix-rendered",
        "--profile",
        "tracking",
        "--title",
        "Existing Issue",
        "--allow-dirty",
    ])
    .expect("parse record attach");

    cli.validate().expect("validation");

    match &cli.command {
        Command::Record(args) => match &args.command {
            RecordCommand::Attach(attach) => {
                assert_eq!(attach.issue, "69");
                assert_eq!(attach.profile, RecordProfile::Tracking);
                assert_eq!(
                    attach.bundle.as_deref(),
                    Some(PathBuf::from("docs/plans/support-matrix-rendered").as_path()),
                );
                assert_eq!(attach.title.as_deref(), Some("Existing Issue"));
                assert!(attach.allow_dirty);
            }
            other => panic!("unexpected record subcommand: {other:?}"),
        },
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parse_contract_record_post_accepts_kind_and_payload_file() {
    let cli = Cli::try_parse_from([
        "plan-issue",
        "record",
        "post",
        "--issue",
        "448",
        "--profile",
        "tracking",
        "--kind",
        "state",
        "--payload-file",
        "state.json",
    ])
    .expect("parse record post");

    cli.validate().expect("validation");

    match &cli.command {
        Command::Record(args) => match &args.command {
            RecordCommand::Post(post) => {
                assert_eq!(post.issue, "448");
                assert_eq!(post.profile, RecordProfile::Tracking);
                assert_eq!(post.kind, LifecycleCommentKind::State);
                assert_eq!(post.payload_file, Some(PathBuf::from("state.json")));
                assert!(post.fixture.is_none());
            }
            other => panic!("unexpected record subcommand: {other:?}"),
        },
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parse_contract_record_post_rejects_marker_family_flag() {
    let err = Cli::try_parse_from([
        "plan-issue",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--marker-family",
        "compat",
    ])
    .expect_err("clap should reject the v1-only --marker-family flag");

    let rendered = err.to_string();
    assert!(
        rendered.contains("--marker-family"),
        "expected rejection to mention --marker-family: {rendered}",
    );
}

#[test]
fn cli_parse_contract_record_rejects_retired_helper_subcommands() {
    for subcommand in [
        "render-dashboard",
        "render-comment",
        "closeout-gate",
        "build-dispatch-ledger",
    ] {
        let err = Cli::try_parse_from(["plan-issue", "record", subcommand])
            .expect_err(&format!("retired helper should not parse: {subcommand}"));
        let rendered = err.to_string();
        assert!(
            rendered.contains(subcommand) || rendered.contains("unrecognized subcommand"),
            "expected rejection to mention {subcommand}: {rendered}",
        );
    }
}

#[test]
fn cli_parse_contract_record_repair_dashboard_accepts_fixture_inputs() {
    let cli = Cli::try_parse_from([
        "plan-issue",
        "record",
        "repair-dashboard",
        "--body-file",
        "issue-body.md",
        "--comments-json",
        "comments.json",
        "--out",
        "dashboard.md",
    ])
    .expect("parse record repair-dashboard");

    cli.validate().expect("validation");

    match &cli.command {
        Command::Record(args) => match &args.command {
            RecordCommand::RepairDashboard(repair) => {
                assert_eq!(repair.body_file, Some(PathBuf::from("issue-body.md")));
                assert_eq!(repair.comments_json, Some(PathBuf::from("comments.json")));
                assert_eq!(repair.out, Some(PathBuf::from("dashboard.md")));
                assert!(repair.fixture.is_none());
                assert!(repair.issue.is_none());
            }
            other => panic!("unexpected record subcommand: {other:?}"),
        },
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parse_contract_record_close_accepts_strict_inputs() {
    let cli = Cli::try_parse_from([
        "plan-issue",
        "record",
        "close",
        "--issue",
        "448",
        "--linked-pr",
        "sympoies/nils-cli#500",
        "--linked-pr",
        "sympoies/nils-cli#501",
        "--approval",
        "https://github.com/sympoies/nils-cli/issues/448#issuecomment-1",
        "--bundle",
        "docs/plans/plan-issue-lifecycle-v3",
    ])
    .expect("parse record close");

    cli.validate().expect("validation");

    match &cli.command {
        Command::Record(args) => match &args.command {
            RecordCommand::Close(close) => {
                assert_eq!(close.issue, "448");
                assert_eq!(
                    close.linked_pr,
                    vec![
                        "sympoies/nils-cli#500".to_string(),
                        "sympoies/nils-cli#501".to_string(),
                    ]
                );
                assert_eq!(
                    close.approval.as_deref(),
                    Some("https://github.com/sympoies/nils-cli/issues/448#issuecomment-1"),
                );
                assert_eq!(
                    close.bundle,
                    Some(PathBuf::from("docs/plans/plan-issue-lifecycle-v3"))
                );
                assert!(close.body_file.is_none());
                assert!(close.comments_json.is_none());
                assert!(close.fixture.is_none());
            }
            other => panic!("unexpected record subcommand: {other:?}"),
        },
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parse_contract_record_close_rejects_require_complete_flag() {
    let err = Cli::try_parse_from([
        "plan-issue",
        "record",
        "close",
        "--issue",
        "448",
        "--require-complete",
    ])
    .expect_err("clap should reject the v1 --require-complete flag on record close");

    let rendered = err.to_string();
    assert!(
        rendered.contains("--require-complete"),
        "expected rejection to mention --require-complete: {rendered}",
    );
}

#[test]
fn cli_parse_contract_record_close_rejects_other_require_flags() {
    for flag in [
        "--require-session",
        "--require-validation",
        "--require-review",
        "--require-closeout",
    ] {
        let parsed = Cli::try_parse_from(["plan-issue", "record", "close", "--issue", "448", flag]);
        let err = parsed.expect_err(&format!("clap should reject v1 require flag {flag}"));
        let rendered = err.to_string();
        assert!(
            rendered.contains(flag),
            "expected rejection to mention {flag}: {rendered}",
        );
    }
}

#[test]
fn cli_parse_contract_record_close_accepts_fixture_mode() {
    let cli = Cli::try_parse_from([
        "plan-issue",
        "record",
        "close",
        "--issue",
        "448",
        "--linked-pr",
        "sympoies/nils-cli#500",
        "--approval",
        "approved",
        "--fixture",
        "tests/fixtures/lifecycle/example",
        "--body-file",
        "issue-body.md",
        "--comments-json",
        "comments.json",
    ])
    .expect("parse record close with fixture");

    cli.validate().expect("validation");

    match &cli.command {
        Command::Record(args) => match &args.command {
            RecordCommand::Close(close) => {
                assert_eq!(
                    close.fixture,
                    Some(PathBuf::from("tests/fixtures/lifecycle/example"))
                );
                assert_eq!(close.body_file, Some(PathBuf::from("issue-body.md")));
                assert_eq!(close.comments_json, Some(PathBuf::from("comments.json")));
            }
            other => panic!("unexpected record subcommand: {other:?}"),
        },
        other => panic!("unexpected command parsed: {other:?}"),
    }
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

#[test]
fn cli_parse_contract_record_open_accepts_repeatable_label() {
    let cli = Cli::try_parse_from([
        "plan-issue",
        "record",
        "open",
        "--bundle",
        "docs/plans/example",
        "--label",
        "workflow::plan",
        "--label",
        "state::needs-triage",
    ])
    .expect("parse record open with labels");

    cli.validate().expect("validation");

    match &cli.command {
        Command::Record(args) => match &args.command {
            RecordCommand::Open(open) => {
                assert_eq!(open.labels, vec!["workflow::plan", "state::needs-triage"]);
            }
            other => panic!("unexpected record subcommand: {other:?}"),
        },
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parse_contract_record_post_accepts_add_remove_label() {
    let cli = Cli::try_parse_from([
        "plan-issue",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--add-label",
        "state::blocked",
        "--remove-label",
        "state::in-progress",
    ])
    .expect("parse record post with label mutations");

    cli.validate().expect("validation");

    match &cli.command {
        Command::Record(args) => match &args.command {
            RecordCommand::Post(post) => {
                assert_eq!(post.add_labels, vec!["state::blocked"]);
                assert_eq!(post.remove_labels, vec!["state::in-progress"]);
            }
            other => panic!("unexpected record subcommand: {other:?}"),
        },
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parse_contract_record_post_accepts_execution_state_file_and_display() {
    let cli = Cli::try_parse_from([
        "plan-issue",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--execution-state-file",
        "docs/plans/example/example-execution-state.md",
        "--task-ledger-display",
        "collapsed",
    ])
    .expect("parse record post with execution state file");

    cli.validate().expect("validation");

    match &cli.command {
        Command::Record(args) => match &args.command {
            RecordCommand::Post(post) => {
                assert_eq!(
                    post.execution_state_file,
                    Some(PathBuf::from(
                        "docs/plans/example/example-execution-state.md"
                    ))
                );
                assert_eq!(post.task_ledger_display, TaskLedgerDisplay::Collapsed);
            }
            other => panic!("unexpected record subcommand: {other:?}"),
        },
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parse_contract_record_post_accepts_open_task_ledger_display() {
    let cli = Cli::try_parse_from([
        "plan-issue",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--task-ledger-display",
        "open",
    ])
    .expect("parse record post with open display");

    cli.validate().expect("validation");

    match &cli.command {
        Command::Record(args) => match &args.command {
            RecordCommand::Post(post) => {
                assert_eq!(post.task_ledger_display, TaskLedgerDisplay::Open);
            }
            other => panic!("unexpected record subcommand: {other:?}"),
        },
        other => panic!("unexpected command parsed: {other:?}"),
    }
}

#[test]
fn cli_parse_contract_record_close_accepts_add_remove_label() {
    let cli = Cli::try_parse_from([
        "plan-issue",
        "record",
        "close",
        "--issue",
        "448",
        "--linked-pr",
        "sympoies/nils-cli#500",
        "--approval",
        "https://github.com/sympoies/nils-cli/issues/448#issuecomment-1",
        "--add-label",
        "state::closed",
        "--remove-label",
        "state::in-progress",
    ])
    .expect("parse record close with label mutations");

    cli.validate().expect("validation");

    match &cli.command {
        Command::Record(args) => match &args.command {
            RecordCommand::Close(close) => {
                assert_eq!(close.add_labels, vec!["state::closed"]);
                assert_eq!(close.remove_labels, vec!["state::in-progress"]);
            }
            other => panic!("unexpected record subcommand: {other:?}"),
        },
        other => panic!("unexpected command parsed: {other:?}"),
    }
}
