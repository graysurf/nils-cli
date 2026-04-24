#![allow(dead_code)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use agent_docs::env::ResolvedRoots;
use agent_docs::model::{Context, OutputFormat};
use nils_test_support::{cmd, fs as test_fs};

static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

pub struct FixtureWorkspace {
    _temp: TestTempDir,
    pub root: PathBuf,
    pub agent_home: PathBuf,
    pub project_path: PathBuf,
}

impl FixtureWorkspace {
    pub fn from_fixtures() -> Self {
        let temp = TestTempDir::new("agent-docs-resolve-builtin");
        let root = temp.path().to_path_buf();
        let agent_home = root.join("agent-home");
        let project_path = root.join("project");

        copy_fixture_tree(&fixture_path("home"), &agent_home);
        copy_fixture_tree(&fixture_path("project"), &project_path);
        ensure_agents_fixture_docs(&agent_home, &project_path);

        Self {
            _temp: temp,
            root,
            agent_home,
            project_path,
        }
    }

    pub fn roots(&self) -> ResolvedRoots {
        ResolvedRoots {
            agent_home: self.agent_home.clone(),
            project_path: self.project_path.clone(),
            is_linked_worktree: false,
            git_common_dir: None,
            primary_worktree_path: None,
        }
    }
}

impl Default for FixtureWorkspace {
    fn default() -> Self {
        Self::from_fixtures()
    }
}

pub fn fixture_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(relative)
}

pub fn remove_file_if_exists(path: &Path) {
    if path.exists() {
        fs::remove_file(path).expect("remove file");
    }
}

pub fn run_resolve_exit_code(
    workspace: &FixtureWorkspace,
    context: Context,
    format: OutputFormat,
    strict: bool,
) -> i32 {
    let mut args: Vec<OsString> = vec![
        OsString::from("agent-docs"),
        OsString::from("resolve"),
        OsString::from("--context"),
        OsString::from(context.as_str()),
        OsString::from("--format"),
        OsString::from(format.as_str()),
        OsString::from("--agent-home"),
        workspace.agent_home.as_os_str().to_owned(),
        OsString::from("--project-path"),
        workspace.project_path.as_os_str().to_owned(),
    ];

    if strict {
        args.push(OsString::from("--strict"));
    }

    agent_docs::run_with_args(args)
}

pub fn required_lines(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| line.starts_with("[required]"))
        .collect()
}

#[derive(Debug)]
pub struct ChecklistBegin<'a> {
    pub context: &'a str,
    pub mode: &'a str,
}

#[derive(Debug)]
pub struct ChecklistDoc<'a> {
    pub file_name: &'a str,
    pub status: &'a str,
    pub path: &'a str,
}

#[derive(Debug)]
pub struct ChecklistEnd<'a> {
    pub required: usize,
    pub present: usize,
    pub missing: usize,
    pub mode: &'a str,
    pub context: &'a str,
}

#[derive(Debug)]
pub struct ParsedChecklist<'a> {
    pub begin: ChecklistBegin<'a>,
    pub docs: Vec<ChecklistDoc<'a>>,
    pub end: ChecklistEnd<'a>,
}

pub fn parse_checklist(output: &str) -> ParsedChecklist<'_> {
    let lines: Vec<&str> = output.lines().collect();
    assert!(
        lines.len() >= 2,
        "checklist output requires at least begin/end markers:\n{output}"
    );

    let begin = parse_begin_line(lines[0]);
    let end = parse_end_line(lines.last().expect("last line"));
    let docs = lines[1..lines.len() - 1]
        .iter()
        .map(|line| parse_doc_line(line))
        .collect();

    ParsedChecklist { begin, docs, end }
}

#[derive(Debug)]
pub struct CliOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CliOutput {
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
}

pub fn run_agent_docs_command(workspace: &FixtureWorkspace, args: &[&str]) -> CliOutput {
    let agent_home = workspace
        .agent_home
        .to_str()
        .expect("fixture agent_home path should be utf-8");
    let project_path = workspace
        .project_path
        .to_str()
        .expect("fixture project_path path should be utf-8");

    let mut full_args = vec!["--agent-home", agent_home, "--project-path", project_path];
    full_args.extend_from_slice(args);

    let output = cmd::run_resolved("agent-docs", &full_args, &cmd::CmdOptions::default());
    CliOutput {
        exit_code: output.code,
        stdout: output.stdout_text(),
        stderr: output.stderr_text(),
    }
}

