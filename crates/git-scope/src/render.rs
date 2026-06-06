use crate::change::parse_name_status_lines;
use crate::print::{HeadFallback, PrintSource, emit_file};
use crate::progress::ProgressRunner;
use crate::tree::render_path_tree;
use anyhow::Result;
use std::collections::BTreeSet;

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

    let rendered = render_path_tree(files, no_color);
    for line in rendered.lines() {
        println!("{line}");
    }
    println!();
    println!("{}", rendered.summary());
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

#[cfg(test)]
mod tests {
    use super::{color_reset_for_commit, kind_color_for_commit};

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
}
