use crate::change::parse_name_status_lines;
use crate::print::{HeadFallback, PrintSource, emit_file};
use crate::progress::ProgressRunner;
use crate::tree::{TREE_MISSING_WARNING, TREE_UNSUPPORTED_WARNING, tree_support};
use anyhow::Result;
use nils_common::shell::{AnsiStripMode, strip_ansi as strip_ansi_impl};
use std::collections::BTreeSet;
use std::process::Command;

#[derive(Debug, Clone, Copy)]
pub enum PrintMode {
    Worktree,
    Index,
}

pub fn render_with_type(
    lines: &[String],
    no_color: bool,
    print_mode: PrintMode,
    print: bool,
    progress_opt_in: bool,
) -> Result<Vec<String>> {
    if lines.is_empty() {
        println!("⚠️  No matching files");
        return Ok(Vec::new());
    }

    println!();
    println!("📄 Changed files:");

    let mut files: Vec<String> = Vec::new();

    for entry in parse_name_status_lines(lines) {
        let display_path = entry.display_path();
        let file_path = entry.file_path();

        files.push(file_path);

        let color = kind_color(&entry.kind, no_color);
        let reset = color_reset(no_color);
        println!(
            "  {color}➔ [{}] {display}{reset}",
            entry.kind,
            display = display_path
        );
    }

    render_tree(&files, no_color)?;

    if print {
        println!();
        println!("📦 Printing file contents:");

        let progress = ProgressRunner::new(files.len() as u64, progress_opt_in);

        for file in &files {
            match print_mode {
                PrintMode::Index => {
                    progress.run(file, || -> Result<()> {
                        emit_file(PrintSource::Index, file, HeadFallback::DeletedInIndex)?;
                        println!();
                        Ok(())
                    })?;
                }
                PrintMode::Worktree => {
                    progress.run(file, || -> Result<()> {
                        emit_file(PrintSource::Worktree, file, HeadFallback::FromHead)?;
                        println!();
                        Ok(())
                    })?;
                }
            }
        }

        progress.finish();
    }

    Ok(files)
}

pub fn print_all_files(
    files: &[String],
    staged_lines: &[String],
    unstaged_lines: &[String],
    progress_opt_in: bool,
) -> Result<()> {
    println!();
    println!("📦 Printing file contents:");

    let staged_paths = collect_paths(staged_lines);
    let unstaged_paths = collect_paths(unstaged_lines);

    let total_ops = files
        .iter()
        .map(|file| {
            let staged = staged_paths.contains(file) as u64;
            let unstaged = unstaged_paths.contains(file) as u64;
            let ops = staged + unstaged;
            if ops == 0 { 1 } else { ops }
        })
        .sum::<u64>();

    let progress = ProgressRunner::new(total_ops, progress_opt_in);

    for file in files {
        let mut printed = false;

        if staged_paths.contains(file) {
            progress.run(format!("{file} (index)"), || -> Result<()> {
                emit_file(PrintSource::Index, file, HeadFallback::DeletedInIndex)?;
                println!();
                Ok(())
            })?;
            printed = true;
        }

        if unstaged_paths.contains(file) {
            progress.run(format!("{file} (working tree)"), || -> Result<()> {
                emit_file(PrintSource::Worktree, file, HeadFallback::FromHead)?;
                println!();
                Ok(())
            })?;
            printed = true;
        }

        if !printed {
            progress.run(file, || -> Result<()> {
                emit_file(PrintSource::Worktree, file, HeadFallback::FromHead)?;
                println!();
                Ok(())
            })?;
        }
    }

    progress.finish();

    Ok(())
}

fn collect_paths(lines: &[String]) -> BTreeSet<String> {
    parse_name_status_lines(lines)
        .into_iter()
        .map(|entry| entry.file_path())
        .collect()
}

fn kind_color(kind: &str, no_color: bool) -> &'static str {
    if no_color {
        return "";
    }

    // Use explicit RGB values for former xterm-256 indices so color output
    // stays visually consistent across terminal renderers.
    const COLOR_A: &str = "\x1b[38;2;95;135;135m"; // was 38;5;66
    const COLOR_M: &str = "\x1b[38;2;135;175;215m"; // was 38;5;110
    const COLOR_D: &str = "\x1b[38;2;135;95;95m"; // was 38;5;95

    match kind {
        "A" => COLOR_A,
        "M" => COLOR_M,
        "D" => COLOR_D,
        "U" => COLOR_M,
        "-" => "\x1b[0m",
        _ => COLOR_M,
    }
}

fn color_reset(no_color: bool) -> &'static str {
    if no_color { "" } else { "\x1b[0m" }
}

fn render_tree(files: &[String], no_color: bool) -> Result<()> {
    if files.is_empty() {
        println!("⚠️ No files to render as tree");
        return Ok(());
    }

    println!();
    println!("📂 Directory tree:");

    let support = tree_support();
    if !support.is_installed || !support.supports_fromfile {
        if let Some(warning) = support.warning {
            println!("{warning}");
        } else if !support.is_installed {
            println!("{TREE_MISSING_WARNING}");
        } else {
            println!("{TREE_UNSUPPORTED_WARNING}");
        }
        return Ok(());
    }

    let tree_args = build_tree_args(no_color);

    let tree_input = expand_tree_paths(files);
    let mut cmd = Command::new("tree");
    cmd.args(&tree_args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());

    let mut child = cmd.spawn()?;
    if let Err(err) = write_tree_input(child.stdin.take(), &tree_input) {
        let _ = child.wait();
        println!("⚠️  Failed to stream paths to tree --fromfile ({err}). Skipping directory tree.");
        return Ok(());
    }

    let output = child.wait_with_output()?;
    if !output.status.success() {
        println!(
            "⚠️  tree exited with status {}. Skipping directory tree output.",
            output.status
        );
        return Ok(());
    }

    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    if no_color {
        text = strip_ansi(&text);
    }
    print!("{text}");
    Ok(())
}

