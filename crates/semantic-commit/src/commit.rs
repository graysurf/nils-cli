use crate::git;
use nils_common::agent_attribution;
use nils_common::git as common_git;
use nils_term::progress::{Progress, ProgressFinish, ProgressOptions};
use serde_json::json;
use std::fs::File;
use std::io::{BufRead, BufReader, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};

const EXIT_ERROR: i32 = 1;
const EXIT_NO_STAGED_CHANGES: i32 = 2;
const EXIT_MESSAGE_REQUIRED: i32 = 3;
const EXIT_VALIDATION_FAILED: i32 = 4;
const EXIT_DEPENDENCY_ERROR: i32 = 5;
const CAT_PAGER_ENV: [(&str, &str); 2] = [("GIT_PAGER", "cat"), ("PAGER", "cat")];
const NONINTERACTIVE_COMMIT_ENV: [(&str, &str); 3] = [
    ("GIT_PAGER", "cat"),
    ("PAGER", "cat"),
    ("GIT_EDITOR", "true"),
];
const COMMIT_RESULT_SCHEMA_VERSION: &str = "cli.semantic-commit.commit.v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SummaryMode {
    GitScope,
    GitShow,
    None,
}

impl SummaryMode {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "git-scope" => Some(Self::GitScope),
            "git-show" => Some(Self::GitShow),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutputFormat {
    Text,
    Json,
}

impl OutputFormat {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "text" => Some(Self::Text),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommitOperation {
    Create,
    Amend,
    MessageOnlyAmend,
}

impl CommitOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Create => "commit",
            Self::Amend => "amend",
            Self::MessageOnlyAmend => "message_only_amend",
        }
    }

    fn progress_label(self) -> &'static str {
        match self {
            Self::Create => "git commit",
            Self::Amend | Self::MessageOnlyAmend => "git commit --amend",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StructuredMessage {
    typ: Option<String>,
    scope: Option<String>,
    subject: Option<String>,
    body_bullets: Vec<String>,
}

impl StructuredMessage {
    fn has_any(&self) -> bool {
        self.typ.is_some()
            || self.scope.is_some()
            || self.subject.is_some()
            || !self.body_bullets.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CommitMetadata {
    sha: String,
    subject: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StagedEntry {
    status: String,
    path: String,
    old_path: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CleanupOperation {
    Fixup,
    Squash,
}

impl CleanupOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fixup => "fixup",
            Self::Squash => "squash",
        }
    }

    fn git_flag(self) -> &'static str {
        match self {
            Self::Fixup => "--fixup",
            Self::Squash => "--squash",
        }
    }

    fn subject_prefix(self) -> &'static str {
        match self {
            Self::Fixup => "fixup!",
            Self::Squash => "squash!",
        }
    }
}

#[derive(Debug)]
struct CleanupOptions {
    target: Option<String>,
    summary_mode: SummaryMode,
    output_format: OutputFormat,
    no_progress: bool,
    quiet: bool,
    dry_run: bool,
    allow_empty: bool,
    require_clean: bool,
    expect_head: Option<String>,
    repo: Option<PathBuf>,
}

impl CleanupOptions {
    fn new(_operation: CleanupOperation) -> Self {
        Self {
            target: None,
            summary_mode: SummaryMode::GitScope,
            output_format: OutputFormat::Text,
            no_progress: false,
            quiet: false,
            dry_run: false,
            allow_empty: false,
            require_clean: false,
            expect_head: None,
            repo: None,
        }
    }
}

#[derive(Debug)]
struct CommitOptions {
    message: Option<String>,
    message_file: Option<String>,
    message_out: Option<PathBuf>,
    summary_mode: SummaryMode,
    output_format: OutputFormat,
    no_progress: bool,
    quiet: bool,
    automation: bool,
    validate_only: bool,
    dry_run: bool,
    auto_fix: bool,
    amend: bool,
    no_edit: bool,
    message_only: bool,
    allow_empty: bool,
    require_clean: bool,
    expect_head: Option<String>,
    signoff: bool,
    trailers: Vec<String>,
    structured: StructuredMessage,
    repo: Option<PathBuf>,
    max_header_width: usize,
}

impl Default for CommitOptions {
    fn default() -> Self {
        Self {
            message: None,
            message_file: None,
            message_out: None,
            summary_mode: SummaryMode::GitScope,
            output_format: OutputFormat::Text,
            no_progress: false,
            quiet: false,
            automation: false,
            validate_only: false,
            dry_run: false,
            auto_fix: false,
            amend: false,
            no_edit: false,
            message_only: false,
            allow_empty: false,
            require_clean: false,
            expect_head: None,
            signoff: false,
            trailers: Vec::new(),
            structured: StructuredMessage {
                typ: None,
                scope: None,
                subject: None,
                body_bullets: Vec::new(),
            },
            repo: None,
            max_header_width: DEFAULT_MAX_HEADER_WIDTH,
        }
    }
}

pub fn run(args: &[String]) -> i32 {
    run_with_signing_policy(args, false)
}

fn run_with_signing_policy(args: &[String], force_signing: bool) -> i32 {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(code) => return code,
    };
    run_options(options, force_signing)
}

/// Typed commit configuration shared by the exceptional default-branch path.
///
/// The default-branch parser delegates every message-construction option to
/// this type, so the ordinary and exceptional commands cannot drift through a
/// manually synchronized option/value list.
pub(crate) struct DefaultBranchCommitOptions {
    options: CommitOptions,
    max_header_width_from_flag: bool,
}

impl DefaultBranchCommitOptions {
    pub(crate) fn new() -> Self {
        Self {
            options: CommitOptions::default(),
            max_header_width_from_flag: false,
        }
    }

    pub(crate) fn parse_message_argument(
        &mut self,
        args: &[String],
        index: usize,
    ) -> Result<Option<usize>, i32> {
        parse_message_argument(
            args,
            index,
            &mut self.options,
            &mut self.max_header_width_from_flag,
        )
    }

    pub(crate) fn finish(&mut self) -> Result<(), i32> {
        finish_message_options(&mut self.options, self.max_header_width_from_flag)
    }

    pub(crate) fn run(mut self, repo: PathBuf, expect_head: String, dry_run: bool) -> i32 {
        self.options.summary_mode = SummaryMode::None;
        self.options.no_progress = true;
        self.options.quiet = true;
        self.options.dry_run = dry_run;
        self.options.require_clean = true;
        self.options.expect_head = Some(expect_head);
        self.options.repo = Some(repo);
        run_options(self.options, true)
    }
}

