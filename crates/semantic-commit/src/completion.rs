use clap::{Arg, ArgAction, Command, ValueHint};
use clap_complete::{Shell, generate};
use std::io::{self, Write};

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        None => {
            eprintln!("usage: semantic-commit completion <bash|zsh>");
            1
        }
        Some("bash") if args.len() == 1 => generate_script(Shell::Bash),
        Some("zsh") if args.len() == 1 => generate_script(Shell::Zsh),
        Some(shell) if args.len() == 1 => {
            eprintln!("semantic-commit: error: unsupported completion shell '{shell}'");
            eprintln!("usage: semantic-commit completion <bash|zsh>");
            1
        }
        _ => {
            eprintln!("semantic-commit: error: expected `semantic-commit completion <bash|zsh>`");
            1
        }
    }
}

fn generate_script(generator: Shell) -> i32 {
    let mut command = build_completion_command();
    let bin_name = command.get_name().to_string();
    if matches!(generator, Shell::Bash) {
        let mut output = Vec::new();
        generate(generator, &mut command, bin_name.clone(), &mut output);
        let normalized = normalize_bash_completion(
            String::from_utf8(output).expect("bash completion should be valid UTF-8"),
        );
        io::stdout()
            .write_all(normalized.as_bytes())
            .expect("failed to write bash completion");
        return 0;
    }

    generate(generator, &mut command, bin_name, &mut io::stdout());
    0
}

fn normalize_bash_completion(script: String) -> String {
    script.replace("__subcmd__", "__")
}

fn build_completion_command() -> Command {
    Command::new("semantic-commit")
        .version(env!("CARGO_PKG_VERSION"))
        .long_version(nils_build_info::long_version(env!("CARGO_PKG_VERSION")))
        .about("Commit workflow helper with semantic commit validation")
        .disable_help_subcommand(true)
        .subcommand(
            Command::new("staged-context")
                .about("Print staged change context for commit message generation")
                .arg(
                    Arg::new("format")
                        .long("format")
                        .help("Output format for staged context")
                        .value_name("bundle|json|patch")
                        .value_parser(["bundle", "json", "patch"]),
                )
                .arg(
                    Arg::new("json")
                        .long("json")
                        .help("Alias for --format json")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("repo")
                        .long("repo")
                        .help("Repository path override")
                        .value_name("path")
                        .value_hint(ValueHint::DirPath),
                ),
        )
        .subcommand(
            Command::new("commit")
                .about("Commit staged changes with a prepared commit message")
                .arg(
                    Arg::new("message")
                        .short('m')
                        .long("message")
                        .help("Inline commit message")
                        .value_name("text"),
                )
                .arg(
                    Arg::new("message-file")
                        .short('F')
                        .long("message-file")
                        .help("Read commit message from file")
                        .value_name("path")
                        .value_hint(ValueHint::FilePath),
                )
                .arg(
                    Arg::new("message-out")
                        .long("message-out")
                        .help("Write final commit message to file")
                        .value_name("path")
                        .value_hint(ValueHint::FilePath),
                )
                .arg(
                    Arg::new("summary")
                        .long("summary")
                        .help("Summary provider")
                        .value_name("git-scope|git-show|none")
                        .value_parser(["git-scope", "git-show", "none"]),
                )
                .arg(
                    Arg::new("no-summary")
                        .long("no-summary")
                        .help("Disable summary section")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("format")
                        .long("format")
                        .help("Output format")
                        .value_name("text|json")
                        .value_parser(["text", "json"]),
                )
                .arg(
                    Arg::new("json")
                        .long("json")
                        .help("Alias for --format json")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("repo")
                        .long("repo")
                        .help("Repository path override")
                        .value_name("path")
                        .value_hint(ValueHint::DirPath),
                )
                .arg(
                    Arg::new("max-header-width")
                        .long("max-header-width")
                        .help("Override commit header width")
                        .value_name("N"),
                )
                .arg(
                    Arg::new("automation")
                        .long("automation")
                        .help("Disable interactive prompts and stdin input")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("non-interactive")
                        .long("non-interactive")
                        .help("Fail instead of prompting for input")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("validate-only")
                        .long("validate-only")
                        .help("Validate message and exit without committing")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("dry-run")
                        .long("dry-run")
                        .help("Validate message and staged changes without running git commit")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("auto-fix")
                        .long("auto-fix")
                        .help("Normalize body wrap, bullet/type/scope case before validation")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("amend")
                        .long("amend")
                        .help("Amend HEAD instead of creating a new commit")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("no-edit")
                        .long("no-edit")
                        .help("Reuse the HEAD message with --amend")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("message-only")
                        .long("message-only")
                        .help("Amend only the HEAD message and require no staged changes")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("allow-empty")
                        .long("allow-empty")
                        .help("Allow commit operation without staged changes")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("require-clean")
                        .long("require-clean")
                        .help("Require no unstaged or untracked changes")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("no-unstaged")
                        .long("no-unstaged")
                        .help("Alias for --require-clean")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("expect-head")
                        .long("expect-head")
                        .help("Require HEAD to match revision before committing")
                        .value_name("rev"),
                )
                .arg(
                    Arg::new("signoff")
                        .long("signoff")
                        .help("Pass --signoff to git commit")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("trailer")
                        .long("trailer")
                        .help("Add a git trailer")
                        .value_name("token: value")
                        .action(ArgAction::Append),
                )
                .arg(
                    Arg::new("type")
                        .long("type")
                        .help("Structured message type")
                        .value_name("type"),
                )
                .arg(
                    Arg::new("scope")
                        .long("scope")
                        .help("Structured message scope")
                        .value_name("scope"),
                )
                .arg(
                    Arg::new("subject")
                        .long("subject")
                        .help("Structured message subject")
                        .value_name("subject"),
                )
                .arg(
                    Arg::new("body-bullet")
                        .long("body-bullet")
                        .alias("bullet")
                        .help("Structured message body bullet")
                        .value_name("text")
                        .action(ArgAction::Append),
                )
                .arg(
                    Arg::new("no-progress")
                        .long("no-progress")
                        .help("Disable progress UI")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("quiet")
                        .long("quiet")
                        .help("Reduce non-error output")
                        .action(ArgAction::SetTrue),
                ),
        )
        .subcommand(local_default_completion_command())
        .subcommand(cleanup_completion_command(
            "fixup",
            "Create a fixup! commit for staged changes",
        ))
        .subcommand(cleanup_completion_command(
            "squash",
            "Create a squash! commit for staged changes",
        ))
        .subcommand(
            Command::new("completion")
                .about("Export shell completion script")
                .arg(
                    Arg::new("shell")
                        .value_name("shell")
                        .value_parser(["bash", "zsh"])
                        .required(true),
                ),
        )
        .subcommand(Command::new("help").about("Display help message"))
}

