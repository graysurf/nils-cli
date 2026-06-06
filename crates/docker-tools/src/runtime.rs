use nils_common::cli_contract::exit;
use nils_common::process::cmd_exists;
use std::io::{self, IsTerminal, Write};
use std::process::{Command, Stdio};

use crate::cli::{ComposeDownArgs, ContainerRmArgs, ContainerShellArgs, RunZshArgs};

const LEGACY_USAGE: i32 = 2;
const NOT_FOUND: i32 = 127;
const SHELL_FALLBACK: &str = "if command -v zsh >/dev/null 2>&1; then exec zsh; elif command -v bash >/dev/null 2>&1; then exec bash; else exec sh; fi";

pub fn container_shell(args: ContainerShellArgs) -> i32 {
    if !cmd_exists("docker") {
        eprintln!("docker-container-sh: docker is not installed");
        return NOT_FOUND;
    }

    let user = user_arg(args.user, args.root);
    let mut exec_args = vec!["exec".to_string(), "-it".to_string()];
    if let Some(user) = &user {
        exec_args.extend(["-u".to_string(), user.clone()]);
    }
    exec_args.extend([
        "--".to_string(),
        args.container.clone(),
        "sh".to_string(),
        "-c".to_string(),
        SHELL_FALLBACK.to_string(),
    ]);

    let status = run_status("docker", &exec_args);
    if status == exit::SUCCESS {
        return exit::SUCCESS;
    }

    for shell in ["zsh", "bash", "sh"] {
        let mut direct_args = vec!["exec".to_string(), "-it".to_string()];
        if let Some(user) = &user {
            direct_args.extend(["-u".to_string(), user.clone()]);
        }
        direct_args.extend(["--".to_string(), args.container.clone(), shell.to_string()]);
        if run_status("docker", &direct_args) == exit::SUCCESS {
            return exit::SUCCESS;
        }
    }

    exit::RUNTIME
}

pub fn container_rm(args: ContainerRmArgs) -> i32 {
    if !cmd_exists("docker") {
        eprintln!("docker-container-rm: docker is not installed");
        return NOT_FOUND;
    }

    let mut docker_args = vec!["container".to_string(), "rm".to_string()];
    if !args.no_force {
        docker_args.push("-f".to_string());
    }
    if args.volumes {
        docker_args.push("-v".to_string());
    }
    docker_args.push("--".to_string());
    docker_args.extend(args.containers);

    run_status("docker", &docker_args)
}

pub fn compose_down(args: ComposeDownArgs) -> i32 {
    let mut extra = Vec::new();
    if args.all {
        extra.extend([
            "--remove-orphans".to_string(),
            "--volumes".to_string(),
            "--rmi".to_string(),
            "all".to_string(),
        ]);
        if !args.yes && !confirm_compose_all(&extra, &args.args) {
            return if io::stdin().is_terminal() {
                exit::RUNTIME
            } else {
                LEGACY_USAGE
            };
        }
    }

    let Some(compose) = resolve_compose_command() else {
        return NOT_FOUND;
    };

    let mut compose_args = compose.args;
    compose_args.push("down".to_string());
    compose_args.extend(extra);
    compose_args.extend(args.args);

    run_status(&compose.program, &compose_args)
}

pub fn run_zsh(args: RunZshArgs) -> i32 {
    if !cmd_exists("docker") {
        eprintln!("docker-run-zsh: docker is not installed");
        return NOT_FOUND;
    }

    let mount = !args.no_mount;
    let workdir = args.workdir.or_else(|| mount.then(|| "/work".to_string()));

    let mut docker_args = vec!["run".to_string(), "--rm".to_string(), "-it".to_string()];
    if let Some(name) = args.name {
        docker_args.extend(["--name".to_string(), name]);
    }
    if let Some(user) = user_arg(args.user, args.root) {
        docker_args.extend(["-u".to_string(), user]);
    }
    if mount {
        let cwd = std::env::current_dir()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());
        docker_args.extend(["-v".to_string(), format!("{cwd}:/work")]);
    }
    if let Some(workdir) = workdir {
        docker_args.extend(["-w".to_string(), workdir]);
    }
    docker_args.extend([
        "--".to_string(),
        args.image,
        "sh".to_string(),
        "-c".to_string(),
        SHELL_FALLBACK.to_string(),
    ]);

    run_status("docker", &docker_args)
}

fn user_arg(user: Option<String>, root: bool) -> Option<String> {
    if root { Some("root".to_string()) } else { user }
}

fn confirm_compose_all(extra: &[String], passthrough: &[String]) -> bool {
    if !io::stdin().is_terminal() {
        eprintln!("docker-compose-down: --all requires --yes in non-interactive shells");
        return false;
    }

    println!(
        "About to run: docker compose down {} {}",
        extra.join(" "),
        passthrough.join(" ")
    );
    print!("Proceed? [y/N] ");
    let _ = io::stdout().flush();

    let mut reply = String::new();
    if io::stdin().read_line(&mut reply).is_err() {
        return false;
    }

    matches!(reply.trim(), "y" | "Y" | "yes" | "Yes" | "YES")
}

struct ComposeCommand {
    program: String,
    args: Vec<String>,
}

fn resolve_compose_command() -> Option<ComposeCommand> {
    if let Ok(override_cmd) = std::env::var("ZSH_DOCKER_COMPOSE_CMD")
        && !override_cmd.trim().is_empty()
    {
        return parse_compose_override(&override_cmd);
    }

    if cmd_exists("docker") {
        if command_succeeds("docker", &["compose", "version"]) {
            return Some(ComposeCommand {
                program: "docker".to_string(),
                args: vec!["compose".to_string()],
            });
        }

        if !cmd_exists("docker-compose") {
            return Some(ComposeCommand {
                program: "docker".to_string(),
                args: vec!["compose".to_string()],
            });
        }
    }

    if cmd_exists("docker-compose") {
        return Some(ComposeCommand {
            program: "docker-compose".to_string(),
            args: Vec::new(),
        });
    }

    eprintln!("docker-tools: docker is not installed");
    None
}

fn parse_compose_override(raw: &str) -> Option<ComposeCommand> {
    let words = match shell_words::split(raw) {
        Ok(words) if !words.is_empty() => words,
        Ok(_) => return None,
        Err(err) => {
            eprintln!("docker-tools: invalid ZSH_DOCKER_COMPOSE_CMD: {err}");
            return None;
        }
    };

    let mut iter = words.into_iter();
    let program = iter.next()?;
    Some(ComposeCommand {
        program,
        args: iter.collect(),
    })
}

fn command_succeeds(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn run_status(program: &str, args: &[String]) -> i32 {
    match Command::new(program)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
    {
        Ok(status) => status.code().unwrap_or(exit::RUNTIME),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            eprintln!("docker-tools: {program} is not installed");
            NOT_FOUND
        }
        Err(err) => {
            eprintln!("docker-tools: failed to run {program}: {err}");
            exit::RUNTIME
        }
    }
}
