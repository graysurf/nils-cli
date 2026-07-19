use clap::{Arg, ArgAction, Command, ValueHint};
use clap_complete::CompleteEnv;
use clap_complete::engine::{ArgValueCandidates, CompletionCandidate};
use clap_complete::env::{Bash, EnvCompleter, Zsh};
use std::io;

pub fn dispatch(shell_raw: &str, extra: &[String]) -> i32 {
    if !extra.is_empty() {
        eprintln!("git-cli: error: expected `git-cli completion <bash|zsh>`");
        return 1;
    }

    match shell_raw {
        "bash" => emit_registration(&Bash),
        "zsh" => emit_registration(&Zsh),
        other => {
            eprintln!("git-cli: error: unsupported completion shell '{other}'");
            eprintln!("usage: git-cli completion <bash|zsh>");
            1
        }
    }
}

/// Emit a `clap_complete` `CompleteEnv` dynamic-completion registration stub for
/// the given shell.
///
/// git-cli is a `completion_engine=dynamic` CLI (see the completion coverage
/// matrix): candidates such as live worktree names and branches are computed at
/// TAB time by the binary itself, so the exported script is a thin registration
/// that calls back into `git-cli` rather than a static `generate()` script. This
/// remains a single completion path per the completion development standard.
fn emit_registration<C: EnvCompleter>(completer: &C) -> i32 {
    match completer.write_registration(
        "COMPLETE",
        "git-cli",
        "git-cli",
        "git-cli",
        &mut io::stdout(),
    ) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("git-cli: error: failed to emit completion registration: {err}");
            1
        }
    }
}

/// Intercept `COMPLETE=<shell> git-cli ...` completion requests before the
/// hand-rolled dispatch.
///
/// On a completion request `CompleteEnv::complete()` prints the registration
/// stub (or the runtime candidates) and exits the process itself; when
/// `COMPLETE` is unset it returns and the normal application path proceeds
/// unchanged, so this is a no-op for ordinary invocations.
pub(crate) fn complete_env() {
    CompleteEnv::with_factory(build_command_model).complete();
}

/// Live completion candidates for `git-cli worktree go|remove <target>`:
/// managed worktree slugs and their branch names, sourced at TAB time from the
/// real `git worktree list`. Supersedes the static `gxwcd` workaround.
fn worktree_target_candidates() -> Vec<CompletionCandidate> {
    crate::worktree::go_target_candidates()
        .into_iter()
        .map(CompletionCandidate::new)
        .collect()
}

fn build_command_model() -> Command {
    Command::new("git-cli")
        .version(env!("CARGO_PKG_VERSION"))
        .long_version(nils_build_info::long_version(env!("CARGO_PKG_VERSION")))
        .about("Git helper CLI")
        .disable_help_subcommand(true)
        .subcommand(build_utils_group())
        .subcommand(build_reset_group())
        .subcommand(build_commit_group())
        .subcommand(build_branch_group())
        .subcommand(build_worktree_group())
        .subcommand(build_ci_group())
        .subcommand(build_open_group())
        .subcommand(Command::new("help").about("Display help message for git-cli"))
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
}

fn build_utils_group() -> Command {
    Command::new("utils")
        .about("Utility helpers")
        .subcommand(Command::new("zip").about("Create zip archive from HEAD"))
        .subcommand(
            Command::new("copy-staged")
                .visible_alias("copy")
                .about("Copy staged diff to clipboard")
                .arg(
                    Arg::new("stdout")
                        .long("stdout")
                        .help("Print staged diff to stdout")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("print")
                        .short('p')
                        .long("print")
                        .help("Alias for --stdout")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("both")
                        .long("both")
                        .help("Print diff and copy it to clipboard")
                        .action(ArgAction::SetTrue),
                ),
        )
        .subcommand(
            Command::new("root").about("Jump to git root").arg(
                Arg::new("shell")
                    .long("shell")
                    .help("Print shell command instead of plain output")
                    .action(ArgAction::SetTrue),
            ),
        )
        .subcommand(
            Command::new("commit-hash")
                .visible_alias("hash")
                .about("Resolve commit hash")
                .arg(Arg::new("ref").value_name("ref")),
        )
        .subcommand(Command::new("help").about("Display help message for utils"))
}

