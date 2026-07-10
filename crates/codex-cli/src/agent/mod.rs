use std::io::{self, BufRead, Write};

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

    let template_content = match crate::prompts::read_template(template_name) {
        Ok((_path, content)) => content,
        Err(crate::prompts::PromptTemplateError::TemplateMissing { path }) => {
            let _ = writeln!(
                stderr,
                "codex-tools: prompt template not found: {}",
                path.to_string_lossy()
            );
            return 1;
        }
        Err(crate::prompts::PromptTemplateError::ReadFailed { path }) => {
            let _ = writeln!(
                stderr,
                "codex-tools: failed to read prompt template: {}",
                path.to_string_lossy()
            );
            return 1;
        }
        Err(crate::prompts::PromptTemplateError::PromptsDirNotFound) => return 1,
    };

    let final_prompt = template_content.replace("$ARGUMENTS", &user_query);
    exec::exec_dangerous_with_options(
        &final_prompt,
        &format!("codex-tools:{template_name}"),
        stderr,
        exec_options,
    )
}
