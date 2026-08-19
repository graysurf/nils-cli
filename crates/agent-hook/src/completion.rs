use std::io::{self, Write};

use clap::CommandFactory;
use clap_complete::{Shell, generate};

use crate::cli::{Cli, CompletionShell};

pub fn run(shell: CompletionShell) -> i32 {
    let mut command = Cli::command();
    let bin_name = command.get_name().to_string();
    match shell {
        CompletionShell::Bash => {
            let mut output = Vec::new();
            generate(Shell::Bash, &mut command, &bin_name, &mut output);
            let script = String::from_utf8(output).expect("bash completion should be UTF-8");
            io::stdout()
                .write_all(
                    public_completion(Shell::Bash, &script)
                        .replace("__subcmd__", "__")
                        .as_bytes(),
                )
                .expect("failed to write bash completion");
        }
        CompletionShell::Zsh => {
            let mut output = Vec::new();
            generate(Shell::Zsh, &mut command, &bin_name, &mut output);
            let script = String::from_utf8(output).expect("zsh completion should be UTF-8");
            io::stdout()
                .write_all(public_completion(Shell::Zsh, &script).as_bytes())
                .expect("failed to write zsh completion");
        }
    }
    0
}

fn public_completion(shell: Shell, script: &str) -> String {
    const INTERNAL_COMMANDS: [&str; 2] = ["quiesce", "release"];
    let mut filtered = String::with_capacity(script.len());
    let mut skip_until_case_end = None;
    let mut skip_until_function_end = false;
    let mut removed_sections = 0_u8;
    let mut removed_entries = 0_u8;

    for line in script.split_inclusive('\n') {
        let trimmed = line.trim();
        let indentation = line.bytes().take_while(|byte| *byte == b' ').count();
        if let Some(terminator_indentation) = skip_until_case_end {
            if trimmed == ";;" && indentation == terminator_indentation {
                skip_until_case_end = None;
            }
            continue;
        }
        if skip_until_function_end {
            if trimmed == "}" {
                skip_until_function_end = false;
            }
            continue;
        }

        match shell {
            Shell::Bash
                if INTERNAL_COMMANDS.iter().any(|command| {
                    trimmed
                        == format!("agent__hook__subcmd__finish__subcmd__line,{command})")
                    || trimmed
                        == format!(
                            "agent__subcmd__hook__subcmd__finish__subcmd__line__subcmd__{command})"
                        )
                }) =>
            {
                removed_sections = removed_sections.saturating_add(1);
                skip_until_case_end = Some(indentation + 4);
                continue;
            }
            Shell::Zsh
                if INTERNAL_COMMANDS
                    .iter()
                    .any(|command| trimmed == format!("({command})")) =>
            {
                removed_sections = removed_sections.saturating_add(1);
                skip_until_case_end = Some(indentation);
                continue;
            }
            Shell::Zsh
                if INTERNAL_COMMANDS.iter().any(|command| {
                    trimmed.starts_with(&format!(
                        "(( $+functions[_agent-hook__subcmd__finish-line__subcmd__{command}_commands] ))"
                    ))
                }) =>
            {
                removed_sections = removed_sections.saturating_add(1);
                skip_until_function_end = true;
                continue;
            }
            Shell::Zsh
                if INTERNAL_COMMANDS
                    .iter()
                    .any(|command| trimmed == format!("'{command}:' \\")) =>
            {
                removed_entries = removed_entries.saturating_add(1);
                continue;
            }
            _ => {}
        }

        if shell == Shell::Bash && line.contains("open begin run stop status quiesce release") {
            removed_entries = removed_entries.saturating_add(1);
            filtered.push_str(&line.replace(
                "open begin run stop status quiesce release",
                "open begin run stop status",
            ));
        } else {
            filtered.push_str(line);
        }
    }

    let expected_sections = match shell {
        Shell::Bash => 2,
        Shell::Zsh => 2,
        _ => unreachable!("agent-hook only exports bash and zsh completion"),
    };
    let expected_entries = match shell {
        Shell::Bash => 1,
        Shell::Zsh => INTERNAL_COMMANDS.len() as u8,
        _ => unreachable!("agent-hook only exports bash and zsh completion"),
    };
    let internal_lines = script
        .lines()
        .filter(|line| {
            INTERNAL_COMMANDS
                .iter()
                .any(|command| line.contains(command))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        (removed_sections, removed_entries),
        (
            expected_sections * INTERNAL_COMMANDS.len() as u8,
            expected_entries
        ),
        "clap completion shape changed around internal finish-line commands: {internal_lines:?}"
    );
    for command in INTERNAL_COMMANDS {
        assert!(
            !filtered.contains(command),
            "internal finish-line {command} survived completion filtering"
        );
    }
    filtered
}