fn build_reset_group() -> Command {
    let count_arg = || Arg::new("count").value_name("count");

    Command::new("reset")
        .about("Reset helpers")
        .subcommand(
            Command::new("soft")
                .about("Reset to HEAD~N (soft)")
                .arg(count_arg()),
        )
        .subcommand(
            Command::new("mixed")
                .about("Reset to HEAD~N (mixed)")
                .arg(count_arg()),
        )
        .subcommand(
            Command::new("hard")
                .about("Reset to HEAD~N (hard)")
                .arg(count_arg()),
        )
        .subcommand(Command::new("undo").about("Undo last reset"))
        .subcommand(Command::new("back-head").about("Checkout HEAD@{1}"))
        .subcommand(Command::new("back-checkout").about("Return to previous branch"))
        .subcommand(
            Command::new("remote")
                .about("Reset to remote branch")
                .arg(
                    Arg::new("ref")
                        .long("ref")
                        .help("Remote ref in <remote>/<branch> form")
                        .value_name("ref"),
                )
                .arg(
                    Arg::new("remote")
                        .short('r')
                        .long("remote")
                        .help("Remote name")
                        .value_name("remote"),
                )
                .arg(
                    Arg::new("branch")
                        .short('b')
                        .long("branch")
                        .help("Remote branch name")
                        .value_name("branch"),
                )
                .arg(
                    Arg::new("no-fetch")
                        .long("no-fetch")
                        .help("Skip fetching remote refs")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("prune")
                        .long("prune")
                        .help("Run fetch with --prune")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("clean")
                        .long("clean")
                        .help("Run git clean -fd after reset")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("set-upstream")
                        .long("set-upstream")
                        .help("Set upstream to the target remote branch")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("yes")
                        .short('y')
                        .long("yes")
                        .help("Skip confirmation prompts")
                        .action(ArgAction::SetTrue),
                ),
        )
        .subcommand(Command::new("help").about("Display help message for reset"))
}

fn build_commit_group() -> Command {
    Command::new("commit")
        .about("Commit helpers")
        .subcommand(
            Command::new("context")
                .about("Print commit context")
                .arg(
                    Arg::new("stdout")
                        .long("stdout")
                        .help("Print report to stdout")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("both")
                        .long("both")
                        .help("Print report and write output file")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("no-color")
                        .long("no-color")
                        .help("Disable ANSI colors")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("include")
                        .long("include")
                        .help("Additional glob(s) to include")
                        .value_name("glob")
                        .num_args(1..),
                ),
        )
        .subcommand(
            Command::new("context-json")
                .visible_aliases(["context_json", "contextjson", "json"])
                .about("Print commit context as JSON")
                .arg(
                    Arg::new("stdout")
                        .long("stdout")
                        .help("Print JSON to stdout")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("both")
                        .long("both")
                        .help("Print JSON and write files")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("pretty")
                        .long("pretty")
                        .help("Pretty-print JSON output")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("bundle")
                        .long("bundle")
                        .help("Write bundle files to output directory")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("out-dir")
                        .long("out-dir")
                        .help("Output directory for generated files")
                        .value_name("path"),
                ),
        )
        .subcommand(
            Command::new("to-stash")
                .visible_alias("stash")
                .about("Create stash from commit")
                .arg(Arg::new("ref").value_name("ref")),
        )
        .subcommand(Command::new("help").about("Display help message for commit"))
}

