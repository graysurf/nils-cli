use clap::{Arg, ArgAction, Command, ValueHint};
use clap_complete::{Shell, generate};
use std::io::{self, Write};

use crate::default_branch::clap_command as default_branch_completion_command;

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
        .subcommand(default_branch_completion_command())
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

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use clap::ArgAction;
    use pretty_assertions::assert_eq;

    use crate::default_branch::{OptionArity, OptionKind, option_contract};

    use super::default_branch_completion_command;

    #[test]
    fn default_branch_completion_is_derived_from_the_public_parser_contract() {
        let command = default_branch_completion_command();
        let long_options = command
            .get_arguments()
            .filter_map(|argument| argument.get_long())
            .collect::<BTreeSet<_>>();
        let expected_long_options = option_contract()
            .iter()
            .map(|option| option.long)
            .collect::<BTreeSet<_>>();
        assert_eq!(long_options, expected_long_options);

        let short_options = command
            .get_arguments()
            .filter_map(|argument| argument.get_short())
            .collect::<BTreeSet<_>>();
        let expected_short_options = option_contract()
            .iter()
            .filter_map(|option| option.short)
            .collect::<BTreeSet<_>>();
        assert_eq!(short_options, expected_short_options);

        let visible_aliases = command
            .get_arguments()
            .filter_map(|argument| {
                argument
                    .get_visible_aliases()
                    .map(|aliases| (argument.get_id().as_str(), aliases))
            })
            .collect::<BTreeMap<_, _>>();
        let expected_visible_aliases = option_contract()
            .iter()
            .filter(|option| !option.visible_aliases.is_empty())
            .map(|option| (option.long, option.visible_aliases.to_vec()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(visible_aliases, expected_visible_aliases);

        for option in option_contract() {
            let argument = command
                .get_arguments()
                .find(|argument| argument.get_long() == Some(option.long))
                .expect("contract option should be present");
            let action_matches = match (option.kind, option.arity, option.repeatable) {
                (OptionKind::Help, _, _) => {
                    matches!(argument.get_action(), ArgAction::HelpLong)
                }
                (_, OptionArity::Flag, _) => {
                    matches!(argument.get_action(), ArgAction::SetTrue)
                }
                (_, OptionArity::Value, true) => {
                    matches!(argument.get_action(), ArgAction::Append)
                }
                (_, OptionArity::Value, false) => {
                    matches!(argument.get_action(), ArgAction::Set)
                }
            };
            assert!(
                action_matches,
                "--{} value arity has action {:?}",
                option.long,
                argument.get_action()
            );
            assert_eq!(
                argument.is_required_set(),
                option.required,
                "--{} required state",
                option.long
            );
        }
    }
}