fn run_options(mut options: CommitOptions, force_signing: bool) -> i32 {
    let operation = commit_operation(&options);

    if !git::command_exists("git") {
        eprintln!("error: git is required (ensure it is installed and on PATH)");
        return EXIT_DEPENDENCY_ERROR;
    }

    if options.output_format == OutputFormat::Json {
        options.no_progress = true;
        options.summary_mode = SummaryMode::None;
    }

    if options.quiet {
        options.no_progress = true;
        if options.output_format == OutputFormat::Text {
            options.summary_mode = SummaryMode::None;
        }
    }

    let mut message_contents = match read_message_contents(&options) {
        Ok(contents) => contents,
        Err(code) => return code,
    };

    if options.auto_fix {
        message_contents = normalize_message(&message_contents);
    }

    if let Some(path) = options.message_out.as_deref()
        && let Err(err) = write_message_file(path, &message_contents)
    {
        eprintln!("error: failed to write --message-out file: {err}");
        return EXIT_ERROR;
    }

    if let Err(code) = validate_blocked_message_rules(&message_contents, &options.trailers) {
        return code;
    }

    let tmpfile = match tempfile::NamedTempFile::new() {
        Ok(file) => file,
        Err(_) => {
            eprintln!("error: failed to create temp file for commit message");
            return EXIT_ERROR;
        }
    };

    if let Err(err) = write_message_file(tmpfile.path(), &message_contents) {
        eprintln!("{err:#}");
        return EXIT_ERROR;
    }

    if let Err(code) = validate_commit_message_with_width(tmpfile.path(), options.max_header_width)
    {
        return code;
    }

    if options.validate_only {
        if options.output_format == OutputFormat::Json {
            print_commit_json_result(operation, true, false, None, None, Vec::new());
        }
        return 0;
    }

    if !git::is_inside_work_tree(options.repo.as_deref()) {
        eprintln!("error: must run inside a git work tree");
        return EXIT_ERROR;
    }

    if let Some(expected) = options.expect_head.as_deref()
        && let Err(code) = ensure_expected_head(options.repo.as_deref(), expected)
    {
        return code;
    }

    if options.require_clean
        && let Err(code) = ensure_no_unstaged_or_untracked(options.repo.as_deref())
    {
        return code;
    }

    let has_staged_changes = match git::has_staged_changes(options.repo.as_deref()) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("{err:#}");
            return EXIT_ERROR;
        }
    };

    if operation == CommitOperation::MessageOnlyAmend && has_staged_changes {
        eprintln!("error: --message-only requires no staged changes");
        return EXIT_ERROR;
    }

    if !has_staged_changes && !options.allow_empty && operation != CommitOperation::MessageOnlyAmend
    {
        eprintln!("error: no staged changes (stage files with git add first)");
        return EXIT_NO_STAGED_CHANGES;
    }

    let staged_entries = match staged_entries(options.repo.as_deref()) {
        Ok(entries) => entries,
        Err(err) => {
            eprintln!("{err:#}");
            return EXIT_ERROR;
        }
    };

    if options.dry_run {
        if options.output_format == OutputFormat::Json {
            print_commit_json_result(
                operation,
                false,
                true,
                current_head_metadata(options.repo.as_deref()),
                None,
                staged_entries,
            );
        }
        return 0;
    }

    let progress = if !options.no_progress {
        Some(Progress::spinner(
            ProgressOptions::default()
                .with_prefix("semantic-commit ")
                .with_finish(ProgressFinish::Clear),
        ))
    } else {
        None
    };

    if let Some(progress) = &progress {
        progress.set_message(operation.progress_label());
        progress.tick();
    }

    let status = git_commit(tmpfile.path(), &options, operation, force_signing);

    if let Some(progress) = &progress {
        progress.finish_and_clear();
    }

    match status {
        Ok(status) if status.success() => {}
        Ok(status) => {
            let rc = status.code().unwrap_or(EXIT_ERROR);
            eprintln!("error: git commit failed (exit code: {rc})");
            return rc;
        }
        Err(err) => {
            eprintln!("{err:#}");
            return EXIT_ERROR;
        }
    }

    let commit = current_head_metadata(options.repo.as_deref());
    if options.output_format == OutputFormat::Json {
        print_commit_json_result(operation, false, false, commit, None, staged_entries);
        return 0;
    }

    print_summary(options.summary_mode, options.repo.as_deref())
}

fn commit_operation(options: &CommitOptions) -> CommitOperation {
    if options.message_only {
        CommitOperation::MessageOnlyAmend
    } else if options.amend {
        CommitOperation::Amend
    } else {
        CommitOperation::Create
    }
}

pub fn run_fixup(args: &[String]) -> i32 {
    run_cleanup(args, CleanupOperation::Fixup)
}

pub fn run_squash(args: &[String]) -> i32 {
    run_cleanup(args, CleanupOperation::Squash)
}

fn run_cleanup(args: &[String], operation: CleanupOperation) -> i32 {
    let mut options = match parse_cleanup_args(args, operation) {
        Ok(options) => options,
        Err(code) => return code,
    };

    if !git::command_exists("git") {
        eprintln!("error: git is required (ensure it is installed and on PATH)");
        return EXIT_DEPENDENCY_ERROR;
    }

    if options.output_format == OutputFormat::Json {
        options.no_progress = true;
        options.summary_mode = SummaryMode::None;
    }

    if options.quiet {
        options.no_progress = true;
        if options.output_format == OutputFormat::Text {
            options.summary_mode = SummaryMode::None;
        }
    }

    if !git::is_inside_work_tree(options.repo.as_deref()) {
        eprintln!("error: must run inside a git work tree");
        return EXIT_ERROR;
    }

    let target = match resolve_target_metadata(options.repo.as_deref(), options.target.as_deref()) {
        Ok(target) => target,
        Err(code) => return code,
    };

    if let Some(expected) = options.expect_head.as_deref()
        && let Err(code) = ensure_expected_head(options.repo.as_deref(), expected)
    {
        return code;
    }

    if options.require_clean
        && let Err(code) = ensure_no_unstaged_or_untracked(options.repo.as_deref())
    {
        return code;
    }

    let has_staged_changes = match git::has_staged_changes(options.repo.as_deref()) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("{err:#}");
            return EXIT_ERROR;
        }
    };
    if !has_staged_changes && !options.allow_empty {
        eprintln!("error: no staged changes (stage files with git add first)");
        return EXIT_NO_STAGED_CHANGES;
    }

    let staged_entries = match staged_entries(options.repo.as_deref()) {
        Ok(entries) => entries,
        Err(err) => {
            eprintln!("{err:#}");
            return EXIT_ERROR;
        }
    };

    if options.dry_run {
        if options.output_format == OutputFormat::Json {
            print_cleanup_json_result(operation, true, None, target, staged_entries);
        }
        return 0;
    }

    let progress = if !options.no_progress {
        Some(Progress::spinner(
            ProgressOptions::default()
                .with_prefix("semantic-commit ")
                .with_finish(ProgressFinish::Clear),
        ))
    } else {
        None
    };

    if let Some(progress) = &progress {
        progress.set_message(format!("git commit --{}", operation.as_str()));
        progress.tick();
    }

    let status = git_cleanup_commit(&options, operation, &target.sha);

    if let Some(progress) = &progress {
        progress.finish_and_clear();
    }

    match status {
        Ok(status) if status.success() => {}
        Ok(status) => {
            let rc = status.code().unwrap_or(EXIT_ERROR);
            eprintln!(
                "error: git commit --{} failed (exit code: {rc})",
                operation.as_str()
            );
            return rc;
        }
        Err(err) => {
            eprintln!("{err:#}");
            return EXIT_ERROR;
        }
    }

    let commit = current_head_metadata(options.repo.as_deref());
    if options.output_format == OutputFormat::Json {
        print_cleanup_json_result(operation, false, commit, target, staged_entries);
        return 0;
    }

    print_summary(options.summary_mode, options.repo.as_deref())
}

fn parse_message_argument(
    args: &[String],
    index: usize,
    options: &mut CommitOptions,
    max_header_width_from_flag: &mut bool,
) -> Result<Option<usize>, i32> {
    let value = |label: &str| {
        args.get(index + 1).cloned().ok_or_else(|| {
            eprintln!("error: {label} requires a value");
            EXIT_ERROR
        })
    };
    let next = match args[index].as_str() {
        "--message" | "-m" => {
            options.message = Some(value(args[index].as_str())?);
            index + 2
        }
        "--message-file" | "-F" => {
            options.message_file = Some(args.get(index + 1).cloned().ok_or_else(|| {
                eprintln!("error: {} requires a path", args[index]);
                EXIT_ERROR
            })?);
            index + 2
        }
        "--message-out" => {
            options.message_out = Some(PathBuf::from(value("--message-out")?));
            index + 2
        }
        "--automation" | "--non-interactive" => {
            options.automation = true;
            index + 1
        }
        "--auto-fix" => {
            options.auto_fix = true;
            index + 1
        }
        "--signoff" => {
            options.signoff = true;
            index + 1
        }
        "--trailer" => {
            options.trailers.push(value("--trailer")?);
            index + 2
        }
        "--type" => {
            options.structured.typ = Some(value("--type")?);
            index + 2
        }
        "--scope" => {
            options.structured.scope = Some(value("--scope")?);
            index + 2
        }
        "--subject" => {
            options.structured.subject = Some(value("--subject")?);
            index + 2
        }
        "--body-bullet" | "--bullet" => {
            options
                .structured
                .body_bullets
                .push(value(args[index].as_str())?);
            index + 2
        }
        "--max-header-width" => {
            let width = value("--max-header-width")?;
            options.max_header_width = parse_header_width_flag(&width)?;
            *max_header_width_from_flag = true;
            index + 2
        }
        _ => return Ok(None),
    };
    Ok(Some(next))
}

