use nils_common::cli_contract::exit;
use pretty_assertions::assert_eq;

use super::common;

#[test]
fn help_exposes_only_the_default_branch_command() {
    let repo = common::init_repo();

    let output = common::run_semantic_commit_output(repo.path(), &["--help"], &[], None);

    assert_eq!(output.status.code(), Some(exit::SUCCESS));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("default-branch"),
        "new command missing from help: {stdout}"
    );
    assert!(
        !stdout.contains("local-default"),
        "removed command remains in help: {stdout}"
    );
}

#[test]
fn default_branch_help_exposes_the_complete_public_option_contract() {
    struct HelpOption<'a> {
        long: &'a str,
        short: Option<char>,
        aliases: &'a [&'a str],
        value_name: Option<&'a str>,
    }

    let repo = common::init_repo();

    let output =
        common::run_semantic_commit_output(repo.path(), &["default-branch", "--help"], &[], None);

    assert_eq!(output.status.code(), Some(exit::SUCCESS));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let options = [
        HelpOption {
            long: "message",
            short: Some('m'),
            aliases: &[],
            value_name: Some("text"),
        },
        HelpOption {
            long: "message-file",
            short: Some('F'),
            aliases: &[],
            value_name: Some("path"),
        },
        HelpOption {
            long: "expect-head",
            short: None,
            aliases: &[],
            value_name: Some("full-sha"),
        },
        HelpOption {
            long: "receipt-out",
            short: None,
            aliases: &[],
            value_name: Some("path"),
        },
        HelpOption {
            long: "repo",
            short: None,
            aliases: &[],
            value_name: Some("path"),
        },
        HelpOption {
            long: "format",
            short: None,
            aliases: &[],
            value_name: Some("text|json"),
        },
        HelpOption {
            long: "json",
            short: None,
            aliases: &[],
            value_name: None,
        },
        HelpOption {
            long: "dry-run",
            short: None,
            aliases: &[],
            value_name: None,
        },
        HelpOption {
            long: "automation",
            short: None,
            aliases: &["non-interactive"],
            value_name: None,
        },
        HelpOption {
            long: "type",
            short: None,
            aliases: &[],
            value_name: Some("type"),
        },
        HelpOption {
            long: "scope",
            short: None,
            aliases: &[],
            value_name: Some("scope"),
        },
        HelpOption {
            long: "subject",
            short: None,
            aliases: &[],
            value_name: Some("subject"),
        },
        HelpOption {
            long: "body-bullet",
            short: None,
            aliases: &["bullet"],
            value_name: Some("text"),
        },
        HelpOption {
            long: "signoff",
            short: None,
            aliases: &[],
            value_name: None,
        },
        HelpOption {
            long: "trailer",
            short: None,
            aliases: &[],
            value_name: Some("token: value"),
        },
        HelpOption {
            long: "auto-fix",
            short: None,
            aliases: &[],
            value_name: None,
        },
        HelpOption {
            long: "max-header-width",
            short: None,
            aliases: &[],
            value_name: Some("N"),
        },
    ];

    for option in options {
        let long = format!("--{}", option.long);
        let signature = stdout
            .lines()
            .find(|line| {
                let line = line.trim_start();
                line.starts_with(&long)
                    || option
                        .short
                        .is_some_and(|short| line.starts_with(&format!("-{short},")))
            })
            .unwrap_or_else(|| panic!("{long} missing from help: {stdout}"));
        match option.value_name {
            Some(value_name) => assert!(
                signature.contains(&format!("<{value_name}>")),
                "{long} value arity missing from help signature: {signature}"
            ),
            None => assert!(
                !signature.contains('<'),
                "{long} flag unexpectedly advertises a value: {signature}"
            ),
        }
        if let Some(short) = option.short {
            assert!(
                signature.contains(&format!("-{short},")),
                "-{short} missing from {long} help signature: {signature}"
            );
        }
        for alias in option.aliases {
            assert!(
                stdout.contains(&format!("--{alias}")),
                "--{alias} alias missing from {long} help: {stdout}"
            );
        }
    }

    for spelling in ["-h", "--help"] {
        assert!(
            stdout.contains(spelling),
            "{spelling} missing from default-branch help: {stdout}"
        );
    }
    for repeatable in [
        "--body-bullet <text> may be repeated",
        "--trailer <token: value> may be repeated",
    ] {
        assert!(
            stdout.contains(repeatable),
            "repeatability missing from help: {repeatable}: {stdout}"
        );
    }
    assert!(
        stdout.contains("--expect-head <full-sha> is required"),
        "required option state missing from help: {stdout}"
    );
    assert!(
        stdout.contains("--receipt-out <path> is required for mutation"),
        "mutating receipt requirement missing from help: {stdout}"
    );
    assert!(
        stdout.contains("--receipt-out is forbidden with --dry-run"),
        "dry-run receipt prohibition missing from help: {stdout}"
    );
    for removed in [
        "--expected-branch",
        "--remote-mode",
        "--validate-only",
        "--message-out",
        "--no-progress",
        "--quiet",
    ] {
        assert!(
            !stdout.contains(removed),
            "removed flag {removed} remains in help: {stdout}"
        );
    }
    assert!(
        !stdout.contains("local-default"),
        "removed command remains in default-branch help: {stdout}"
    );
    assert!(
        stdout.contains("Never contacts or updates a remote"),
        "no-network boundary missing from help: {stdout}"
    );
}

