use std::io::{self, BufRead, Write};

use crate::runtime::{AgentCommandProfile, AgentRuntimeMode, AgentRuntimeOptions, exec_isolated};

pub mod commit;
pub mod exec;
pub mod resume;

pub fn prompt(prompt_args: &[String]) -> i32 {
    prompt_with_options(prompt_args, exec::ExecOptions::default())
}

pub fn prompt_with_options(prompt_args: &[String], exec_options: exec::ExecOptions) -> i32 {
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let stderr = io::stderr();
    let mut stderr = stderr.lock();
    prompt_with_io(
        prompt_args,
        exec_options,
        &mut stdin,
        &mut stdout,
        &mut stderr,
    )
}

pub fn prompt_with_runtime_options(
    prompt_args: &[String],
    runtime_options: AgentRuntimeOptions,
) -> i32 {
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let stderr = io::stderr();
    let mut stderr = stderr.lock();
    prompt_with_runtime_io(
        prompt_args,
        runtime_options,
        &mut stdin,
        &mut stdout,
        &mut stderr,
    )
}

fn prompt_with_runtime_io<R: BufRead, WOut: Write, WErr: Write>(
    prompt_args: &[String],
    runtime_options: AgentRuntimeOptions,
    stdin: &mut R,
    stdout: &mut WOut,
    stderr: &mut WErr,
) -> i32 {
    let Some(prompt) = collect_input("Prompt: ", "prompt", prompt_args, stdin, stdout, stderr)
    else {
        return 1;
    };
    match resolve_runtime(runtime_options, stderr) {
        Some(AgentRuntimeMode::Isolated) => {
            exec_isolated(&prompt, AgentCommandProfile::Prompt, stderr)
        }
        Some(AgentRuntimeMode::Inherited) => exec::exec_dangerous_with_options(
            &prompt,
            "codex-tools:prompt",
            stderr,
            exec::ExecOptions {
                ephemeral: runtime_options.ephemeral,
            },
        ),
        None => 64,
    }
}

pub fn prompt_with_io<R: BufRead, WOut: Write, WErr: Write>(
    prompt_args: &[String],
    exec_options: exec::ExecOptions,
    stdin: &mut R,
    stdout: &mut WOut,
    stderr: &mut WErr,
) -> i32 {
    let mut user_prompt = prompt_args.join(" ");

    if user_prompt.is_empty() {
        if write!(stdout, "Prompt: ").is_err() {
            return 1;
        }
        let _ = stdout.flush();

        user_prompt.clear();
        if stdin
            .read_line(&mut user_prompt)
            .ok()
            .filter(|n| *n > 0)
            .is_none()
        {
            return 1;
        }
        user_prompt = user_prompt.trim_end_matches(&['\n', '\r'][..]).to_string();
    }

    if user_prompt.is_empty() {
        let _ = writeln!(stderr, "codex-tools: missing prompt");
        return 1;
    }

    exec::exec_dangerous_with_options(&user_prompt, "codex-tools:prompt", stderr, exec_options)
}

pub fn advice(question_args: &[String]) -> i32 {
    advice_with_options(question_args, exec::ExecOptions::default())
}

pub fn advice_with_options(question_args: &[String], exec_options: exec::ExecOptions) -> i32 {
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let stderr = io::stderr();
    let mut stderr = stderr.lock();
    run_template_with_io(
        "actionable-advice",
        question_args,
        exec_options,
        &mut stdin,
        &mut stdout,
        &mut stderr,
    )
}

pub fn advice_with_runtime_options(
    question_args: &[String],
    runtime_options: AgentRuntimeOptions,
) -> i32 {
    run_template_with_runtime_options(
        "actionable-advice",
        question_args,
        AgentCommandProfile::Advice,
        runtime_options,
    )
}

pub fn knowledge(concept_args: &[String]) -> i32 {
    knowledge_with_options(concept_args, exec::ExecOptions::default())
}

pub fn knowledge_with_options(concept_args: &[String], exec_options: exec::ExecOptions) -> i32 {
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let stderr = io::stderr();
    let mut stderr = stderr.lock();
    run_template_with_io(
        "actionable-knowledge",
        concept_args,
        exec_options,
        &mut stdin,
        &mut stdout,
        &mut stderr,
    )
}