fn build_branch_group() -> Command {
    Command::new("branch")
        .about("Branch helpers")
        .subcommand(
            Command::new("cleanup")
                .visible_alias("delete-merged")
                .about("Delete merged branches")
                .arg(
                    Arg::new("base")
                        .short('b')
                        .long("base")
                        .help("Base ref used to determine merged branches")
                        .value_name("base"),
                )
                .arg(
                    Arg::new("squash")
                        .short('s')
                        .long("squash")
                        .help("Include branches already applied via squash")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("remove-worktrees")
                        .short('w')
                        .long("remove-worktrees")
                        .help("Force-remove linked worktrees for candidate branches")
                        .action(ArgAction::SetTrue),
                ),
        )
        .subcommand(Command::new("help").about("Display help message for branch"))
}

fn build_worktree_group() -> Command {
    Command::new("worktree")
        .about("Worktree helpers")
        .subcommand(
            Command::new("add")
                .about("Create a managed agent worktree")
                .arg(Arg::new("slug").value_name("slug").required(true))
                .arg(
                    Arg::new("from")
                        .long("from")
                        .help("Base ref for the new branch")
                        .value_name("ref"),
                )
                .arg(kind_arg())
                .arg(format_arg()),
        )
        .subcommand(
            Command::new("dirty-snapshot")
                .about("Hash the current dirty checkout state")
                .arg(format_arg()),
        )
        .subcommand(
            Command::new("adopt-dirty")
                .about("Adopt one challenged dirty checkout snapshot")
                .arg(
                    Arg::new("challenge")
                        .long("challenge")
                        .value_name("token")
                        .num_args(1)
                        .required(true),
                )
                .arg(
                    Arg::new("reason-file")
                        .long("reason-file")
                        .value_name("path")
                        .value_hint(ValueHint::FilePath)
                        .num_args(1)
                        .required(true),
                )
                .arg(format_arg()),
        )
        .subcommand(
            Command::new("list")
                .about("List git worktrees")
                .arg(format_arg()),
        )
        .subcommand(
            Command::new("go")
                .about("Resolve a worktree path to cd into")
                .arg(
                    Arg::new("target")
                        .value_name("slug-or-branch-or-path")
                        .required(true)
                        .add(ArgValueCandidates::new(worktree_target_candidates)),
                )
                .arg(
                    Arg::new("shell")
                        .long("shell")
                        .help("Print an evaluable cd command instead of the bare path")
                        .action(ArgAction::SetTrue),
                )
                .arg(format_arg()),
        )
        .subcommand(
            Command::new("remove")
                .about("Remove a managed worktree by slug or path")
                .arg(
                    Arg::new("target")
                        .value_name("slug-or-path")
                        .required(true)
                        .add(ArgValueCandidates::new(worktree_target_candidates)),
                )
                .arg(format_arg()),
        )
        .subcommand(
            Command::new("prune")
                .about("Prune stale git worktree metadata")
                .arg(format_arg()),
        )
        .subcommand(
            Command::new("revoke-dirty")
                .about("Revoke a receipt-bound dirty adoption")
                .arg(
                    Arg::new("receipt")
                        .long("receipt")
                        .value_name("id")
                        .num_args(1)
                        .required(true),
                )
                .arg(format_arg()),
        )
        .subcommand(Command::new("help").about("Display help message for worktree"))
}

fn build_ci_group() -> Command {
    Command::new("ci")
        .about("CI helpers")
        .subcommand(
            Command::new("pick")
                .about("Cherry-pick into CI branch")
                .arg(
                    Arg::new("remote")
                        .short('r')
                        .long("remote")
                        .help("Remote used for fetch/push")
                        .value_name("name"),
                )
                .arg(
                    Arg::new("no-fetch")
                        .long("no-fetch")
                        .help("Skip remote fetch before branch creation")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("force")
                        .short('f')
                        .long("force")
                        .help("Reset existing CI branch and force push")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("stay")
                        .long("stay")
                        .help("Stay on CI branch after push")
                        .action(ArgAction::SetTrue),
                ),
        )
        .subcommand(Command::new("help").about("Display help message for ci"))
}