fn write_tree_input<W: std::io::Write>(stdin: Option<W>, tree_input: &[String]) -> Result<()> {
    let mut stdin = stdin.ok_or_else(|| anyhow::anyhow!("tree stdin unavailable"))?;
    for line in tree_input {
        writeln!(stdin, "{line}")?;
    }
    Ok(())
}

pub fn kind_color_for_commit(kind: &str, no_color: bool) -> &'static str {
    kind_color(kind, no_color)
}

pub fn color_reset_for_commit(no_color: bool) -> &'static str {
    color_reset(no_color)
}

pub fn render_tree_for_commit(files: &[String], no_color: bool) -> Result<()> {
    render_tree(files, no_color)
}

fn build_tree_args(no_color: bool) -> Vec<&'static str> {
    // `-a` is required even when callers feed paths via stdin: `tree --fromfile`
    // still applies its hidden-name filter on each node, so dot-prefixed paths
    // (e.g. `.agents/`, `.github/`) are silently dropped without it.
    let mut args = vec!["--fromfile", "-a"];
    if !no_color {
        args.push("-C");
    }
    args
}

fn expand_tree_paths(files: &[String]) -> Vec<String> {
    let mut set = BTreeSet::new();
    for file in files {
        let parts: Vec<&str> = file.split('/').collect();
        if parts.is_empty() {
            continue;
        }
        for i in 1..=parts.len() {
            let path = parts[..i].join("/");
            if !path.is_empty() {
                set.insert(path);
            }
        }
    }
    set.into_iter().collect()
}

fn strip_ansi(input: &str) -> String {
    strip_ansi_impl(input, AnsiStripMode::CsiSgrOnly).into_owned()
}

#[cfg(test)]
mod tests {
    use super::{
        build_tree_args, color_reset_for_commit, expand_tree_paths, kind_color_for_commit,
        strip_ansi, write_tree_input,
    };

    #[test]
    fn no_color_mode_returns_empty_color_sequences() {
        assert_eq!(kind_color_for_commit("A", true), "");
        assert_eq!(kind_color_for_commit("M", true), "");
        assert_eq!(kind_color_for_commit("D", true), "");
        assert_eq!(color_reset_for_commit(true), "");
    }

    #[test]
    fn color_mode_uses_expected_commit_palette() {
        assert_eq!(kind_color_for_commit("A", false), "\x1b[38;2;95;135;135m");
        assert_eq!(kind_color_for_commit("M", false), "\x1b[38;2;135;175;215m");
        assert_eq!(kind_color_for_commit("D", false), "\x1b[38;2;135;95;95m");
        assert_eq!(color_reset_for_commit(false), "\x1b[0m");
    }

    #[test]
    fn strip_ansi_removes_m_terminated_sequences() {
        let input = "\x1b[31mred\x1b[0m plain \x1b[38;5;110mblue\x1b[0m";
        assert_eq!(strip_ansi(input), "red plain blue");
    }

    #[test]
    fn write_tree_input_errors_when_stdin_is_missing() {
        let tree_input = vec!["src/main.rs".to_string()];
        let err = write_tree_input::<Vec<u8>>(None, &tree_input).expect_err("missing stdin");
        assert_eq!(err.to_string(), "tree stdin unavailable");
    }

    #[test]
    fn write_tree_input_uses_newline_delimited_paths() {
        let tree_input = vec!["src/main.rs".to_string(), "src/lib.rs".to_string()];
        let mut sink = Vec::new();
        write_tree_input(Some(&mut sink), &tree_input).expect("write tree input");
        assert_eq!(
            String::from_utf8(sink).expect("utf8"),
            "src/main.rs\nsrc/lib.rs\n"
        );
    }

    #[test]
    fn build_tree_args_always_includes_dash_a_to_show_dot_paths() {
        // Regression: `tree --fromfile` would silently drop dot-prefixed paths
        // (e.g. `.agents/...`) from the rendered tree without `-a`.
        assert!(build_tree_args(true).contains(&"-a"));
        assert!(build_tree_args(false).contains(&"-a"));
    }

    #[test]
    fn build_tree_args_toggles_color_flag() {
        assert!(!build_tree_args(true).contains(&"-C"));
        assert!(build_tree_args(false).contains(&"-C"));
    }

    #[test]
    fn expand_tree_paths_preserves_dot_prefixed_segments() {
        let files = vec![
            ".agents/skills/foo/SKILL.md".to_string(),
            ".github/workflows/ci.yml".to_string(),
            "docs/plans/plan.md".to_string(),
        ];
        let paths = expand_tree_paths(&files);
        assert!(paths.contains(&".agents".to_string()));
        assert!(paths.contains(&".agents/skills".to_string()));
        assert!(paths.contains(&".agents/skills/foo".to_string()));
        assert!(paths.contains(&".agents/skills/foo/SKILL.md".to_string()));
        assert!(paths.contains(&".github/workflows/ci.yml".to_string()));
        assert!(paths.contains(&"docs/plans/plan.md".to_string()));
    }

    #[test]
    fn expand_tree_paths_deduplicates_and_sorts_paths() {
        let files = vec![
            "src/lib.rs".to_string(),
            "src/main.rs".to_string(),
            "README.md".to_string(),
        ];
        assert_eq!(
            expand_tree_paths(&files),
            vec![
                "README.md".to_string(),
                "src".to_string(),
                "src/lib.rs".to_string(),
                "src/main.rs".to_string(),
            ]
        );
    }
}