pub fn knowledge_with_runtime_options(
    concept_args: &[String],
    runtime_options: AgentRuntimeOptions,
) -> i32 {
    run_template_with_runtime_options(
        "actionable-knowledge",
        concept_args,
        AgentCommandProfile::Knowledge,
        runtime_options,
    )
}

fn run_template_with_runtime_options(
    template_name: &str,
    args: &[String],
    profile: AgentCommandProfile,
    runtime_options: AgentRuntimeOptions,
) -> i32 {
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let stderr = io::stderr();
    let mut stderr = stderr.lock();
    let Some(query) = collect_input(
        "Question: ",
        "question",
        args,
        &mut stdin,
        &mut stdout,
        &mut stderr,
    ) else {
        return 1;
    };
    let Some(final_prompt) = render_template(template_name, &query, &mut stderr) else {
        return 1;
    };
    match resolve_runtime(runtime_options, &mut stderr) {
        Some(AgentRuntimeMode::Isolated) => exec_isolated(&final_prompt, profile, &mut stderr),
        Some(AgentRuntimeMode::Inherited) => exec::exec_dangerous_with_options(
            &final_prompt,
            &format!("codex-tools:{template_name}"),
            &mut stderr,
            exec::ExecOptions {
                ephemeral: runtime_options.ephemeral,
            },
        ),
        None => 64,
    }
}

fn run_template_with_io<R: BufRead, WOut: Write, WErr: Write>(
    template_name: &str,
    args: &[String],
    exec_options: exec::ExecOptions,
    stdin: &mut R,
    stdout: &mut WOut,
    stderr: &mut WErr,
) -> i32 {
    let mut user_query = args.join(" ");
    if user_query.trim().is_empty() {
        if write!(stdout, "Question: ").is_err() {
            return 1;
        }
        let _ = stdout.flush();

        user_query.clear();
        if stdin
            .read_line(&mut user_query)
            .ok()
            .filter(|n| *n > 0)
            .is_none()
        {
            return 1;
        }
        user_query = user_query.trim_end_matches(&['\n', '\r'][..]).to_string();
    }

    if user_query.trim().is_empty() {
        let _ = writeln!(stderr, "codex-tools: missing question");
        return 1;
    }

    let Some(final_prompt) = render_template(template_name, &user_query, stderr) else {
        return 1;
    };

    exec::exec_dangerous_with_options(
        &final_prompt,
        &format!("codex-tools:{template_name}"),
        stderr,
        exec_options,
    )
}

fn render_template(
    template_name: &str,
    user_query: &str,
    stderr: &mut impl Write,
) -> Option<String> {
    let template_content = match crate::prompts::read_template(template_name) {
        Ok((_path, content)) => content,
        Err(crate::prompts::PromptTemplateError::TemplateMissing { path }) => {
            let _ = writeln!(
                stderr,
                "codex-tools: prompt template not found: {}",
                path.to_string_lossy()
            );
            return None;
        }
        Err(crate::prompts::PromptTemplateError::ReadFailed { path }) => {
            let _ = writeln!(
                stderr,
                "codex-tools: failed to read prompt template: {}",
                path.to_string_lossy()
            );
            return None;
        }
        Err(crate::prompts::PromptTemplateError::PromptsDirNotFound) => return None,
    };
    Some(template_content.replace("$ARGUMENTS", user_query))
}

fn collect_input<R: BufRead, WOut: Write, WErr: Write>(
    prompt_label: &str,
    missing_label: &str,
    args: &[String],
    stdin: &mut R,
    stdout: &mut WOut,
    stderr: &mut WErr,
) -> Option<String> {
    let mut value = args.join(" ");
    if value.trim().is_empty() {
        write!(stdout, "{prompt_label}").ok()?;
        stdout.flush().ok()?;
        value.clear();
        stdin.read_line(&mut value).ok().filter(|n| *n > 0)?;
        value = value.trim_end_matches(&['\n', '\r'][..]).to_string();
    }
    if value.trim().is_empty() {
        let _ = writeln!(stderr, "codex-tools: missing {missing_label}");
        None
    } else {
        Some(value)
    }
}

fn resolve_runtime(
    options: AgentRuntimeOptions,
    stderr: &mut impl Write,
) -> Option<AgentRuntimeMode> {
    match options.resolve() {
        Ok(mode) => Some(mode),
        Err(message) => {
            let _ = writeln!(stderr, "codex-cli agent: {message}");
            None
        }
    }
}