fn build_open_group() -> Command {
    Command::new("open")
        .about("Open remote pages")
        .subcommand(
            Command::new("repo")
                .about("Open repository page")
                .arg(remotes_arg()),
        )
        .subcommand(
            Command::new("branch")
                .about("Open branch tree page")
                .arg(Arg::new("ref").value_name("ref")),
        )
        .subcommand(
            Command::new("default-branch")
                .visible_alias("default")
                .about("Open default branch tree page")
                .arg(remotes_arg()),
        )
        .subcommand(
            Command::new("commit")
                .about("Open commit page")
                .arg(Arg::new("ref").value_name("ref")),
        )
        .subcommand(
            Command::new("compare")
                .about("Open compare page")
                .arg(Arg::new("from").value_name("from"))
                .arg(Arg::new("to").value_name("to")),
        )
        .subcommand(
            Command::new("pr")
                .visible_aliases(["pull-request", "mr", "merge-request"])
                .about("Open pull or merge request page")
                .arg(Arg::new("id").value_name("id")),
        )
        .subcommand(
            Command::new("pulls")
                .visible_aliases(["prs", "merge-requests", "mrs"])
                .about("Open pull or merge request list"),
        )
        .subcommand(
            Command::new("issues")
                .visible_alias("issue")
                .about("Open issues list/page")
                .arg(Arg::new("id").value_name("id")),
        )
        .subcommand(
            Command::new("actions")
                .visible_alias("action")
                .about("Open actions page")
                .arg(Arg::new("workflow").value_name("workflow")),
        )
        .subcommand(
            Command::new("releases")
                .visible_alias("release")
                .about("Open releases list/page")
                .arg(Arg::new("tag").value_name("tag")),
        )
        .subcommand(
            Command::new("tags")
                .visible_alias("tag")
                .about("Open tags list/page")
                .arg(Arg::new("tag").value_name("tag")),
        )
        .subcommand(
            Command::new("commits")
                .visible_alias("history")
                .about("Open commit history page")
                .arg(Arg::new("ref").value_name("ref")),
        )
        .subcommand(
            Command::new("file")
                .visible_alias("blob")
                .about("Open file page")
                .arg(
                    Arg::new("path")
                        .value_name("path")
                        .value_hint(ValueHint::FilePath),
                )
                .arg(Arg::new("ref").value_name("ref")),
        )
        .subcommand(
            Command::new("blame")
                .about("Open blame page")
                .arg(
                    Arg::new("path")
                        .value_name("path")
                        .value_hint(ValueHint::FilePath),
                )
                .arg(Arg::new("ref").value_name("ref")),
        )
        .subcommand(Command::new("help").about("Display help message for open"))
}

fn remotes_arg() -> Arg {
    Arg::new("remote").value_name("remote")
}

fn format_arg() -> Arg {
    Arg::new("format")
        .long("format")
        .help("Output format")
        .value_name("format")
        .num_args(1)
        .value_parser(["text", "json"])
}

fn kind_arg() -> Arg {
    Arg::new("kind")
        .long("kind")
        .help("Branch prefix kind")
        .value_name("kind")
        .value_parser(["feature", "bug", "chore", "docs", "ci", "refactor"])
}

#[cfg(test)]
mod tests {
    use super::build_command_model;
    use clap::{Arg, ArgAction, ValueHint};

    #[test]
    fn worktree_group_exposes_go_subcommand() {
        let cmd = build_command_model();
        let worktree = cmd
            .find_subcommand("worktree")
            .expect("worktree group present");
        let go = worktree
            .find_subcommand("go")
            .expect("worktree go subcommand present in completion model");
        assert!(
            go.get_arguments().any(|arg| arg.get_id() == "shell"),
            "worktree go should advertise --shell in completion"
        );
    }