fn finish_message_options(
    options: &mut CommitOptions,
    max_header_width_from_flag: bool,
) -> Result<(), i32> {
    if options.message.is_some() && options.message_file.is_some() {
        eprintln!("error: use only one of --message or --message-file");
        return Err(EXIT_ERROR);
    }
    if options.structured.has_any() && (options.message.is_some() || options.message_file.is_some())
    {
        eprintln!(
            "error: structured message fields cannot be combined with --message or --message-file"
        );
        return Err(EXIT_ERROR);
    }
    if options.structured.has_any()
        && (options.structured.typ.is_none() || options.structured.subject.is_none())
    {
        eprintln!("error: structured message fields require --type and --subject");
        return Err(EXIT_ERROR);
    }
    if let Err(message) = validate_trailers(&options.trailers) {
        eprintln!("error: {message}");
        return Err(EXIT_ERROR);
    }
    if !max_header_width_from_flag {
        options.max_header_width = env_header_width()?;
    }
    Ok(())
}

fn parse_args(args: &[String]) -> Result<CommitOptions, i32> {
    let mut options = CommitOptions::default();
    let mut max_header_width_from_flag = false;

    let mut i = 0;
    while i < args.len() {
        if let Some(next) =
            parse_message_argument(args, i, &mut options, &mut max_header_width_from_flag)?
        {
            i = next;
            continue;
        }
        match args[i].as_str() {
            "-h" | "--help" => {
                print_usage_stdout();
                return Err(0);
            }
            "--summary" => {
                let value = match args.get(i + 1) {
                    Some(value) => value,
                    None => {
                        eprintln!("error: --summary requires a value");
                        print_usage_stderr();
                        return Err(EXIT_ERROR);
                    }
                };

                let Some(mode) = SummaryMode::parse(value) else {
                    eprintln!(
                        "error: invalid --summary value: {value} (expected: git-scope, git-show, none)"
                    );
                    print_usage_stderr();
                    return Err(EXIT_ERROR);
                };

                options.summary_mode = mode;
                i += 2;
            }
            "--no-summary" => {
                options.summary_mode = SummaryMode::None;
                i += 1;
            }
            "--format" => {
                let value = match args.get(i + 1) {
                    Some(value) => value,
                    None => {
                        eprintln!("error: --format requires a value");
                        print_usage_stderr();
                        return Err(EXIT_ERROR);
                    }
                };
                let Some(format) = OutputFormat::parse(value) else {
                    eprintln!("error: invalid --format value: {value} (expected: text, json)");
                    print_usage_stderr();
                    return Err(EXIT_ERROR);
                };
                options.output_format = format;
                i += 2;
            }
            "--json" => {
                options.output_format = OutputFormat::Json;
                i += 1;
            }
            "--no-progress" => {
                options.no_progress = true;
                i += 1;
            }
            "--quiet" => {
                options.quiet = true;
                i += 1;
            }
            "--validate-only" => {
                options.validate_only = true;
                i += 1;
            }
            "--dry-run" => {
                options.dry_run = true;
                i += 1;
            }
            "--amend" => {
                options.amend = true;
                i += 1;
            }
            "--no-edit" => {
                options.no_edit = true;
                i += 1;
            }
            "--message-only" => {
                options.message_only = true;
                i += 1;
            }
            "--allow-empty" => {
                options.allow_empty = true;
                i += 1;
            }
            "--require-clean" | "--no-unstaged" => {
                options.require_clean = true;
                i += 1;
            }
            "--expect-head" => {
                let value = match args.get(i + 1) {
                    Some(value) => value.clone(),
                    None => {
                        eprintln!("error: --expect-head requires a revision");
                        print_usage_stderr();
                        return Err(EXIT_ERROR);
                    }
                };
                options.expect_head = Some(value);
                i += 2;
            }
            "--repo" => {
                let value = match args.get(i + 1) {
                    Some(value) => value.clone(),
                    None => {
                        eprintln!("error: --repo requires a path");
                        print_usage_stderr();
                        return Err(EXIT_ERROR);
                    }
                };
                options.repo = Some(PathBuf::from(value));
                i += 2;
            }
            other => {
                eprintln!("error: unknown argument: {other}");
                print_usage_stderr();
                return Err(EXIT_ERROR);
            }
        }
    }

    finish_message_options(&mut options, max_header_width_from_flag)?;

    if options.no_edit && !options.amend {
        eprintln!("error: --no-edit requires --amend");
        return Err(EXIT_ERROR);
    }

    if options.message_only && !options.amend {
        eprintln!("error: --message-only requires --amend");
        return Err(EXIT_ERROR);
    }

    if options.no_edit && options.message_only {
        eprintln!("error: use only one of --no-edit or --message-only");
        return Err(EXIT_ERROR);
    }

    if options.no_edit
        && (options.message.is_some()
            || options.message_file.is_some()
            || options.structured.has_any()
            || options.auto_fix)
    {
        eprintln!("error: --no-edit cannot be combined with message input or --auto-fix");
        return Err(EXIT_ERROR);
    }

    Ok(options)
}

fn parse_cleanup_args(args: &[String], operation: CleanupOperation) -> Result<CleanupOptions, i32> {
    let mut options = CleanupOptions::new(operation);

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_cleanup_usage_stdout(operation);
                return Err(0);
            }
            "--target" => {
                let value = match args.get(i + 1) {
                    Some(value) => value.clone(),
                    None => {
                        eprintln!("error: --target requires a revision");
                        print_cleanup_usage_stderr(operation);
                        return Err(EXIT_ERROR);
                    }
                };
                options.target = Some(value);
                i += 2;
            }
            "--summary" => {
                let value = match args.get(i + 1) {
                    Some(value) => value,
                    None => {
                        eprintln!("error: --summary requires a value");
                        print_cleanup_usage_stderr(operation);
                        return Err(EXIT_ERROR);
                    }
                };
                let Some(mode) = SummaryMode::parse(value) else {
                    eprintln!(
                        "error: invalid --summary value: {value} (expected: git-scope, git-show, none)"
                    );
                    print_cleanup_usage_stderr(operation);
                    return Err(EXIT_ERROR);
                };
                options.summary_mode = mode;
                i += 2;
            }
            "--no-summary" => {
                options.summary_mode = SummaryMode::None;
                i += 1;
            }
            "--format" => {
                let value = match args.get(i + 1) {
                    Some(value) => value,
                    None => {
                        eprintln!("error: --format requires a value");
                        print_cleanup_usage_stderr(operation);
                        return Err(EXIT_ERROR);
                    }
                };
                let Some(format) = OutputFormat::parse(value) else {
                    eprintln!("error: invalid --format value: {value} (expected: text, json)");
                    print_cleanup_usage_stderr(operation);
                    return Err(EXIT_ERROR);
                };
                options.output_format = format;
                i += 2;
            }
            "--json" => {
                options.output_format = OutputFormat::Json;
                i += 1;
            }
            "--dry-run" => {
                options.dry_run = true;
                i += 1;
            }
            "--allow-empty" => {
                options.allow_empty = true;
                i += 1;
            }
            "--require-clean" | "--no-unstaged" => {
                options.require_clean = true;
                i += 1;
            }
            "--expect-head" => {
                let value = match args.get(i + 1) {
                    Some(value) => value.clone(),
                    None => {
                        eprintln!("error: --expect-head requires a revision");
                        print_cleanup_usage_stderr(operation);
                        return Err(EXIT_ERROR);
                    }
                };
                options.expect_head = Some(value);
                i += 2;
            }
            "--repo" => {
                let value = match args.get(i + 1) {
                    Some(value) => value.clone(),
                    None => {
                        eprintln!("error: --repo requires a path");
                        print_cleanup_usage_stderr(operation);
                        return Err(EXIT_ERROR);
                    }
                };
                options.repo = Some(PathBuf::from(value));
                i += 2;
            }
            "--no-progress" => {
                options.no_progress = true;
                i += 1;
            }
            "--quiet" => {
                options.quiet = true;
                i += 1;
            }
            other => {
                eprintln!("error: unknown argument: {other}");
                print_cleanup_usage_stderr(operation);
                return Err(EXIT_ERROR);
            }
        }
    }

    if options.target.is_none() {
        eprintln!("error: --target is required");
        print_cleanup_usage_stderr(operation);
        return Err(EXIT_ERROR);
    }

    Ok(options)
}