pub fn write_text(path: &Path, body: &str) {
    let _ = test_fs::write_text(path, body);
}

fn parse_begin_line(line: &str) -> ChecklistBegin<'_> {
    let payload = line
        .strip_prefix("REQUIRED_DOCS_BEGIN ")
        .expect("begin marker should start with REQUIRED_DOCS_BEGIN");
    let context = parse_kv(payload, "context").expect("begin marker should include context");
    let mode = parse_kv(payload, "mode").expect("begin marker should include mode");

    ChecklistBegin { context, mode }
}

fn parse_doc_line(line: &str) -> ChecklistDoc<'_> {
    let (file_name, remainder) = line
        .split_once(" status=")
        .expect("doc line should include status");
    let (status, path_payload) = remainder
        .split_once(" path=")
        .expect("doc line should include path");

    ChecklistDoc {
        file_name,
        status,
        path: path_payload,
    }
}

fn parse_end_line(line: &str) -> ChecklistEnd<'_> {
    let payload = line
        .strip_prefix("REQUIRED_DOCS_END ")
        .expect("end marker should start with REQUIRED_DOCS_END");

    let required = parse_kv(payload, "required")
        .expect("end marker should include required")
        .parse::<usize>()
        .expect("required should be usize");
    let present = parse_kv(payload, "present")
        .expect("end marker should include present")
        .parse::<usize>()
        .expect("present should be usize");
    let missing = parse_kv(payload, "missing")
        .expect("end marker should include missing")
        .parse::<usize>()
        .expect("missing should be usize");
    let mode = parse_kv(payload, "mode").expect("end marker should include mode");
    let context = parse_kv(payload, "context").expect("end marker should include context");

    ChecklistEnd {
        required,
        present,
        missing,
        mode,
        context,
    }
}

fn parse_kv<'a>(payload: &'a str, key: &str) -> Option<&'a str> {
    payload
        .split_whitespace()
        .find_map(|part| part.strip_prefix(&format!("{key}=")))
}

fn copy_fixture_tree(source: &Path, destination: &Path) {
    assert!(
        source.is_dir(),
        "fixture source missing: {}",
        source.display()
    );
    fs::create_dir_all(destination).expect("create destination fixture directory");

    let mut entries: Vec<_> = fs::read_dir(source)
        .expect("read fixture directory")
        .map(|entry| entry.expect("fixture entry"))
        .collect();
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type().expect("fixture file type");
        if file_type.is_dir() {
            copy_fixture_tree(&source_path, &destination_path);
        } else if file_type.is_file() {
            fs::copy(&source_path, &destination_path).expect("copy fixture file");
        }
    }
}

fn ensure_agents_fixture_docs(agent_home: &Path, project_path: &Path) {
    ensure_text_file(
        &agent_home.join("AGENTS.md"),
        "# Fixture: home AGENTS default\n\nid: fixture-home-agents-default\n",
    );
    ensure_text_file(
        &agent_home.join("AGENTS.override.md"),
        "# Fixture: home AGENTS override\n\nid: fixture-home-agents-override\n",
    );
    ensure_text_file(
        &project_path.join("AGENTS.md"),
        "# Fixture: project AGENTS default\n\nid: fixture-project-agents-default\n",
    );
    ensure_text_file(
        &project_path.join("AGENTS.override.md"),
        "# Fixture: project AGENTS override\n\nid: fixture-project-agents-override\n",
    );
}

fn ensure_text_file(path: &Path, body: &str) {
    if path.exists() {
        return;
    }
    write_text(path, body);
}

struct TestTempDir {
    path: PathBuf,
}

impl TestTempDir {
    fn new(prefix: &str) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let sequence = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dirname = format!("{prefix}-{}-{timestamp}-{sequence}", std::process::id());
        let path = std::env::temp_dir().join(dirname);
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestTempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
