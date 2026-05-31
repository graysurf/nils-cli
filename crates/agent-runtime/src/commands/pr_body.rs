use clap::{Args, Subcommand, ValueEnum};
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

const DEFAULT_RISK_NOTES: &str = "- N/A";

#[derive(Args, Debug)]
pub struct PrBodyArgs {
    #[command(subcommand)]
    pub command: PrBodyCommand,
}

#[derive(Subcommand, Debug)]
pub enum PrBodyCommand {
    /// Render a forge-cli-compatible PR / MR body.
    Render(PrBodyRenderArgs),
}

#[derive(Args, Debug)]
pub struct PrBodyRenderArgs {
    /// Body kind. `feature` and `bug` render their dedicated templates;
    /// `chore`, `docs`, `ci`, and `refactor` render a generic
    /// Summary / Test-First / Test plan / Risk skeleton. The set matches the
    /// six kinds `forge-cli pr deliver --kind` accepts.
    #[arg(long, value_enum)]
    pub kind: PrBodyKind,
    /// One-paragraph summary of the change and scope.
    #[arg(long)]
    pub summary_file: PathBuf,
    /// Feature-only list of key changes.
    #[arg(long, required_if_eq("kind", "feature"))]
    pub changes_file: Option<PathBuf>,
    /// Bug-only expected/actual/impact section.
    #[arg(long, required_if_eq("kind", "bug"))]
    pub problem_file: Option<PathBuf>,
    /// Bug-only reproduction steps.
    #[arg(long, required_if_eq("kind", "bug"))]
    pub reproduction_file: Option<PathBuf>,
    /// Bug-only issue table or issue list.
    #[arg(long, required_if_eq("kind", "bug"))]
    pub issues_file: Option<PathBuf>,
    /// Bug-only fix approach summary.
    #[arg(long, required_if_eq("kind", "bug"))]
    pub fix_approach_file: Option<PathBuf>,
    /// Test-first evidence, including the waiver when a failing test was not practical.
    #[arg(long)]
    pub test_first_file: PathBuf,
    /// Validation commands and results. Rendered as `## Test plan` for forge-cli.
    #[arg(long)]
    pub test_plan_file: PathBuf,
    /// Optional risk notes. Defaults to `- N/A`.
    #[arg(long)]
    pub risk_file: Option<PathBuf>,
    /// Output path. Defaults to stdout.
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum PrBodyKind {
    Feature,
    Bug,
    Chore,
    Docs,
    Ci,
    Refactor,
}

pub fn run(args: PrBodyArgs) -> anyhow::Result<u8> {
    match args.command {
        PrBodyCommand::Render(render_args) => render(render_args),
    }
}

fn render(args: PrBodyRenderArgs) -> anyhow::Result<u8> {
    let summary = read_required("summary", &args.summary_file)?;
    let test_first = read_required("test-first", &args.test_first_file)?;
    let test_plan = read_required("test-plan", &args.test_plan_file)?;
    let risk = match args.risk_file.as_deref() {
        Some(path) => read_required("risk", path)?,
        None => DEFAULT_RISK_NOTES.to_string(),
    };

    let body = match args.kind {
        PrBodyKind::Feature => {
            let changes = read_required(
                "changes",
                required_path("changes", args.changes_file.as_deref())?,
            )?;
            render_feature(&summary, &changes, &test_first, &test_plan, &risk)
        }
        PrBodyKind::Bug => {
            let problem = read_required(
                "problem",
                required_path("problem", args.problem_file.as_deref())?,
            )?;
            let reproduction = read_required(
                "reproduction",
                required_path("reproduction", args.reproduction_file.as_deref())?,
            )?;
            let issues = read_required(
                "issues",
                required_path("issues", args.issues_file.as_deref())?,
            )?;
            let fix_approach = read_required(
                "fix-approach",
                required_path("fix-approach", args.fix_approach_file.as_deref())?,
            )?;
            render_bug(
                &summary,
                &problem,
                &reproduction,
                &issues,
                &fix_approach,
                &test_first,
                &test_plan,
                &risk,
            )
        }
        PrBodyKind::Chore | PrBodyKind::Docs | PrBodyKind::Ci | PrBodyKind::Refactor => {
            render_generic(&summary, &test_first, &test_plan, &risk)
        }
    };

    validate_forge_sections(&body)?;
    write_output(args.out.as_deref(), &body)?;
    Ok(0)
}

fn read_required(label: &str, path: &Path) -> anyhow::Result<String> {
    let raw = fs::read_to_string(path).map_err(|err| {
        anyhow::anyhow!("failed to read --{label}-file {}: {err}", path.display())
    })?;
    let body = raw.trim();
    if body.is_empty() {
        anyhow::bail!("--{label}-file {} is empty", path.display());
    }
    Ok(body.to_string())
}

fn required_path<'a>(label: &str, path: Option<&'a Path>) -> anyhow::Result<&'a Path> {
    path.ok_or_else(|| anyhow::anyhow!("--{label}-file is required"))
}