fn env_header_width() -> Result<usize, i32> {
    match std::env::var("SEMANTIC_COMMIT_HEADER_WIDTH") {
        Ok(value) => parse_header_width_env(&value),
        Err(std::env::VarError::NotPresent) => Ok(DEFAULT_MAX_HEADER_WIDTH),
        Err(std::env::VarError::NotUnicode(_)) => {
            eprintln!("error: SEMANTIC_COMMIT_HEADER_WIDTH must be valid UTF-8");
            Err(EXIT_ERROR)
        }
    }
}

fn parse_header_width_flag(value: &str) -> Result<usize, i32> {
    parse_positive_width(value, "--max-header-width")
}

fn parse_header_width_env(value: &str) -> Result<usize, i32> {
    parse_positive_width(value, "SEMANTIC_COMMIT_HEADER_WIDTH")
}

fn parse_positive_width(value: &str, label: &str) -> Result<usize, i32> {
    let Ok(parsed) = value.parse::<usize>() else {
        eprintln!("error: {label} must be a positive integer");
        return Err(EXIT_ERROR);
    };
    if parsed == 0 {
        eprintln!("error: {label} must be a positive integer");
        return Err(EXIT_ERROR);
    }
    Ok(parsed)
}

fn validate_trailers(trailers: &[String]) -> Result<(), String> {
    for trailer in trailers {
        if !is_valid_trailer_line(trailer) {
            return Err(format!(
                "invalid --trailer value: {trailer} (expected 'Token: value' or 'Token=value')"
            ));
        }
    }
    Ok(())
}

fn build_structured_message(structured: &StructuredMessage) -> Result<String, i32> {
    let typ = structured
        .typ
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            eprintln!("error: --type must not be empty");
            EXIT_MESSAGE_REQUIRED
        })?;
    let subject = structured
        .subject
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            eprintln!("error: --subject must not be empty");
            EXIT_MESSAGE_REQUIRED
        })?;

    let typ = typ.to_ascii_lowercase();
    let header = match structured
        .scope
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(scope) => format!("{}({}): {subject}", typ, scope.to_ascii_lowercase()),
        None => format!("{typ}: {subject}"),
    };

    let mut lines = vec![header];
    if !structured.body_bullets.is_empty() {
        lines.push(String::new());
        for bullet in &structured.body_bullets {
            let trimmed = bullet.trim();
            if trimmed.is_empty() {
                eprintln!("error: --body-bullet must not be empty");
                return Err(EXIT_MESSAGE_REQUIRED);
            }
            let body = trimmed.strip_prefix("- ").unwrap_or(trimmed);
            let line = capitalize_bullet_first_char(&format!("- {body}"));
            lines.extend(wrap_body_line(&line, BODY_LINE_WIDTH));
        }
    }

    Ok(lines.join("\n"))
}

fn read_message_contents(options: &CommitOptions) -> Result<String, i32> {
    if options.no_edit {
        let Some(contents) = commit_message_for_rev(options.repo.as_deref(), "HEAD")? else {
            eprintln!("error: --no-edit requires an existing HEAD commit");
            return Err(EXIT_ERROR);
        };
        return Ok(contents);
    }

    if options.structured.has_any() {
        return build_structured_message(&options.structured);
    }

    let message_contents = match (&options.message, &options.message_file) {
        (Some(text), None) => text.clone(),
        (None, Some(path)) => match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(_) => {
                eprintln!("error: message file not found: {path}");
                return Err(EXIT_ERROR);
            }
        },
        (None, None) => {
            if options.automation {
                eprintln!(
                    "error: no commit message provided in automation mode (use --message or --message-file)"
                );
                return Err(EXIT_MESSAGE_REQUIRED);
            }

            if std::io::stdin().is_terminal() {
                eprintln!(
                    "error: no commit message provided (use stdin, --message, or --message-file)"
                );
                print_usage_stderr();
                return Err(EXIT_MESSAGE_REQUIRED);
            }

            let mut buf = String::new();
            if let Err(err) = std::io::stdin().read_to_string(&mut buf) {
                eprintln!("{err:#}");
                return Err(EXIT_ERROR);
            }
            buf
        }
        (Some(_), Some(_)) => unreachable!("validated above"),
    };

    if message_contents.trim().is_empty() {
        eprintln!("error: commit message is empty");
        return Err(EXIT_MESSAGE_REQUIRED);
    }

    Ok(message_contents)
}

fn run_git_output_with_pager(repo: Option<&Path>, args: &[&str]) -> std::io::Result<Output> {
    match repo {
        Some(repo) => common_git::run_output_in_with_env(repo, args, &CAT_PAGER_ENV),
        None => common_git::run_output_with_env(args, &CAT_PAGER_ENV),
    }
}

fn run_git_output_with_pager_owned(
    repo: Option<&Path>,
    args: &[String],
) -> std::io::Result<Output> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_git_output_with_pager(repo, &refs)
}

fn run_git_status_inherit_with_pager(
    repo: Option<&Path>,
    args: &[&str],
) -> std::io::Result<ExitStatus> {
    match repo {
        Some(repo) => common_git::run_status_inherit_in_with_env(repo, args, &CAT_PAGER_ENV),
        None => common_git::run_status_inherit_with_env(args, &CAT_PAGER_ENV),
    }
}

fn git_commit(
    message_path: &Path,
    options: &CommitOptions,
    operation: CommitOperation,
    force_signing: bool,
) -> anyhow::Result<std::process::ExitStatus> {
    let message_path = message_path.to_string_lossy();
    let mut args = vec!["commit".to_string()];
    if force_signing {
        args.push("-S".to_string());
    }
    if matches!(
        operation,
        CommitOperation::Amend | CommitOperation::MessageOnlyAmend
    ) {
        args.push("--amend".to_string());
    }
    if options.allow_empty {
        args.push("--allow-empty".to_string());
    }
    if options.signoff {
        args.push("--signoff".to_string());
    }
    for trailer in &options.trailers {
        args.push("--trailer".to_string());
        args.push(trailer.clone());
    }
    if options.no_edit {
        args.push("--no-edit".to_string());
    } else {
        args.push("-F".to_string());
        args.push(message_path.to_string());
    }

    let output = run_git_output_with_pager_owned(options.repo.as_deref(), &args)?;

    if !output.stderr.is_empty() {
        std::io::stderr().write_all(&output.stderr)?;
    }

    Ok(output.status)
}

fn git_cleanup_commit(
    options: &CleanupOptions,
    operation: CleanupOperation,
    target_sha: &str,
) -> anyhow::Result<std::process::ExitStatus> {
    let mut args = vec!["commit".to_string()];
    if options.allow_empty {
        args.push("--allow-empty".to_string());
    }
    args.push(format!("{}={target_sha}", operation.git_flag()));

    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = match options.repo.as_deref() {
        Some(repo) => common_git::run_output_in_with_env(repo, &refs, &NONINTERACTIVE_COMMIT_ENV),
        None => common_git::run_output_with_env(&refs, &NONINTERACTIVE_COMMIT_ENV),
    }?;

    if !output.stderr.is_empty() {
        std::io::stderr().write_all(&output.stderr)?;
    }

    Ok(output.status)
}

fn resolve_target_metadata(
    repo: Option<&Path>,
    target: Option<&str>,
) -> Result<CommitMetadata, i32> {
    let target = target.expect("target checked by parser");
    let rev = format!("{target}^{{commit}}");
    let Some(sha) = git_stdout(repo, &["rev-parse", "--verify", &rev]) else {
        eprintln!("error: target revision not found: {target}");
        return Err(EXIT_ERROR);
    };
    let Some(subject) = git_stdout(repo, &["show", "-s", "--format=%s", &sha]) else {
        eprintln!("error: failed to read target subject: {target}");
        return Err(EXIT_ERROR);
    };
    Ok(CommitMetadata { sha, subject })
}

