//! `pr tasks` atom — GFM task-list state for a PR / MR description.
//!
//! Spec / ops: `cli.forge-cli.pr.tasks.v1`. Parses GitHub-flavored-Markdown
//! task-list items (`- [ ]` / `- [x]`) out of the PR/MR description and
//! returns each item with its checked state, text, and source line. The
//! description is fetched through `pr view --json number,url,body` on GitHub
//! and the MR `description` field on GitLab; the parse itself is
//! provider-neutral. Items inside fenced code blocks are not task-list items
//! and are skipped.
//!
//! Also hosts [`ensure_tasklist_complete`], the `pr merge` lock-down rule
//! (rule 13): merging while the description still contains unchecked task
//! items fails closed with `unchecked_task_items` unless
//! `--allow-unchecked-tasks` (paired with a recorded
//! `--allow-unchecked-tasks-reason`) is passed. `- [x]` / `- [X]` count as
//! done; GitLab's `- [~]` (inapplicable) counts as dispositioned.
//!
//! The local provider stores no PR body: the atom returns an empty item list
//! and the merge gate passes trivially.

use std::ffi::OsString;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{
    BackendCall, BackendProgram, BackendRunner, BackendSuccess, DryRunPayload, ProcessRunner,
};
use crate::cli::{BINARY, GlobalFlags, PrTasksArgs};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};

const SCHEMA: &str = "pr.tasks";
const SCHEMA_VERSION: u32 = 1;

/// One GFM task-list item from the PR/MR description.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PrTaskItem {
    pub checked: bool,
    /// Raw checkbox marker: `" "` (unchecked), `"x"` / `"X"` (done), or
    /// `"~"` (GitLab inapplicable). Anything marked counts as dispositioned.
    pub marker: String,
    pub text: String,
    /// 1-based line number within the description.
    pub line: usize,
}

/// Envelope payload for `cli.forge-cli.pr.tasks.v1`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PrTasksPayload {
    pub provider: &'static str,
    pub number: u64,
    pub url: String,
    pub total: usize,
    pub unchecked: usize,
    pub items: Vec<PrTaskItem>,
}

pub fn run(
    global: &GlobalFlags,
    args: PrTasksArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    if global.is_local() {
        let runner = crate::local::LocalRunner::from_global(global)?;
        return run_with(&runner, global, args, format, git_remote_url);
    }
    let runner = ProcessRunner;
    run_with(&runner, global, args, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: PrTasksArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        remote_url_lookup,
    )?;

    let view_call = build_tasks_view_call(&ctx, args.id);

    if global.dry_run {
        let payload = DryRunPayload::new(ctx.provider, &view_call);
        return Ok(emit_success(
            schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
            payload,
            format,
            |p| println!("would run: {plan}", plan = p.plan.join(" ")),
        ));
    }

    let output = runner.run(&view_call)?;
    let view = parse_tasks_view(&ctx, &output)?;
    let items = parse_task_items(&view.body);
    let unchecked = items.iter().filter(|i| !i.checked).count();

    Ok(emit_success(
        schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
        PrTasksPayload {
            provider: ctx.provider.as_str(),
            number: view.number,
            url: view.url,
            total: items.len(),
            unchecked,
            items,
        },
        format,
        render_text,
    ))
}

/// `pr merge` lock-down rule 13. Parses the PR/MR description and fails
/// closed with `unchecked_task_items` (DATA 65) when any GFM task-list item
/// is still unchecked. Bodyless providers (local) pass trivially via the
/// empty string. Callers bypass via `--allow-unchecked-tasks`, which skips
/// this call entirely and records its paired reason in the merge payload.
pub fn ensure_tasklist_complete(body: &str) -> Result<(), ForgeError> {
    let unchecked: Vec<PrTaskItem> = parse_task_items(body)
        .into_iter()
        .filter(|i| !i.checked)
        .collect();
    if unchecked.is_empty() {
        return Ok(());
    }
    let listing = unchecked
        .iter()
        .map(|i| format!("- line {line}: {text}", line = i.line, text = i.text))
        .collect::<Vec<_>>()
        .join("\n");
    Err(ForgeError::validation(
        schema_err(),
        "unchecked_task_items",
        format!(
            "{n} unchecked task-list item(s) in the PR/MR description; disposition each (complete and check it off, or rewrite it as deferred with a follow-up ref) or pass --allow-unchecked-tasks with --allow-unchecked-tasks-reason to bypass",
            n = unchecked.len(),
        ),
        Some(listing),
    ))
}

