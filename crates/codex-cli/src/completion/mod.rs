use clap::CommandFactory;
use clap_complete::{Shell, generate};
use std::io::{self, Write};

pub fn run(shell: crate::cli::CompletionShell) -> i32 {
    match shell {
        crate::cli::CompletionShell::Bash => generate_script(Shell::Bash),
        crate::cli::CompletionShell::Zsh => generate_script(Shell::Zsh),
    }
}

fn generate_script(generator: Shell) -> i32 {
    let mut command = crate::cli::Cli::command();
    let bin_name = command.get_name().to_string();
    if matches!(generator, Shell::Bash) {
        let mut output = Vec::new();
        generate(generator, &mut command, bin_name.clone(), &mut output);
        let normalized = hide_auth_remote_export_bash(normalize_bash_completion(
            String::from_utf8(output).expect("bash completion should be valid UTF-8"),
        ));
        io::stdout()
            .write_all(normalized.as_bytes())
            .expect("failed to write bash completion");
        return 0;
    }

    let mut output = Vec::new();
    generate(generator, &mut command, bin_name, &mut output);
    let normalized = hide_auth_remote_export_zsh(
        String::from_utf8(output).expect("zsh completion should be valid UTF-8"),
    );
    io::stdout()
        .write_all(normalized.as_bytes())
        .expect("failed to write zsh completion");
    0
}

fn normalize_bash_completion(script: String) -> String {
    script.replace("__subcmd__", "__")
}

fn hide_auth_remote_export_bash(script: String) -> String {
    script
        .replace(
            "opts=\"-h --help pull export help\"",
            "opts=\"-h --help pull help\"",
        )
        .replace("opts=\"pull export help\"", "opts=\"pull help\"")
        .replace("opts=\"pull export\"", "opts=\"pull\"")
}

fn hide_auth_remote_export_zsh(script: String) -> String {
    let mut output = String::new();
    let mut skipping_function = false;

    for line in script.lines() {
        if !skipping_function
            && (line
                .contains("_codex-cli__subcmd__auth__subcmd__remote__subcmd__export_commands")
                || line.contains(
                    "_codex-cli__subcmd__auth__subcmd__help__subcmd__remote__subcmd__export_commands",
                )
                || line.contains(
                    "_codex-cli__subcmd__auth__subcmd__remote__subcmd__help__subcmd__export_commands",
                )
                || line.contains(
                    "_codex-cli__subcmd__help__subcmd__auth__subcmd__remote__subcmd__export_commands",
                ))
        {
            skipping_function = true;
            continue;
        }

        if skipping_function {
            if line.trim() == "}" {
                skipping_function = false;
            }
            continue;
        }

        if line.contains("'export:Export remote auth payload for SSH transport'") {
            continue;
        }

        output.push_str(line);
        output.push('\n');
    }

    output
}