fn current_head_metadata(repo: Option<&Path>) -> Option<CommitMetadata> {
    let sha = git_stdout(repo, &["rev-parse", "--verify", "HEAD"])?;
    let subject = git_stdout(repo, &["show", "-s", "--format=%s", "HEAD"])?;
    Some(CommitMetadata { sha, subject })
}

fn commit_message_for_rev(repo: Option<&Path>, rev: &str) -> Result<Option<String>, i32> {
    let output =
        run_git_output_with_pager(repo, &["log", "-1", "--format=%B", rev]).map_err(|err| {
            eprintln!("error: failed to read {rev} commit message: {err}");
            EXIT_ERROR
        })?;
    if output.status.success() {
        Ok(Some(String::from_utf8_lossy(&output.stdout).to_string()))
    } else {
        Ok(None)
    }
}

fn git_stdout(repo: Option<&Path>, args: &[&str]) -> Option<String> {
    let output = run_git_output_with_pager(repo, args).ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

fn ensure_expected_head(repo: Option<&Path>, expected: &str) -> Result<(), i32> {
    let Some(current) = git_stdout(repo, &["rev-parse", "--verify", "HEAD"]) else {
        eprintln!("error: --expect-head requires an existing HEAD commit");
        return Err(EXIT_ERROR);
    };
    let rev = format!("{expected}^{{commit}}");
    let Some(expected_sha) = git_stdout(repo, &["rev-parse", "--verify", &rev]) else {
        eprintln!("error: --expect-head revision not found: {expected}");
        return Err(EXIT_ERROR);
    };
    if current != expected_sha {
        eprintln!("error: HEAD mismatch (expected {expected_sha}, found {current})");
        return Err(EXIT_ERROR);
    }
    Ok(())
}

fn ensure_no_unstaged_or_untracked(repo: Option<&Path>) -> Result<(), i32> {
    if has_unstaged_or_untracked(repo)? {
        eprintln!("error: unstaged or untracked changes present");
        return Err(EXIT_ERROR);
    }
    Ok(())
}

fn has_unstaged_or_untracked(repo: Option<&Path>) -> Result<bool, i32> {
    let output =
        run_git_output_with_pager(repo, &["status", "--porcelain", "--untracked-files=all"])
            .map_err(|err| {
                eprintln!("error: failed to inspect worktree status: {err}");
                EXIT_ERROR
            })?;
    if !output.status.success() {
        eprintln!("error: failed to inspect worktree status");
        return Err(EXIT_ERROR);
    }
    let status = String::from_utf8_lossy(&output.stdout);
    Ok(status
        .lines()
        .any(|line| line.starts_with("??") || line.as_bytes().get(1).is_some_and(|b| *b != b' ')))
}

fn staged_entries(repo: Option<&Path>) -> anyhow::Result<Vec<StagedEntry>> {
    let output = run_git_output_with_pager(
        repo,
        &[
            "-c",
            "core.quotepath=false",
            "diff",
            "--cached",
            "--name-status",
            "-z",
        ],
    )?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    let entries =
        common_git::parse_name_status_z(&output.stdout).map_err(|err| anyhow::anyhow!("{err}"))?;
    Ok(entries
        .into_iter()
        .map(|entry| StagedEntry {
            status: String::from_utf8_lossy(entry.status_raw).to_string(),
            path: String::from_utf8_lossy(entry.path).to_string(),
            old_path: entry
                .old_path
                .map(|path| String::from_utf8_lossy(path).to_string()),
        })
        .collect())
}

fn print_commit_json_result(
    operation: CommitOperation,
    validate_only: bool,
    dry_run: bool,
    commit: Option<CommitMetadata>,
    target: Option<CommitMetadata>,
    staged_entries: Vec<StagedEntry>,
) {
    let files: Vec<_> = staged_entries
        .iter()
        .map(|entry| {
            json!({
                "status": entry.status,
                "path": entry.path,
                "old_path": entry.old_path,
            })
        })
        .collect();
    let commit_json = commit.map(|commit| {
        json!({
            "sha": commit.sha,
            "subject": commit.subject,
        })
    });
    let target_json = target.map(|target| {
        json!({
            "sha": target.sha,
            "subject": target.subject,
        })
    });
    let payload = json!({
        "schema_version": COMMIT_RESULT_SCHEMA_VERSION,
        "ok": true,
        "operation": operation.as_str(),
        "validate_only": validate_only,
        "dry_run": dry_run,
        "commit": commit_json,
        "target": target_json,
        "staged": {
            "file_count": files.len(),
            "files": files,
        },
    });
    println!("{payload}");
}

fn print_cleanup_json_result(
    operation: CleanupOperation,
    dry_run: bool,
    commit: Option<CommitMetadata>,
    target: CommitMetadata,
    staged_entries: Vec<StagedEntry>,
) {
    let generated_subject = format!("{} {}", operation.subject_prefix(), target.subject);
    let files: Vec<_> = staged_entries
        .iter()
        .map(|entry| {
            json!({
                "status": entry.status,
                "path": entry.path,
                "old_path": entry.old_path,
            })
        })
        .collect();
    let commit_json = commit.map(|commit| {
        json!({
            "sha": commit.sha,
            "subject": commit.subject,
        })
    });
    let payload = json!({
        "schema_version": COMMIT_RESULT_SCHEMA_VERSION,
        "ok": true,
        "operation": operation.as_str(),
        "validate_only": false,
        "dry_run": dry_run,
        "commit": commit_json,
        "target": {
            "sha": target.sha,
            "subject": target.subject,
        },
        "generated_subject": generated_subject,
        "staged": {
            "file_count": files.len(),
            "files": files,
        },
    });
    println!("{payload}");
}

fn print_summary(summary_mode: SummaryMode, repo: Option<&Path>) -> i32 {
    match summary_mode {
        SummaryMode::None => 0,
        SummaryMode::GitShow => print_git_show_summary(repo),
        SummaryMode::GitScope => {
            if run_git_scope_summary(repo) {
                0
            } else {
                eprintln!(
                    "warning: git-scope summary unavailable; falling back to git show --name-status"
                );
                print_git_show_summary(repo)
            }
        }
    }
}

fn run_git_scope_summary(repo: Option<&Path>) -> bool {
    let mut command = Command::new("git-scope");
    if let Some(repo) = repo {
        command.current_dir(repo);
    }

    let status = command
        .args(["commit", "HEAD", "--no-color"])
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    match status {
        Ok(status) if status.success() => true,
        Ok(status) => {
            let rc = status.code().unwrap_or(EXIT_ERROR);
            eprintln!("warning: git-scope commit failed (exit code: {rc})");
            false
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("warning: git-scope not found on PATH");
            false
        }
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("warning: git-scope is not executable");
            false
        }
        Err(err) => {
            eprintln!("warning: git-scope commit failed: {err}");
            false
        }
    }
}

fn print_git_show_summary(repo: Option<&Path>) -> i32 {
    let status = run_git_status_inherit_with_pager(
        repo,
        &["show", "-1", "--name-status", "--oneline", "--no-color"],
    );

    match status {
        Ok(status) if status.success() => 0,
        Ok(status) => {
            let rc = status.code().unwrap_or(EXIT_ERROR);
            eprintln!("error: git show summary failed (exit code: {rc})");
            rc
        }
        Err(err) => {
            eprintln!("error: git show summary failed: {err}");
            EXIT_ERROR
        }
    }
}

fn write_message_file(path: &Path, contents: &str) -> anyhow::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(contents.as_bytes())?;
    Ok(())
}

#[cfg(test)]
fn validate_commit_message(path: &Path) -> Result<(), i32> {
    validate_commit_message_with_width(path, DEFAULT_MAX_HEADER_WIDTH)
}