#[test]
fn default_branch_help_spellings_share_one_truthful_long_help_contract() {
    let repo = common::init_repo();

    let short =
        common::run_semantic_commit_output(repo.path(), &["default-branch", "-h"], &[], None);
    let long =
        common::run_semantic_commit_output(repo.path(), &["default-branch", "--help"], &[], None);

    assert_eq!(short.status.code(), Some(exit::SUCCESS));
    assert_eq!(long.status.code(), Some(exit::SUCCESS));
    assert_eq!(short.stdout, long.stdout);

    let stdout = String::from_utf8_lossy(&long.stdout);
    assert!(
        stdout.contains("-h, --help") && stdout.contains("Print help"),
        "shared help option missing from rendered help: {stdout}"
    );
    assert!(
        !stdout.contains("summary with '-h'"),
        "help advertises a distinct short summary that the runtime does not render: {stdout}"
    );
}

#[test]
fn local_default_is_an_unknown_subcommand() {
    let repo = common::init_repo();

    let output =
        common::run_semantic_commit_output(repo.path(), &["local-default", "--help"], &[], None);

    assert_eq!(output.status.code(), Some(exit::USAGE));
}

#[test]
fn generated_completion_contains_only_the_new_command_and_flags() {
    let repo = common::init_repo();

    for shell in ["bash", "zsh"] {
        let output =
            common::run_semantic_commit_output(repo.path(), &["completion", shell], &[], None);
        assert_eq!(output.status.code(), Some(exit::SUCCESS), "{shell}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("default-branch"), "{shell}: {stdout}");
        assert!(!stdout.contains("local-default"), "{shell}: {stdout}");
        assert!(!stdout.contains("--expected-branch"), "{shell}: {stdout}");
        assert!(!stdout.contains("--remote-mode"), "{shell}: {stdout}");
    }
}

#[test]
fn default_branch_options_follow_the_public_parser_and_generated_shells() {
    struct AcceptedCase<'a> {
        spelling: &'a str,
        args: Vec<&'a str>,
        bash_needle: &'a str,
        zsh_needle: &'a str,
    }

    let repo = common::init_repo();
    let repo_path = repo.path().to_str().expect("repository path");
    let invalid_head = "f".repeat(40);
    let invalid_head = invalid_head.as_str();
    let output_dir = tempfile::tempdir().expect("contract output directory");
    let receipt_path = output_dir.path().join("receipt.json");
    let receipt = receipt_path.to_str().expect("receipt path");
    let message_path = output_dir.path().join("message.txt");
    let message_file = message_path.to_str().expect("message path");
    let cases = vec![
        AcceptedCase {
            spelling: "--message",
            args: vec![
                "--expect-head",
                invalid_head,
                "--dry-run",
                "--message",
                "docs: parser contract",
            ],
            bash_needle: "--message",
            zsh_needle: "--message=",
        },
        AcceptedCase {
            spelling: "-m",
            args: vec![
                "--expect-head",
                invalid_head,
                "--dry-run",
                "-m",
                "docs: parser contract",
            ],
            bash_needle: "opts=\"-m ",
            zsh_needle: "'-m+",
        },
        AcceptedCase {
            spelling: "--message-file",
            args: vec![
                "--expect-head",
                invalid_head,
                "--dry-run",
                "--message-file",
                message_file,
            ],
            bash_needle: "--message-file",
            zsh_needle: "--message-file=",
        },
        AcceptedCase {
            spelling: "-F",
            args: vec![
                "--expect-head",
                invalid_head,
                "--dry-run",
                "-F",
                message_file,
            ],
            bash_needle: " -F ",
            zsh_needle: "'-F+",
        },
        AcceptedCase {
            spelling: "--expect-head",
            args: vec![
                "--expect-head",
                invalid_head,
                "--dry-run",
                "--message",
                "docs: parser contract",
            ],
            bash_needle: "--expect-head",
            zsh_needle: "--expect-head=",
        },
        AcceptedCase {
            spelling: "--receipt-out",
            args: vec![
                "--expect-head",
                invalid_head,
                "--receipt-out",
                receipt,
                "--message",
                "docs: parser contract",
            ],
            bash_needle: "--receipt-out",
            zsh_needle: "--receipt-out=",
        },
        AcceptedCase {
            spelling: "--repo",
            args: vec![
                "--expect-head",
                invalid_head,
                "--dry-run",
                "--repo",
                repo_path,
                "--message",
                "docs: parser contract",
            ],
            bash_needle: "--repo",
            zsh_needle: "--repo=",
        },
        AcceptedCase {
            spelling: "--format",
            args: vec![
                "--expect-head",
                invalid_head,
                "--dry-run",
                "--format",
                "json",
                "--message",
                "docs: parser contract",
            ],
            bash_needle: "--format",
            zsh_needle: "--format=",
        },
        AcceptedCase {
            spelling: "--json",
            args: vec![
                "--expect-head",
                invalid_head,
                "--dry-run",
                "--json",
                "--message",
                "docs: parser contract",
            ],
            bash_needle: "--json",
            zsh_needle: "--json[",
        },
        AcceptedCase {
            spelling: "--dry-run",
            args: vec![
                "--expect-head",
                invalid_head,
                "--dry-run",
                "--message",
                "docs: parser contract",
            ],
            bash_needle: "--dry-run",
            zsh_needle: "--dry-run[",
        },
        AcceptedCase {
            spelling: "--automation",
            args: vec![
                "--expect-head",
                invalid_head,
                "--dry-run",
                "--automation",
                "--message",
                "docs: parser contract",
            ],
            bash_needle: "--automation",
            zsh_needle: "--automation[",
        },
        AcceptedCase {
            spelling: "--non-interactive",
            args: vec![
                "--expect-head",
                invalid_head,
                "--dry-run",
                "--non-interactive",
                "--message",
                "docs: parser contract",
            ],
            bash_needle: "--non-interactive",
            zsh_needle: "--non-interactive[",
        },
        AcceptedCase {
            spelling: "--type",
            args: vec![
                "--expect-head",
                invalid_head,
                "--dry-run",
                "--type",
                "docs",
                "--subject",
                "parser contract",
            ],
            bash_needle: "--type",
            zsh_needle: "--type=",
        },
        AcceptedCase {
            spelling: "--scope",
            args: vec![
                "--expect-head",
                invalid_head,
                "--dry-run",
                "--type",
                "docs",
                "--scope",
                "parser",
                "--subject",
                "contract",
            ],
            bash_needle: "--scope",
            zsh_needle: "--scope=",
        },
        AcceptedCase {
            spelling: "--subject",
            args: vec![
                "--expect-head",
                invalid_head,
                "--dry-run",
                "--type",
                "docs",
                "--subject",
                "parser contract",
            ],
            bash_needle: "--subject",
            zsh_needle: "--subject=",
        },
        AcceptedCase {
            spelling: "--body-bullet",
            args: vec![
                "--expect-head",
                invalid_head,
                "--dry-run",
                "--type",
                "docs",
                "--subject",
                "parser contract",
                "--body-bullet",
                "Cover the long form",
            ],
            bash_needle: "--body-bullet",
            zsh_needle: "--body-bullet=",
        },
        AcceptedCase {
            spelling: "--bullet",
            args: vec![
                "--expect-head",
                invalid_head,
                "--dry-run",
                "--type",
                "docs",
                "--subject",
                "parser contract",
                "--bullet",
                "Cover the alias",
            ],
            bash_needle: "--bullet",
            zsh_needle: "--bullet=",
        },
        AcceptedCase {
            spelling: "--signoff",
            args: vec![
                "--expect-head",
                invalid_head,
                "--dry-run",
                "--signoff",
                "--message",
                "docs: parser contract",
            ],
            bash_needle: "--signoff",
            zsh_needle: "--signoff[",
        },
        AcceptedCase {
            spelling: "--trailer",
            args: vec![
                "--expect-head",
                invalid_head,
                "--dry-run",
                "--trailer",
                "Refs: local",
                "--message",
                "docs: parser contract",
            ],
            bash_needle: "--trailer",
            zsh_needle: "--trailer=",
        },
        AcceptedCase {
            spelling: "--auto-fix",
            args: vec![
                "--expect-head",
                invalid_head,
                "--dry-run",
                "--auto-fix",
                "--message",
                "DOCS: Parser contract",
            ],
            bash_needle: "--auto-fix",
            zsh_needle: "--auto-fix[",
        },
        AcceptedCase {
            spelling: "--max-header-width",
            args: vec![
                "--expect-head",
                invalid_head,
                "--dry-run",
                "--max-header-width",
                "72",
                "--message",
                "docs: parser contract",
            ],
            bash_needle: "--max-header-width",
            zsh_needle: "--max-header-width=",
        },
    ];

    let mut generated = Vec::new();
    for shell in ["bash", "zsh"] {
        let output =
            common::run_semantic_commit_output(repo.path(), &["completion", shell], &[], None);
        assert_eq!(output.status.code(), Some(exit::SUCCESS), "{shell}");
        generated.push((shell, String::from_utf8_lossy(&output.stdout).into_owned()));
    }

    for case in &cases {
        let mut args = vec!["default-branch"];
        args.extend(case.args.iter().copied());
        let output = common::run_semantic_commit_output(repo.path(), &args, &[], None);
        assert_ne!(
            output.status.code(),
            Some(exit::USAGE),
            "{} was rejected by the public parser: {}",
            case.spelling,
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            generated[0].1.contains(case.bash_needle),
            "{} missing from generated bash completion",
            case.spelling
        );
        assert!(
            generated[1].1.contains(case.zsh_needle),
            "{} missing from generated zsh completion",
            case.spelling
        );
    }

    for (spelling, bash_needle, zsh_needle) in
        [("-h", " -h ", "'-h["), ("--help", "--help", "'--help[")]
    {
        let output = common::run_semantic_commit_output(
            repo.path(),
            &["default-branch", spelling],
            &[],
            None,
        );
        assert_eq!(
            output.status.code(),
            Some(exit::SUCCESS),
            "{spelling} was rejected by the public parser"
        );
        assert!(
            generated[0].1.contains(bash_needle),
            "{spelling} missing from generated bash completion"
        );
        assert!(
            generated[1].1.contains(zsh_needle),
            "{spelling} missing from generated zsh completion"
        );
    }

    for spelling in [
        "--message",
        "-m",
        "--message-file",
        "-F",
        "--expect-head",
        "--receipt-out",
        "--repo",
        "--format",
        "--type",
        "--scope",
        "--subject",
        "--body-bullet",
        "--bullet",
        "--trailer",
        "--max-header-width",
    ] {
        let args = if spelling == "--expect-head" {
            vec!["default-branch", spelling]
        } else {
            vec!["default-branch", "--expect-head", invalid_head, spelling]
        };
        let output = common::run_semantic_commit_output(repo.path(), &args, &[], None);
        assert_eq!(
            output.status.code(),
            Some(exit::USAGE),
            "{spelling} accepted without its required value"
        );
    }

    for rejected in [
        "--expected-branch",
        "--remote-mode",
        "--validate-only",
        "--message-out",
        "--no-progress",
        "--quiet",
        "--amend",
        "--allow-empty",
        "--message-only",
        "--no-edit",
        "--not-a-real-option",
    ] {
        let args = [
            "default-branch",
            "--expect-head",
            invalid_head,
            "--dry-run",
            "--message",
            "docs: parser contract",
            rejected,
        ];
        let output = common::run_semantic_commit_output(repo.path(), &args, &[], None);
        assert_eq!(
            output.status.code(),
            Some(exit::USAGE),
            "{rejected} unexpectedly passed the public parser"
        );
    }
}