fn local_default_completion_command() -> Command {
    Command::new("local-default")
        .about("Create one governed signed commit on the primary local default branch")
        .arg(
            Arg::new("message")
                .short('m')
                .long("message")
                .value_name("text"),
        )
        .arg(
            Arg::new("message-file")
                .short('F')
                .long("message-file")
                .value_name("path")
                .value_hint(ValueHint::FilePath),
        )
        .arg(
            Arg::new("expected-branch")
                .long("expected-branch")
                .value_name("name")
                .required(true),
        )
        .arg(
            Arg::new("expect-head")
                .long("expect-head")
                .value_name("full-sha")
                .required(true),
        )
        .arg(
            Arg::new("receipt-out")
                .long("receipt-out")
                .value_name("path")
                .value_hint(ValueHint::FilePath)
                .required(true),
        )
        .arg(
            Arg::new("remote-mode")
                .long("remote-mode")
                .value_name("local-only")
                .value_parser(["local-only"]),
        )
        .arg(
            Arg::new("repo")
                .long("repo")
                .value_name("path")
                .value_hint(ValueHint::DirPath),
        )
        .arg(
            Arg::new("format")
                .long("format")
                .value_name("text|json")
                .value_parser(["text", "json"]),
        )
        .arg(Arg::new("json").long("json").action(ArgAction::SetTrue))
        .arg(
            Arg::new("dry-run")
                .long("dry-run")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("validate-only")
                .long("validate-only")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("automation")
                .long("automation")
                .action(ArgAction::SetTrue),
        )
        .arg(Arg::new("type").long("type").value_name("type"))
        .arg(Arg::new("scope").long("scope").value_name("scope"))
        .arg(Arg::new("subject").long("subject").value_name("subject"))
        .arg(
            Arg::new("body-bullet")
                .long("body-bullet")
                .alias("bullet")
                .value_name("text")
                .action(ArgAction::Append),
        )
        .arg(
            Arg::new("signoff")
                .long("signoff")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("trailer")
                .long("trailer")
                .value_name("token: value")
                .action(ArgAction::Append),
        )
        .arg(
            Arg::new("auto-fix")
                .long("auto-fix")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("no-progress")
                .long("no-progress")
                .action(ArgAction::SetTrue),
        )
        .arg(Arg::new("quiet").long("quiet").action(ArgAction::SetTrue))
}

fn cleanup_completion_command(name: &'static str, about: &'static str) -> Command {
    Command::new(name)
        .about(about)
        .arg(
            Arg::new("target")
                .long("target")
                .help("Target commit revision")
                .value_name("rev"),
        )
        .arg(
            Arg::new("summary")
                .long("summary")
                .help("Summary provider")
                .value_name("git-scope|git-show|none")
                .value_parser(["git-scope", "git-show", "none"]),
        )
        .arg(
            Arg::new("no-summary")
                .long("no-summary")
                .help("Disable summary section")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("format")
                .long("format")
                .help("Output format")
                .value_name("text|json")
                .value_parser(["text", "json"]),
        )
        .arg(
            Arg::new("json")
                .long("json")
                .help("Alias for --format json")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("dry-run")
                .long("dry-run")
                .help("Validate target and staged checks without committing")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("allow-empty")
                .long("allow-empty")
                .help("Allow cleanup commit without staged changes")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("require-clean")
                .long("require-clean")
                .help("Require no unstaged or untracked changes")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("no-unstaged")
                .long("no-unstaged")
                .help("Alias for --require-clean")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("expect-head")
                .long("expect-head")
                .help("Require HEAD to match revision before committing")
                .value_name("rev"),
        )
        .arg(
            Arg::new("repo")
                .long("repo")
                .help("Repository path override")
                .value_name("path")
                .value_hint(ValueHint::DirPath),
        )
        .arg(
            Arg::new("no-progress")
                .long("no-progress")
                .help("Disable progress UI")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("quiet")
                .long("quiet")
                .help("Reduce non-error output")
                .action(ArgAction::SetTrue),
        )
}