fn validate_commit_message_with_width(path: &Path, max_header_width: usize) -> Result<(), i32> {
    let file = File::open(path).map_err(|_| {
        eprintln!("error: commit message validation failed");
        EXIT_VALIDATION_FAILED
    })?;

    let reader = BufReader::new(file);
    let mut lines: Vec<String> = Vec::new();
    for line in reader.lines() {
        match line {
            Ok(line) => lines.push(line),
            Err(_) => {
                return fail_validation("commit message validation failed");
            }
        }
    }

    if lines.is_empty() {
        return fail_validation("commit message is empty");
    }

    let header = &lines[0];
    if header.is_empty() {
        return fail_validation("commit header is empty");
    }
    if header.chars().count() > max_header_width {
        return fail_validation(&format!(
            "commit header exceeds {max_header_width} characters (max {max_header_width})"
        ));
    }
    if !is_valid_header(header) {
        return fail_validation(
            "invalid header format (expected 'type(scope): subject' or 'type: subject' with lowercase type)",
        );
    }

    let body_exists = lines.iter().skip(1).any(|line| !line.is_empty());
    if body_exists {
        if lines.get(1).is_some_and(|line| !line.is_empty()) {
            return fail_validation("commit body must be separated from header by a blank line");
        }

        let mut prev_was_body_line = false;
        let mut trailer_mode = false;
        for (idx, line) in lines.iter().enumerate().skip(2) {
            let line_no = idx + 1;
            if line.is_empty() {
                if prev_was_body_line && !trailer_mode {
                    trailer_mode = true;
                    prev_was_body_line = false;
                    continue;
                }
                return fail_validation(&format!(
                    "commit body line {line_no} is empty; body lines must start with '- ' followed by uppercase letter, a trailer, or '  ' to continue the previous bullet"
                ));
            }
            if line.chars().count() > BODY_LINE_WIDTH {
                return fail_validation(&format!(
                    "commit body line {line_no} exceeds {BODY_LINE_WIDTH} characters (max {BODY_LINE_WIDTH})"
                ));
            }

            if trailer_mode {
                if !is_valid_trailer_line(line) {
                    return fail_validation(&format!(
                        "commit trailer line {line_no} must use 'Token: value' or 'Token=value'"
                    ));
                }
                continue;
            }

            if !prev_was_body_line && is_valid_trailer_line(line) {
                trailer_mode = true;
                continue;
            }

            let is_bullet = line.starts_with("- ")
                && line.chars().nth(2).is_some_and(|c| c.is_ascii_uppercase());
            let is_continuation = prev_was_body_line
                && line.starts_with("  ")
                && line.chars().nth(2).is_some_and(|c| !c.is_whitespace());

            if !is_bullet && !is_continuation {
                return fail_validation(&format!(
                    "commit body line {line_no} must start with '- ' followed by uppercase letter, a trailer, or '  ' to continue the previous bullet"
                ));
            }
            prev_was_body_line = true;
        }
    }

    Ok(())
}

fn fail_validation(message: &str) -> Result<(), i32> {
    eprintln!("error: {message}");
    Err(EXIT_VALIDATION_FAILED)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BlockedMessageRule {
    id: &'static str,
    reason: &'static str,
    matched_hint: &'static str,
    fix: &'static str,
    patterns: &'static [BlockedMessagePattern],
}

/// Blocked-line shapes. Both delegate to
/// [`nils_common::agent_attribution`] so the commit path and forge-cli's
/// provider path (Rule 17) reject the exact same attribution forms. Matching
/// here is verbatim — unlike the markdown payload scan, a commit message gets no
/// code-span exemption, so an attribution line cannot hide behind backticks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockedMessagePattern {
    AgentCoauthorTrailer,
    AgentGeneratorMarker,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockedMessageSource {
    MessageLine(usize),
    TrailerFlag(usize),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BlockedMessageMatch {
    source: BlockedMessageSource,
}

const CLAUDE_COAUTHOR_PATTERNS: &[BlockedMessagePattern] =
    &[BlockedMessagePattern::AgentCoauthorTrailer];

const CLAUDE_GENERATED_MARKER_PATTERNS: &[BlockedMessagePattern] =
    &[BlockedMessagePattern::AgentGeneratorMarker];

const BLOCKED_MESSAGE_RULES: &[BlockedMessageRule] = &[
    BlockedMessageRule {
        id: "claude-coauthor-trailer",
        reason: "do not attribute commits to any Claude model",
        matched_hint: "Co-Authored-By: Claude ...",
        fix: "remove the `Co-Authored-By: Claude ...` trailer",
        patterns: CLAUDE_COAUTHOR_PATTERNS,
    },
    BlockedMessageRule {
        id: "claude-generated-marker",
        reason: "do not advertise the generating agent in commit messages",
        matched_hint: "agent generator marker line",
        fix: "remove the generator marker line, including its claude-code link",
        patterns: CLAUDE_GENERATED_MARKER_PATTERNS,
    },
];

fn validate_blocked_message_rules(message: &str, trailers: &[String]) -> Result<(), i32> {
    for rule in BLOCKED_MESSAGE_RULES {
        if let Some(blocked) = find_blocked_message_match(rule, message, trailers) {
            return fail_validation(&format!(
                "commit message is blocked by rule `{}`\n  source: {}\n  matched: {}\n  rule: {}\n  fix: {}",
                rule.id,
                blocked_message_source_label(blocked.source),
                rule.matched_hint,
                rule.reason,
                rule.fix
            ));
        }
    }
    Ok(())
}

fn find_blocked_message_match<'a>(
    rule: &BlockedMessageRule,
    message: &'a str,
    trailers: &'a [String],
) -> Option<BlockedMessageMatch> {
    for (idx, line) in message.lines().enumerate() {
        if rule.patterns.iter().any(|pattern| pattern.matches(line)) {
            return Some(BlockedMessageMatch {
                source: BlockedMessageSource::MessageLine(idx + 1),
            });
        }
    }

    for (idx, trailer) in trailers.iter().enumerate() {
        if rule.patterns.iter().any(|pattern| pattern.matches(trailer)) {
            return Some(BlockedMessageMatch {
                source: BlockedMessageSource::TrailerFlag(idx + 1),
            });
        }
    }

    None
}

fn blocked_message_source_label(source: BlockedMessageSource) -> String {
    match source {
        BlockedMessageSource::MessageLine(line) => format!("message line {line}"),
        BlockedMessageSource::TrailerFlag(idx) => format!("--trailer #{idx}"),
    }
}

impl BlockedMessagePattern {
    fn matches(self, line: &str) -> bool {
        match self {
            Self::AgentCoauthorTrailer => agent_attribution::line_is_blocked_coauthor_trailer(line),
            Self::AgentGeneratorMarker => agent_attribution::line_has_generator_marker(line),
        }
    }
}

fn is_valid_header(header: &str) -> bool {
    // Regex parity: ^[a-z][a-z0-9-]*(\([a-z0-9._-]+\))?: .+$
    let Some((prefix, subject)) = header.split_once(": ") else {
        return false;
    };
    if subject.is_empty() {
        return false;
    }

    let (typ, scope) = if let Some((t, rest)) = prefix.split_once('(') {
        let Some(scope_end) = rest.strip_suffix(')') else {
            return false;
        };
        (t, Some(scope_end))
    } else {
        (prefix, None)
    };

    let mut chars = typ.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return false;
    }

    if let Some(scope) = scope {
        if scope.is_empty() {
            return false;
        }
        if !scope.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' || c == '-'
        }) {
            return false;
        }
    }

    true
}

fn is_valid_trailer_line(line: &str) -> bool {
    let Some((token, value)) = split_trailer(line) else {
        return false;
    };
    !token.is_empty()
        && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !value.trim().is_empty()
}

fn split_trailer(line: &str) -> Option<(&str, &str)> {
    let colon = line.find(':');
    let equals = line.find('=');
    let idx = match (colon, equals) {
        (Some(colon), Some(equals)) => colon.min(equals),
        (Some(colon), None) => colon,
        (None, Some(equals)) => equals,
        (None, None) => return None,
    };
    let (token, rest) = line.split_at(idx);
    Some((token.trim(), rest[1..].trim()))
}

const DEFAULT_MAX_HEADER_WIDTH: usize = 100;
const BODY_LINE_WIDTH: usize = 100;

fn normalize_message(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    if lines.is_empty() {
        return input.to_string();
    }

    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 2);
    out.push(normalize_header(lines[0]));

    let body: Vec<&str> = lines
        .iter()
        .skip(1)
        .filter(|line| !line.is_empty())
        .copied()
        .collect();

    if !body.is_empty() {
        out.push(String::new());
        for line in body {
            let cased = capitalize_bullet_first_char(line);
            for wrapped in wrap_body_line(&cased, BODY_LINE_WIDTH) {
                out.push(wrapped);
            }
        }
    }

    let mut result = out.join("\n");
    if input.ends_with('\n') {
        result.push('\n');
    }
    result
}