fn render_feature(
    summary: &str,
    changes: &str,
    test_first: &str,
    test_plan: &str,
    risk: &str,
) -> String {
    format!(
        "## Summary\n\n{summary}\n\n## Changes\n\n{changes}\n\n## Test-First Evidence\n\n{test_first}\n\n## Test plan\n\n{test_plan}\n\n## Risk / Notes\n\n{risk}\n"
    )
}

/// Generic skeleton for kinds without a dedicated template
/// (`chore` / `docs` / `ci` / `refactor`). Emits the forge-cli-required
/// `## Summary` and `## Test plan` sections plus test-first evidence and
/// risk notes, with no kind-specific sections.
fn render_generic(summary: &str, test_first: &str, test_plan: &str, risk: &str) -> String {
    format!(
        "## Summary\n\n{summary}\n\n## Test-First Evidence\n\n{test_first}\n\n## Test plan\n\n{test_plan}\n\n## Risk / Notes\n\n{risk}\n"
    )
}

#[allow(clippy::too_many_arguments)]
fn render_bug(
    summary: &str,
    problem: &str,
    reproduction: &str,
    issues: &str,
    fix_approach: &str,
    test_first: &str,
    test_plan: &str,
    risk: &str,
) -> String {
    format!(
        "## Summary\n\n{summary}\n\n## Problem\n\n{problem}\n\n## Reproduction\n\n{reproduction}\n\n## Issues Found\n\n{issues}\n\n## Fix Approach\n\n{fix_approach}\n\n## Test-First Evidence\n\n{test_first}\n\n## Test plan\n\n{test_plan}\n\n## Risk / Notes\n\n{risk}\n"
    )
}

fn validate_forge_sections(body: &str) -> anyhow::Result<()> {
    for heading in ["## Summary", "## Test plan"] {
        if !has_non_empty_h2_section(body, heading) {
            anyhow::bail!("rendered body is missing non-empty {heading}");
        }
    }
    Ok(())
}

fn has_non_empty_h2_section(body: &str, heading: &str) -> bool {
    let mut in_section = false;
    for line in body.lines() {
        if line.starts_with("## ") {
            if in_section {
                return false;
            }
            in_section = line.trim_end() == heading;
            continue;
        }
        if in_section && !line.trim().is_empty() {
            return true;
        }
    }
    false
}

fn write_output(out: Option<&Path>, body: &str) -> anyhow::Result<()> {
    match out {
        Some(path) => {
            fs::write(path, body).map_err(|err| {
                anyhow::anyhow!("failed to write --out {}: {err}", path.display())
            })?;
        }
        None => {
            let mut stdout = io::stdout().lock();
            stdout.write_all(body.as_bytes())?;
        }
    }
    Ok(())
}
