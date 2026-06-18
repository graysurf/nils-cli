//! Shell completion generation (clap-first).
//!
//! Completion is generated directly from the clap command model and emitted
//! verbatim; the committed assets in `completions/{bash,zsh}` are the exact
//! output of `github-app-cli completion <shell>` and verified by
//! `scripts/ci/completion-freshness-audit.sh`.

use std::io::{self, Write};

use clap::CommandFactory;
use clap_complete::{Shell, generate};
use nils_common::cli_contract::exit;

/// Shells for which a completion script can be generated.
#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
#[value(rename_all = "lower")]
pub enum CompletionShell {
    Bash,
    Zsh,
}

/// Emit the completion script for `shell` to stdout.
pub fn run(shell: CompletionShell) -> i32 {
    let mut command = crate::cli::Cli::command();
    let bin_name = command.get_name().to_string();
    match shell {
        CompletionShell::Bash => {
            let mut buf = Vec::new();
            generate(Shell::Bash, &mut command, bin_name, &mut buf);
            let script = String::from_utf8(buf).expect("bash completion should be valid UTF-8");
            // `clap_complete` emits an internal `__subcmd__` separator in bash
            // case labels; the workspace completion-flag-parity audit expects the
            // normalized `<bin>__<subcommand>` form, so strip the marker.
            let normalized = normalize_bash_completion(script);
            io::stdout()
                .write_all(normalized.as_bytes())
                .expect("failed to write bash completion");
        }
        CompletionShell::Zsh => {
            generate(Shell::Zsh, &mut command, bin_name, &mut io::stdout());
        }
    }
    exit::SUCCESS
}

fn normalize_bash_completion(script: String) -> String {
    script.replace("__subcmd__", "__")
}