fn normalize_header(header: &str) -> String {
    let Some((prefix, subject)) = header.split_once(": ") else {
        return header.to_string();
    };

    let (typ, scope) = if let Some((t, rest)) = prefix.split_once('(') {
        let Some(scope_inner) = rest.strip_suffix(')') else {
            return header.to_string();
        };
        (t, Some(scope_inner))
    } else {
        (prefix, None)
    };

    let prefix_norm = match scope {
        Some(s) => format!("{}({})", typ.to_ascii_lowercase(), s.to_ascii_lowercase()),
        None => typ.to_ascii_lowercase(),
    };

    format!("{prefix_norm}: {subject}")
}

fn capitalize_bullet_first_char(line: &str) -> String {
    if !line.starts_with("- ") {
        return line.to_string();
    }
    let mut chars = line.chars();
    let _ = chars.next();
    let _ = chars.next();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {
            let rest: String = chars.collect();
            format!("- {}{rest}", c.to_ascii_uppercase())
        }
        _ => line.to_string(),
    }
}

fn wrap_body_line(line: &str, max: usize) -> Vec<String> {
    if line.chars().count() <= max {
        return vec![line.to_string()];
    }

    let (first_prefix, cont_prefix, content): (&str, &str, String) = if line.starts_with("- ") {
        ("- ", "  ", line.chars().skip(2).collect())
    } else if line.starts_with("  ") {
        ("  ", "  ", line.chars().skip(2).collect())
    } else {
        ("", "", line.to_string())
    };

    let chars: Vec<char> = content.chars().collect();
    let first_budget = max.saturating_sub(first_prefix.chars().count());
    let cont_budget = max.saturating_sub(cont_prefix.chars().count());

    if first_budget == 0 || cont_budget == 0 {
        return vec![line.to_string()];
    }

    let mut result: Vec<String> = Vec::new();
    let mut idx = 0;
    let mut is_first = true;

    while idx < chars.len() {
        let budget = if is_first { first_budget } else { cont_budget };
        let prefix = if is_first { first_prefix } else { cont_prefix };
        let window_end = (idx + budget).min(chars.len());

        let break_at = if window_end == chars.len() {
            window_end
        } else {
            (idx..window_end)
                .rev()
                .find(|&i| chars[i] == ' ' && i > idx)
                .unwrap_or(window_end)
        };

        let chunk: String = chars[idx..break_at].iter().collect();
        result.push(format!("{prefix}{}", chunk.trim_end()));

        idx = if break_at < chars.len() && chars[break_at] == ' ' {
            let mut next = break_at + 1;
            while next < chars.len() && chars[next] == ' ' {
                next += 1;
            }
            next
        } else {
            break_at
        };
        is_first = false;
    }

    if result.is_empty() {
        result.push(first_prefix.to_string());
    }

    result
}

fn print_usage_stdout() {
    print_usage(false);
}

fn print_usage_stderr() {
    print_usage(true);
}

fn print_usage(stderr: bool) {
    let out: &mut dyn std::io::Write = if stderr {
        &mut std::io::stderr()
    } else {
        &mut std::io::stdout()
    };

    let _ = writeln!(out, "Usage:");
    let _ = writeln!(
        out,
        "  semantic-commit commit [--message <text>|--message-file <path>] [options]"
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "Options:");
    let _ = writeln!(out, "  -m, --message <text>         Commit message text");
    let _ = writeln!(
        out,
        "  -F, --message-file <path>    Read commit message from file"
    );
    let _ = writeln!(
        out,
        "      --message-out <path>     Save prepared message for recovery"
    );
    let _ = writeln!(
        out,
        "      --summary <mode>         Summary mode: git-scope | git-show | none"
    );
    let _ = writeln!(
        out,
        "      --no-summary             Equivalent to --summary none"
    );
    let _ = writeln!(
        out,
        "      --format <mode>          Output mode: text | json"
    );
    let _ = writeln!(
        out,
        "      --json                   Equivalent to --format json"
    );
    let _ = writeln!(
        out,
        "      --repo <path>            Run git commands against repo path"
    );
    let _ = writeln!(
        out,
        "      --max-header-width <N>   Override commit header width (default: {DEFAULT_MAX_HEADER_WIDTH}; env: SEMANTIC_COMMIT_HEADER_WIDTH)"
    );
    let _ = writeln!(
        out,
        "      --automation             Disallow stdin message fallback"
    );
    let _ = writeln!(
        out,
        "      --validate-only          Validate message format only"
    );
    let _ = writeln!(
        out,
        "      --dry-run                Validate + staged checks, skip git commit"
    );
    let _ = writeln!(
        out,
        "      --auto-fix               Normalize body wrap, bullet/type/scope case before validation"
    );
    let _ = writeln!(
        out,
        "      --amend                  Amend HEAD instead of creating a new commit"
    );
    let _ = writeln!(
        out,
        "      --no-edit                Reuse HEAD message with --amend"
    );
    let _ = writeln!(
        out,
        "      --message-only           Amend only the HEAD message and require no staged changes"
    );
    let _ = writeln!(
        out,
        "      --allow-empty            Allow a commit operation without staged changes"
    );
    let _ = writeln!(
        out,
        "      --require-clean          Require no unstaged or untracked changes"
    );
    let _ = writeln!(
        out,
        "      --no-unstaged            Alias for --require-clean"
    );
    let _ = writeln!(
        out,
        "      --expect-head <rev>      Require HEAD to match rev before committing"
    );
    let _ = writeln!(
        out,
        "      --signoff                Pass --signoff to git commit"
    );
    let _ = writeln!(
        out,
        "      --trailer <token: value> Add a git trailer (repeatable)"
    );
    let _ = writeln!(
        out,
        "      --type <type>            Structured message type"
    );
    let _ = writeln!(
        out,
        "      --scope <scope>          Structured message scope"
    );
    let _ = writeln!(
        out,
        "      --subject <subject>      Structured message subject"
    );
    let _ = writeln!(
        out,
        "      --body-bullet <text>     Structured message body bullet (repeatable)"
    );
    let _ = writeln!(
        out,
        "      --no-progress            Disable progress spinner"
    );
    let _ = writeln!(
        out,
        "      --quiet                  Suppress progress and summary output"
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "Examples:");
    let _ = writeln!(out, "  cat <<'MSG' | semantic-commit commit");
    let _ = writeln!(out, "  feat(core): add thing");
    let _ = writeln!(out);
    let _ = writeln!(out, "  - Add thing");
    let _ = writeln!(out, "  MSG");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "  semantic-commit commit -F ./message.txt --summary git-show"
    );
}

fn print_cleanup_usage_stdout(operation: CleanupOperation) {
    print_cleanup_usage(operation, false);
}

fn print_cleanup_usage_stderr(operation: CleanupOperation) {
    print_cleanup_usage(operation, true);
}