/// Parse GFM task-list items out of a Markdown body. Recognizes bullet
/// (`-` / `*` / `+`) and ordered (`1.` / `1)`) list items whose content
/// starts with `[ ]`, `[x]`, `[X]`, or `[~]` followed by whitespace or end
/// of line. Lines inside fenced code blocks (``` or ~~~) are skipped.
pub fn parse_task_items(body: &str) -> Vec<PrTaskItem> {
    let mut items = Vec::new();
    let mut fence: Option<char> = None;
    for (idx, line) in body.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some(open) = fence {
            let close = if open == '`' { "```" } else { "~~~" };
            if trimmed.starts_with(close) {
                fence = None;
            }
            continue;
        }
        if trimmed.starts_with("```") {
            fence = Some('`');
            continue;
        }
        if trimmed.starts_with("~~~") {
            fence = Some('~');
            continue;
        }
        if let Some((checked, marker, text)) = parse_task_line(trimmed) {
            items.push(PrTaskItem {
                checked,
                marker: marker.to_string(),
                text,
                line: idx + 1,
            });
        }
    }
    items
}

/// Parse one (already left-trimmed) line as a task-list item. Returns
/// `(checked, marker, text)` or `None` when the line is not a task item.
fn parse_task_line(trimmed: &str) -> Option<(bool, char, String)> {
    let after_marker = strip_list_marker(trimmed)?;
    let rest = after_marker.trim_start();
    let mut chars = rest.chars();
    if chars.next()? != '[' {
        return None;
    }
    let marker = chars.next()?;
    if chars.next()? != ']' {
        return None;
    }
    if !matches!(marker, ' ' | 'x' | 'X' | '~') {
        return None;
    }
    let tail = chars.as_str();
    // GFM only treats the bracket pair as a checkbox when followed by
    // whitespace or end of line; `- [ ]text` renders as literal text.
    let text = if tail.is_empty() {
        String::new()
    } else if tail.starts_with(' ') || tail.starts_with('\t') {
        tail.trim().to_string()
    } else {
        return None;
    };
    Some((marker != ' ', marker, text))
}

/// Strip a leading list marker (`- ` / `* ` / `+ ` / `1. ` / `1) `) and
/// return the content after it.
fn strip_list_marker(trimmed: &str) -> Option<&str> {
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(bullet) {
            return Some(rest);
        }
    }
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 9 {
        return None;
    }
    let after = &trimmed[digits..];
    let after = after
        .strip_prefix('.')
        .or_else(|| after.strip_prefix(')'))?;
    after.strip_prefix(' ')
}

/// Minimal view projection for `pr tasks`: number, url, and description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TasksView {
    pub number: u64,
    pub url: String,
    pub body: String,
}

pub(crate) fn build_tasks_view_call(ctx: &ProviderContext, id: u64) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let id_str = id.to_string();
    let mut argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub | Provider::Local => vec![
            OsString::from("pr"),
            OsString::from("view"),
            OsString::from(id_str),
            OsString::from("--json"),
            OsString::from("number,url,body"),
        ],
        Provider::GitLab => vec![
            OsString::from("mr"),
            OsString::from("view"),
            OsString::from(id_str),
            OsString::from("-F"),
            OsString::from("json"),
        ],
    };
    ctx.push_repo_override(&mut argv);
    BackendCall::new(program, argv)
}

