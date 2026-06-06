use std::env;
use std::io::Write;
use std::process::Command;

use nils_common::process;

pub fn exec_prompt(prompt: &str, title: &str, stderr: &mut impl Write) -> i32 {
    if !process::cmd_exists("opencode") {
        let _ = writeln!(stderr, "opencode-tools: missing binary: opencode");
        return 1;
    }

    let mut command = Command::new("opencode");
    command.arg("run");

    if let Some(model) = non_empty_env("OPENCODE_CLI_MODEL") {
        command.arg("-m").arg(model);
    }

    if let Some(variant) = non_empty_env("OPENCODE_CLI_VARIANT") {
        command.arg("--variant").arg(variant);
    }

    if !title.trim().is_empty() {
        command.arg("--title").arg(title);
    }

    let status = command.arg("--").arg(prompt).status();
    match status {
        Ok(status) if status.success() => 0,
        Ok(status) => status.code().unwrap_or(1),
        Err(err) => {
            let _ = writeln!(stderr, "opencode-tools: failed to run opencode: {err}");
            1
        }
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