fn print_cleanup_usage(operation: CleanupOperation, stderr: bool) {
    let out: &mut dyn std::io::Write = if stderr {
        &mut std::io::stderr()
    } else {
        &mut std::io::stdout()
    };
    let name = operation.as_str();
    let _ = writeln!(out, "Usage:");
    let _ = writeln!(out, "  semantic-commit {name} --target <rev> [options]");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Create a {}! commit for staged changes.",
        operation.as_str()
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "Options:");
    let _ = writeln!(out, "      --target <rev>          Target commit revision");
    let _ = writeln!(
        out,
        "      --summary <mode>        Summary mode: git-scope | git-show | none"
    );
    let _ = writeln!(
        out,
        "      --no-summary            Equivalent to --summary none"
    );
    let _ = writeln!(
        out,
        "      --format <mode>         Output mode: text | json"
    );
    let _ = writeln!(
        out,
        "      --json                  Equivalent to --format json"
    );
    let _ = writeln!(
        out,
        "      --dry-run               Validate target and staged checks without committing"
    );
    let _ = writeln!(
        out,
        "      --allow-empty           Allow a cleanup commit without staged changes"
    );
    let _ = writeln!(
        out,
        "      --require-clean         Require no unstaged or untracked changes"
    );
    let _ = writeln!(
        out,
        "      --no-unstaged           Alias for --require-clean"
    );
    let _ = writeln!(
        out,
        "      --expect-head <rev>     Require HEAD to match rev before committing"
    );
    let _ = writeln!(
        out,
        "      --repo <path>           Run git commands against repo path"
    );
    let _ = writeln!(
        out,
        "      --no-progress           Disable progress spinner"
    );
    let _ = writeln!(
        out,
        "      --quiet                 Suppress progress and summary output"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn message_file(contents: &str) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().expect("create temp message file");
        file.write_all(contents.as_bytes())
            .expect("write temp message file");
        file
    }

    #[test]
    fn summary_mode_parse_supports_known_values() {
        assert_eq!(SummaryMode::parse("git-scope"), Some(SummaryMode::GitScope));
        assert_eq!(SummaryMode::parse("git-show"), Some(SummaryMode::GitShow));
        assert_eq!(SummaryMode::parse("none"), Some(SummaryMode::None));
        assert_eq!(SummaryMode::parse("other"), None);
    }

    #[test]
    fn validate_commit_message_accepts_valid_header_without_body() {
        let file = message_file("feat: add parser coverage\n");
        assert!(validate_commit_message(file.path()).is_ok());
    }

    #[test]
    fn validate_commit_message_accepts_valid_scoped_header_with_body() {
        let file = message_file("fix(parser-2): handle edge case\n\n- Handle malformed input\n");
        assert!(validate_commit_message(file.path()).is_ok());
    }

    #[test]
    fn validate_commit_message_rejects_header_over_100_chars() {
        let subject = "a".repeat(95);
        let file = message_file(&format!("feat: {subject}\n"));

        assert_eq!(
            validate_commit_message(file.path()),
            Err(EXIT_VALIDATION_FAILED)
        );
    }

    #[test]
    fn validate_commit_message_rejects_uppercase_scope() {
        let file = message_file("feat(Core): add parser coverage\n");
        assert_eq!(
            validate_commit_message(file.path()),
            Err(EXIT_VALIDATION_FAILED)
        );
    }

    #[test]
    fn validate_commit_message_rejects_empty_line_inside_body() {
        let file = message_file("feat: add parser coverage\n\n- First line\n\n- Second line\n");
        assert_eq!(
            validate_commit_message(file.path()),
            Err(EXIT_VALIDATION_FAILED)
        );
    }

    #[test]
    fn validate_commit_message_rejects_body_line_over_100_chars() {
        let long_line = "A".repeat(99);
        let file = message_file(&format!("feat: add parser coverage\n\n- {long_line}\n"));

        assert_eq!(
            validate_commit_message(file.path()),
            Err(EXIT_VALIDATION_FAILED)
        );
    }

    #[test]
    fn is_valid_header_enforces_shape_rules() {
        assert!(is_valid_header("fix(core_2): handle edge case"));
        assert!(is_valid_header("chore: update fixtures"));
        assert!(!is_valid_header("Fix: uppercase type"));
        assert!(!is_valid_header("fix(core!): invalid scope character"));
        assert!(!is_valid_header("fix(scope):"));
        assert!(!is_valid_header("fix(scope): "));
        assert!(!is_valid_header("fix(scope) missing colon"));
    }

    #[test]
    fn normalize_header_lowercases_type_and_scope() {
        assert_eq!(
            normalize_header("Feat(Core): add thing"),
            "feat(core): add thing"
        );
        assert_eq!(normalize_header("FIX: bug"), "fix: bug");
    }

    #[test]
    fn normalize_header_preserves_subject_case() {
        assert_eq!(
            normalize_header("feat(core): Add THING with MixedCase"),
            "feat(core): Add THING with MixedCase"
        );
    }

    #[test]
    fn normalize_header_passes_through_invalid_shape() {
        assert_eq!(normalize_header("no colon here"), "no colon here");
        assert_eq!(
            normalize_header("feat(unclosed: subj"),
            "feat(unclosed: subj"
        );
    }

    #[test]
    fn capitalize_bullet_first_char_upcases_lowercase_ascii() {
        assert_eq!(capitalize_bullet_first_char("- add thing"), "- Add thing");
    }

    #[test]
    fn capitalize_bullet_first_char_leaves_already_uppercase() {
        assert_eq!(capitalize_bullet_first_char("- Add thing"), "- Add thing");
    }

    #[test]
    fn capitalize_bullet_first_char_leaves_non_bullets() {
        assert_eq!(capitalize_bullet_first_char("  cont"), "  cont");
        assert_eq!(capitalize_bullet_first_char("plain"), "plain");
        assert_eq!(
            capitalize_bullet_first_char("- 1.0 release"),
            "- 1.0 release"
        );
    }

    #[test]
    fn wrap_body_line_short_unchanged() {
        assert_eq!(
            wrap_body_line("- Short bullet", 100),
            vec!["- Short bullet"]
        );
    }

    #[test]
    fn wrap_body_line_breaks_bullet_at_last_space() {
        let line = format!("- {}", "word ".repeat(30).trim_end());
        let out = wrap_body_line(&line, 40);
        assert!(out.len() > 1, "expected wrapping, got {out:?}");
        assert!(out[0].starts_with("- "));
        for cont in &out[1..] {
            assert!(
                cont.starts_with("  "),
                "continuation must start with two spaces: {cont:?}"
            );
            assert!(
                cont.chars().nth(2).is_some_and(|c| !c.is_whitespace()),
                "continuation third char must be non-whitespace: {cont:?}"
            );
        }
        for l in &out {
            assert!(l.chars().count() <= 40, "line exceeds budget: {l:?}");
        }
    }

    #[test]
    fn wrap_body_line_continuation_keeps_two_space_prefix() {
        let line = format!("  {}", "word ".repeat(30).trim_end());
        let out = wrap_body_line(&line, 40);
        for cont in &out {
            assert!(
                cont.starts_with("  "),
                "expected two-space prefix: {cont:?}"
            );
        }
    }

    #[test]
    fn wrap_body_line_hard_breaks_when_no_whitespace() {
        let line = format!("- {}", "a".repeat(120));
        let out = wrap_body_line(&line, 100);
        assert!(out.len() >= 2, "expected hard break, got {out:?}");
        for l in &out {
            assert!(l.chars().count() <= 100, "line exceeds budget: {l:?}");
        }
    }

    #[test]
    fn wrap_body_line_handles_cjk_codepoint_break() {
        let line = format!("- {}", "字".repeat(110));
        let out = wrap_body_line(&line, 100);
        assert!(out.len() >= 2, "expected wrap for long CJK, got {out:?}");
        for l in &out {
            assert!(l.chars().count() <= 100, "line exceeds budget: {l:?}");
        }
    }

    #[test]
    fn normalize_message_inserts_missing_blank_separator() {
        let out = normalize_message("feat: add thing\n- Add thing\n");
        assert_eq!(out, "feat: add thing\n\n- Add thing\n");
    }

    #[test]
    fn normalize_message_drops_empty_body_lines() {
        let out = normalize_message("feat: add thing\n\n- First\n\n- Second\n");
        assert_eq!(out, "feat: add thing\n\n- First\n- Second\n");
    }

    #[test]
    fn normalize_message_passes_through_already_valid() {
        let input = "feat(core): add thing\n\n- Add thing\n";
        assert_eq!(normalize_message(input), input);
    }

    #[test]
    fn normalize_message_does_not_truncate_overlength_header() {
        let header = format!("feat: {}", "a".repeat(120));
        let input = format!("{header}\n");
        let out = normalize_message(&input);
        let header_line = out.lines().next().unwrap();
        assert_eq!(header_line.chars().count(), header.chars().count());
    }

    #[test]
    fn normalize_message_makes_invalid_input_pass_validator() {
        let input = "Feat(Core): subject\n- lowercase bullet that is way too long because it has many words and exceeds the limit by a lot of chars\n";
        let normalized = normalize_message(input);
        let file = message_file(&normalized);
        assert!(
            validate_commit_message(file.path()).is_ok(),
            "normalized message failed validation:\n{normalized}"
        );
    }
}