pub(crate) fn parse_tasks_view(
    ctx: &ProviderContext,
    output: &BackendSuccess,
) -> Result<TasksView, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "pr view returned invalid JSON",
            Some(e.to_string()),
        )
    })?;
    let (number_key, url_key, body_key) = match ctx.provider {
        Provider::GitHub | Provider::Local => ("number", "url", "body"),
        Provider::GitLab => ("iid", "web_url", "description"),
    };
    let number = value
        .get(number_key)
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            ForgeError::software(
                schema_err(),
                "pr view response is missing the PR/MR number",
                Some(format!("stdout={:?}", output.stdout)),
            )
        })?;
    let url = value
        .get(url_key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let body = value
        .get(body_key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok(TasksView { number, url, body })
}

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

fn render_text(payload: &PrTasksPayload) {
    println!(
        "{provider} #{number} ({total} task-list items, {unchecked} unchecked)\n  {url}",
        provider = payload.provider,
        number = payload.number,
        total = payload.total,
        unchecked = payload.unchecked,
        url = payload.url,
    );
    for item in &payload.items {
        let state = if item.checked { "x" } else { " " };
        println!(
            "  - [{state}] (line {line}) {text}",
            line = item.line,
            text = item.text,
        );
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use nils_common::cli_contract::OutputFormat;
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::backend::BackendOutput;
    use crate::cli::GlobalFlags;
    use crate::provider::DetectionSource;

    type RecordedCall = (BackendProgram, Vec<String>);

    struct ScriptedRunner {
        outputs: RefCell<Vec<BackendSuccess>>,
        calls: RefCell<Vec<RecordedCall>>,
    }

    impl ScriptedRunner {
        fn new(outputs: Vec<BackendSuccess>) -> Self {
            Self {
                outputs: RefCell::new(outputs),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<RecordedCall> {
            self.calls.borrow().clone()
        }
    }

    impl BackendRunner for ScriptedRunner {
        fn run(&self, call: &BackendCall) -> Result<BackendSuccess, ForgeError> {
            let argv = call
                .argv
                .iter()
                .map(|os| os.to_string_lossy().into_owned())
                .collect();
            self.calls.borrow_mut().push((call.program, argv));
            Ok(self.outputs.borrow_mut().remove(0))
        }

        fn run_raw(&self, call: &BackendCall) -> Result<BackendOutput, ForgeError> {
            self.run(call).map(|s| BackendOutput {
                exit_code: 0,
                status_success: true,
                stdout: s.stdout,
                stderr: s.stderr,
            })
        }
    }

    fn ctx(provider: Provider) -> ProviderContext {
        ProviderContext {
            provider,
            host: match provider {
                Provider::GitLab => "gitlab.com".into(),
                _ => "github.com".into(),
            },
            source: DetectionSource::Flag,
            repo: None,
        }
    }

    #[test]
    fn parse_finds_bullet_and_ordered_items_with_line_numbers() {
        let body = "## Test plan\n\n- [ ] run unit tests\n* [x] update docs\n+ [X] ship\n1. [ ] ordered item\n2) [~] inapplicable\n";
        let items = parse_task_items(body);
        assert_eq!(items.len(), 5);
        assert_eq!(items[0].checked, false);
        assert_eq!(items[0].text, "run unit tests");
        assert_eq!(items[0].line, 3);
        assert!(items[1].checked);
        assert!(items[2].checked);
        assert_eq!(items[3].checked, false);
        assert_eq!(items[3].line, 6);
        assert!(
            items[4].checked,
            "GitLab inapplicable counts as dispositioned"
        );
        assert_eq!(items[4].marker, "~");
    }

    #[test]
    fn parse_accepts_nested_indentation_and_empty_text() {
        let body = "- [x] parent\n  - [ ] nested child\n- [ ]\n";
        let items = parse_task_items(body);
        assert_eq!(items.len(), 3);
        assert_eq!(items[1].text, "nested child");
        assert_eq!(items[2].text, "");
        assert_eq!(items[2].checked, false);
    }

    #[test]
    fn parse_skips_fenced_code_blocks_and_non_task_lines() {
        let body = "intro\n```markdown\n- [ ] inside backtick fence\n```\n~~~\n- [ ] inside tilde fence\n~~~\n- [ ] real item\n- [?] not a checkbox\n- [ ]not a checkbox either\n-[ ] missing marker space\n";
        let items = parse_task_items(body);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "real item");
    }

    #[test]
    fn parse_handles_unclosed_fence_to_end_of_body() {
        let body = "- [x] before\n```\n- [ ] swallowed by open fence\n";
        let items = parse_task_items(body);
        assert_eq!(items.len(), 1);
        assert!(items[0].checked);
    }

    #[test]
    fn ensure_passes_on_empty_body_and_all_checked() {
        ensure_tasklist_complete("").expect("empty body must pass");
        ensure_tasklist_complete("no lists here").expect("no items must pass");
        ensure_tasklist_complete("- [x] done\n- [~] skipped\n")
            .expect("all dispositioned must pass");
    }

    #[test]
    fn ensure_fails_data_65_with_listing_when_items_unchecked() {
        let err = ensure_tasklist_complete("- [x] done\n- [ ] write release notes\n")
            .expect_err("unchecked items must fail");
        assert_eq!(err.kind(), "unchecked_task_items");
        assert_eq!(err.exit_code(), 65);
        let rendered = format!("{err:?}");
        assert!(rendered.contains("write release notes"));
        assert!(rendered.contains("--allow-unchecked-tasks"));
    }

    fn json_global() -> GlobalFlags {
        GlobalFlags {
            format: Some(OutputFormat::Json),
            remote: "origin".into(),
            provider: Some(crate::cli::ProviderFlag::Github),
            repo: Some("acme/widgets".into()),
            store_root: None,
            dry_run: false,
        }
    }

    #[test]
    fn github_view_call_requests_number_url_body() {
        let call = build_tasks_view_call(&ctx(Provider::GitHub), 7);
        let argv: Vec<String> = call
            .argv
            .iter()
            .map(|os| os.to_string_lossy().into_owned())
            .collect();
        assert_eq!(call.program, BackendProgram::Gh);
        assert_eq!(
            argv[0..3],
            ["pr".to_string(), "view".to_string(), "7".to_string()]
        );
        assert!(argv.iter().any(|s| s == "number,url,body"));
    }

    #[test]
    fn parse_view_github_reads_body() {
        let output = BackendSuccess {
            stdout:
                r#"{"number":7,"url":"https://github.com/acme/widgets/pull/7","body":"- [ ] item"}"#
                    .into(),
            stderr: String::new(),
        };
        let view = parse_tasks_view(&ctx(Provider::GitHub), &output).expect("parse");
        assert_eq!(view.number, 7);
        assert_eq!(view.body, "- [ ] item");
    }

    #[test]
    fn parse_view_gitlab_reads_description() {
        let output = BackendSuccess {
            stdout: r#"{"iid":9,"web_url":"https://gitlab.example.com/g/p/-/merge_requests/9","description":"- [x] item"}"#
                .into(),
            stderr: String::new(),
        };
        let view = parse_tasks_view(&ctx(Provider::GitLab), &output).expect("parse");
        assert_eq!(view.number, 9);
        assert_eq!(view.body, "- [x] item");
    }

    #[test]
    fn run_with_github_counts_unchecked_items() {
        let view = BackendSuccess {
            stdout: r#"{"number":7,"url":"https://github.com/acme/widgets/pull/7","body":"- [ ] a\n- [x] b"}"#
                .into(),
            stderr: String::new(),
        };
        let runner = ScriptedRunner::new(vec![view]);
        let args = PrTasksArgs { id: 7 };
        let code = run_with(&runner, &json_global(), args, OutputFormat::Json, |_| {
            Some("git@github.com:acme/widgets.git".into())
        })
        .expect("run");
        assert_eq!(code, 0);
        assert_eq!(runner.calls().len(), 1);
    }

    #[test]
    fn run_with_dry_run_plans_view_call_without_running() {
        let runner = ScriptedRunner::new(vec![]);
        let mut global = json_global();
        global.dry_run = true;
        let args = PrTasksArgs { id: 7 };
        let code = run_with(&runner, &global, args, OutputFormat::Json, |_| {
            Some("git@github.com:acme/widgets.git".into())
        })
        .expect("run");
        assert_eq!(code, 0);
        assert!(runner.calls().is_empty());
    }
}
