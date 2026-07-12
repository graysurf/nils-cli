use clap::builder::PossibleValuesParser;
use clap::{Arg, ArgAction, Command};
use clap_complete::{Shell, generate};

use crate::ledger;
use std::io::{self, Write};

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        None => {
            eprintln!("usage: plan-tooling completion <bash|zsh>");
            1
        }
        Some("bash") if args.len() == 1 => generate_script(Shell::Bash),
        Some("zsh") if args.len() == 1 => generate_script(Shell::Zsh),
        Some(shell) if args.len() == 1 => {
            eprintln!("plan-tooling: error: unsupported completion shell '{shell}'");
            eprintln!("usage: plan-tooling completion <bash|zsh>");
            1
        }
        _ => {
            eprintln!("plan-tooling: error: expected `plan-tooling completion <bash|zsh>`");
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
    Command::new("plan-tooling")
        .version(env!("CARGO_PKG_VERSION"))
        .long_version(nils_build_info::long_version(env!("CARGO_PKG_VERSION")))
        .about("Plan tooling CLI")
        .disable_help_subcommand(true)
        .subcommand(
            Command::new("to-json")
                .about("Parse a plan markdown file into a stable JSON schema")
                .arg(
                    Arg::new("file")
                        .long("file")
                        .help("Plan markdown file path")
                        .value_name("path")
                        .required(false),
                )
                .arg(
                    Arg::new("sprint")
                        .long("sprint")
                        .help("Sprint number to parse")
                        .value_name("n")
                        .required(false),
                )
                .arg(
                    Arg::new("pretty")
                        .long("pretty")
                        .help("Pretty-print JSON output")
                        .action(ArgAction::SetTrue),
                ),
        )
        .subcommand(
            Command::new("validate")
                .about("Lint plan markdown files")
                .arg(
                    Arg::new("file")
                        .long("file")
                        .help("Plan markdown file path (repeatable)")
                        .value_name("path")
                        .action(ArgAction::Append),
                )
                .arg(
                    Arg::new("format")
                        .long("format")
                        .help("Validation output format")
                        .value_name("fmt")
                        .value_parser(["text", "json"]),
                )
                .arg(
                    Arg::new("explain")
                        .long("explain")
                        .help("Append canonical accepted-shape examples per error class")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("no-group")
                        .long("no-group")
                        .help("Disable class-grouped text output")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("fix")
                        .long("fix")
                        .help("Rewrite mechanical violations in-place across the plan bundle")
                        .action(ArgAction::SetTrue),
                ),
        )
        .subcommand(
            Command::new("batches")
                .about("Compute dependency layers (parallel batches) for a sprint")
                .arg(
                    Arg::new("file")
                        .long("file")
                        .help("Plan markdown file path")
                        .value_name("path")
                        .required(false),
                )
                .arg(
                    Arg::new("sprint")
                        .long("sprint")
                        .help("Sprint number to analyze")
                        .value_name("n")
                        .required(false),
                )
                .arg(
                    Arg::new("format")
                        .long("format")
                        .help("Batch output format")
                        .value_name("fmt")
                        .value_parser(["json", "text"]),
                ),
        )
        .subcommand(
            Command::new("artifact-audit")
                .about("Classify durable coordination artifacts without side effects")
                .arg(
                    Arg::new("candidate")
                        .long("candidate")
                        .help("Candidate artifact path (repeatable)")
                        .value_name("path")
                        .action(ArgAction::Append),
                )
                .arg(
                    Arg::new("candidate-file")
                        .long("candidate-file")
                        .help("File containing candidate paths, one per line")
                        .value_name("path"),
                )
                .arg(
                    Arg::new("repo")
                        .long("repo")
                        .help("Repository root for reference scans")
                        .value_name("path"),
                )
                .arg(
                    Arg::new("format")
                        .long("format")
                        .help("Audit output format")
                        .value_name("fmt")
                        .value_parser(["text", "json"]),
                )
                .arg(
                    Arg::new("explain")
                        .long("explain")
                        .help("Include evidence details in text output")
                        .action(ArgAction::SetTrue),
                ),
        )
        .subcommand(
            Command::new("split-prs")
                .about("Build task-to-PR split records (deterministic/auto)")
                .arg(
                    Arg::new("file")
                        .long("file")
                        .help("Plan markdown file path")
                        .value_name("path")
                        .required(false),
                )
                .arg(
                    Arg::new("scope")
                        .long("scope")
                        .help("Split scope")
                        .value_name("scope")
                        .value_parser(["plan", "sprint"]),
                )
                .arg(
                    Arg::new("sprint")
                        .long("sprint")
                        .help("Sprint number when --scope sprint")
                        .value_name("n")
                        .required(false),
                )
                .arg(
                    Arg::new("pr-grouping")
                        .long("pr-grouping")
                        .help("PR grouping mode (deterministic only)")
                        .value_name("mode")
                        .value_parser(["per-sprint", "group"]),
                )
                .arg(
                    Arg::new("default-pr-grouping")
                        .long("default-pr-grouping")
                        .help("Auto fallback when sprint metadata omits grouping intent")
                        .value_name("mode")
                        .value_parser(["per-sprint", "group"]),
                )
                .arg(
                    Arg::new("pr-group")
                        .long("pr-group")
                        .help(
                            "Group pin: <task-or-plan-id>=<group> (required in deterministic/group, optional in auto/group)",
                        )
                        .value_name("mapping")
                        .action(ArgAction::Append),
                )
                .arg(
                    Arg::new("strategy")
                        .long("strategy")
                        .help("Split strategy (deterministic requires full group mapping)")
                        .value_name("strategy")
                        .value_parser(["deterministic", "auto"]),
                )
                .arg(
                    Arg::new("owner-prefix")
                        .long("owner-prefix")
                        .help("Owner prefix for generated task rows")
                        .value_name("text"),
                )
                .arg(
                    Arg::new("branch-prefix")
                        .long("branch-prefix")
                        .help("Branch prefix for generated task rows")
                        .value_name("text"),
                )
                .arg(
                    Arg::new("worktree-prefix")
                        .long("worktree-prefix")
                        .help("Worktree prefix for generated task rows")
                        .value_name("text"),
                )
                .arg(
                    Arg::new("format")
                        .long("format")
                        .help("Split output format")
                        .value_name("fmt")
                        .value_parser(["json", "tsv"]),
                ),
        )
        .subcommand(
            Command::new("ledger-update")
                .about("Patch one row in an execution-state `## Task Ledger` table")
                .arg(
                    Arg::new("execution-state")
                        .long("execution-state")
                        .help("Path to the `<slug>-execution-state.md` file")
                        .value_name("path")
                        .required(true),
                )
                .arg(
                    Arg::new("task")
                        .long("task")
                        .help("Ledger row ID to patch (exact match)")
                        .value_name("id")
                        .required(true),
                )
                .arg(
                    Arg::new("status")
                        .long("status")
                        .help("New row status")
                        .value_name("status")
                        .value_parser(PossibleValuesParser::new(
                            ledger::STATUS_VALUES.iter().copied(),
                        ))
                        .required(true),
                )
                .arg(
                    Arg::new("evidence")
                        .long("evidence")
                        .help("Evidence text to merge into the Evidence cell")
                        .value_name("text")
                        .required(true),
                )
                .arg(
                    Arg::new("notes")
                        .long("notes")
                        .help("Optional notes value; omit to leave Notes untouched")
                        .value_name("text"),
                )
                .arg(
                    Arg::new("dry-run")
                        .long("dry-run")
                        .help("Compute the patch but do not write the file")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("format")
                        .long("format")
                        .help("Output format")
                        .value_name("fmt")
                        .value_parser(["text", "json"]),
                ),
        )
        .subcommand(
            Command::new("ledger-sync")
                .about("Reconcile a ledger Evidence column against tracking-issue evidence")
                .arg(
                    Arg::new("bundle")
                        .long("bundle")
                        .help("Plan bundle directory containing `*-execution-state.md`")
                        .value_name("dir")
                        .required(true),
                )
                .arg(
                    Arg::new("body-file")
                        .long("body-file")
                        .help("Issue body markdown file")
                        .value_name("path"),
                )
                .arg(
                    Arg::new("comments-json")
                        .long("comments-json")
                        .help("Issue comments JSON file (`gh issue view --json comments`)")
                        .value_name("path"),
                )
                .arg(
                    Arg::new("fixture")
                        .long("fixture")
                        .help("Fixture directory containing `body.md` + `comments.json`")
                        .value_name("dir"),
                )
                .arg(
                    Arg::new("write")
                        .long("write")
                        .help("Patch the ledger file (empty-cell preference rule)")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("format")
                        .long("format")
                        .help("Output format")
                        .value_name("fmt")
                        .value_parser(["text", "json"]),
                ),
        )
        .subcommand(
            Command::new("exec-state-sync")
                .about("Repair an execution-state file's tracking issue URL / terminal state")
                .arg(
                    Arg::new("bundle")
                        .long("bundle")
                        .help("Plan bundle directory containing `*-execution-state.md`")
                        .value_name("dir"),
                )
                .arg(
                    Arg::new("execution-state")
                        .long("execution-state")
                        .help("Explicit execution-state file (overrides --bundle lookup)")
                        .value_name("path"),
                )
                .arg(
                    Arg::new("issue-url")
                        .long("issue-url")
                        .help("Tracking issue URL to record (autolinked)")
                        .value_name("url"),
                )
                .arg(
                    Arg::new("status")
                        .long("status")
                        .help("Terminal `Status` value")
                        .value_name("text"),
                )
                .arg(
                    Arg::new("last-updated")
                        .long("last-updated")
                        .help("`Last updated` stamp")
                        .value_name("date"),
                )
                .arg(
                    Arg::new("branch-commit-pr")
                        .long("branch-commit-pr")
                        .help("`Branch/commit/PR` value")
                        .value_name("text"),
                )
                .arg(
                    Arg::new("current-task")
                        .long("current-task")
                        .help("`Current task` value")
                        .value_name("text"),
                )
                .arg(
                    Arg::new("next-task")
                        .long("next-task")
                        .help("`Next task` value")
                        .value_name("text"),
                )
                .arg(
                    Arg::new("handoff")
                        .long("handoff")
                        .help("Replacement body for the `## Handoff` section")
                        .value_name("markdown"),
                )
                .arg(
                    Arg::new("dry-run")
                        .long("dry-run")
                        .help("Report the change set without writing the file")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("format")
                        .long("format")
                        .help("Output format")
                        .value_name("fmt")
                        .value_parser(["text", "json"]),
                ),
        )
        .subcommand(
            Command::new("scaffold")
                .about("Create a new plan from template")
                .arg(
                    Arg::new("slug")
                        .long("slug")
                        .help("Plan slug used for output naming")
                        .value_name("slug"),
                )
                .arg(
                    Arg::new("file")
                        .long("file")
                        .help("Explicit output plan path")
                        .value_name("path"),
                )
                .arg(
                    Arg::new("title")
                        .long("title")
                        .help("Plan title override")
                        .value_name("text"),
                )
                .arg(
                    Arg::new("force")
                        .long("force")
                        .help("Overwrite existing output file")
                        .action(ArgAction::SetTrue),
                ),
        )
        .subcommand(
            Command::new("spec")
                .about("Dump the validate catalog (class, pattern, rule, example)")
                .arg(
                    Arg::new("format")
                        .long("format")
                        .help("Spec output format")
                        .value_name("fmt")
                        .value_parser(["text", "json"]),
                ),
        )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_state_sync_completion_exposes_terminal_fields() {
        let command = build_completion_command();
        let sync = command
            .get_subcommands()
            .find(|subcommand| subcommand.get_name() == "exec-state-sync")
            .expect("exec-state-sync completion model");
        let long_flags = sync
            .get_arguments()
            .filter_map(|arg| arg.get_long())
            .collect::<Vec<_>>();
        assert!(long_flags.contains(&"current-task"));
        assert!(long_flags.contains(&"next-task"));
        assert!(long_flags.contains(&"handoff"));
    }
}