    fn assert_single_value_argument(
        argument: &Arg,
        required: bool,
        value_hint: ValueHint,
        possible_values: &[&str],
    ) {
        assert_eq!(argument.is_required_set(), required);
        assert!(matches!(argument.get_action(), ArgAction::Set));
        assert_eq!(
            argument
                .get_num_args()
                .map(|range| (range.min_values(), range.max_values())),
            Some((1, 1)),
            "{} must consume exactly one value",
            argument.get_id()
        );
        assert_eq!(argument.get_value_hint(), value_hint);
        let actual_values: Vec<_> = argument
            .get_possible_values()
            .iter()
            .map(|value| value.get_name().to_string())
            .collect();
        let expected_values: Vec<_> = possible_values
            .iter()
            .map(|value| (*value).to_string())
            .collect();
        assert_eq!(actual_values, expected_values);
    }

    #[test]
    fn dirty_checkout_commands_expose_exact_completion_argument_contracts() {
        let cmd = build_command_model();
        let worktree = cmd
            .find_subcommand("worktree")
            .expect("worktree group present");
        for (command_name, expected_ids) in [
            ("dirty-snapshot", vec!["format"]),
            ("adopt-dirty", vec!["challenge", "reason-file", "format"]),
            ("revoke-dirty", vec!["receipt", "format"]),
        ] {
            let command = worktree
                .find_subcommand(command_name)
                .unwrap_or_else(|| panic!("worktree {command_name} command present"));
            let actual_ids: Vec<_> = command
                .get_arguments()
                .map(|argument| argument.get_id().as_str())
                .collect();
            assert_eq!(actual_ids, expected_ids, "{command_name} argument IDs");

            let format = command
                .get_arguments()
                .find(|argument| argument.get_id() == "format")
                .expect("format argument");
            assert_eq!(format.get_long(), Some("format"));
            assert_single_value_argument(format, false, ValueHint::Unknown, &["text", "json"]);
        }

        let adopt = worktree
            .find_subcommand("adopt-dirty")
            .expect("adopt-dirty command");
        let challenge = adopt
            .get_arguments()
            .find(|argument| argument.get_id() == "challenge")
            .expect("challenge argument");
        assert_eq!(challenge.get_long(), Some("challenge"));
        assert_single_value_argument(challenge, true, ValueHint::Unknown, &[]);
        let reason_file = adopt
            .get_arguments()
            .find(|argument| argument.get_id() == "reason-file")
            .expect("reason-file argument");
        assert_eq!(reason_file.get_long(), Some("reason-file"));
        assert_single_value_argument(reason_file, true, ValueHint::FilePath, &[]);

        let revoke = worktree
            .find_subcommand("revoke-dirty")
            .expect("revoke-dirty command");
        let receipt = revoke
            .get_arguments()
            .find(|argument| argument.get_id() == "receipt")
            .expect("receipt argument");
        assert_eq!(receipt.get_long(), Some("receipt"));
        assert_single_value_argument(receipt, true, ValueHint::Unknown, &[]);
    }

    #[test]
    fn worktree_add_kind_advertises_value_candidates() {
        let cmd = build_command_model();
        let worktree = cmd
            .find_subcommand("worktree")
            .expect("worktree group present");
        let add = worktree
            .find_subcommand("add")
            .expect("worktree add subcommand present");
        let kind = add
            .get_arguments()
            .find(|arg| arg.get_id() == "kind")
            .expect("worktree add --kind arg present in completion model");
        let values: Vec<String> = kind
            .get_possible_values()
            .iter()
            .map(|value| value.get_name().to_string())
            .collect();
        for expected in ["feature", "bug", "chore", "docs", "ci", "refactor"] {
            assert!(
                values.iter().any(|value| value == expected),
                "kind candidate `{expected}` should be present, got {values:?}"
            );
        }
    }
}
