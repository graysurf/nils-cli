use std::io::Write;
use std::path::{Path, PathBuf};

const TEMPLATE: &str = include_str!("../plan-template.md");

const USAGE: &str = r##"Usage:
  scaffold_plan.sh --slug <kebab-case> [--title <title>] [--force]
  scaffold_plan.sh --file <path> [--title <title>] [--force]

Purpose:
  Create a new plan markdown file from the shared plan template.

Options:
  --slug <slug>   Base slug (kebab-case). Writes to docs/plans/<slug>/<slug>-plan.md.
                 If <slug> already ends with "-plan", writes to docs/plans/<slug>/<slug>.md.
  --file <path>   Explicit output path (must end with "-plan.md")
  --title <text>  Replace the plan title line ("# Plan: ...")
  --force         Overwrite if the output file already exists
  -h, --help      Show help

Exit:
  0: plan file created
  1: runtime error
  2: usage error
"##;

fn print_usage() {
    let _ = std::io::stderr().write_all(USAGE.as_bytes());
}

fn die_usage(msg: &str) -> i32 {
    eprintln!("scaffold_plan: {msg}");
    print_usage();
    2
}

pub fn run(args: &[String]) -> i32 {
    let repo_root = crate::repo_root::detect();

    let mut slug: Option<String> = None;
    let mut out_file: Option<String> = None;
    let mut title: Option<String> = None;
    let mut force = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--slug" => {
                let Some(v) = args.get(i + 1) else {
                    return die_usage("missing value for --slug");
                };
                if v.is_empty() {
                    return die_usage("missing value for --slug");
                }
                slug = Some(v.to_string());
                i += 2;
            }
            "--file" => {
                let Some(v) = args.get(i + 1) else {
                    return die_usage("missing value for --file");
                };
                if v.is_empty() {
                    return die_usage("missing value for --file");
                }
                out_file = Some(v.to_string());
                i += 2;
            }
            "--title" => {
                let Some(v) = args.get(i + 1) else {
                    return die_usage("missing value for --title");
                };
                if v.is_empty() {
                    return die_usage("missing value for --title");
                }
                title = Some(v.to_string());
                i += 2;
            }
            "--force" => {
                force = true;
                i += 1;
            }
            "-h" | "--help" => {
                print_usage();
                return 0;
            }
            other => {
                return die_usage(&format!("unknown argument: {other}"));
            }
        }
    }

    if slug.is_some() && out_file.is_some() {
        return die_usage("use either --slug or --file (not both)");
    }

    if slug.is_none() && out_file.is_none() {
        return die_usage("missing required --slug or --file");
    }

    if let Some(slug) = slug.as_deref() {
        if !is_kebab_case(slug) {
            return die_usage("--slug must be kebab-case (lowercase letters, digits, hyphens)");
        }
        out_file = Some(default_slug_plan_path(slug));
    }

    let Some(out_file_raw) = out_file else {
        return die_usage("missing required --slug or --file");
    };

    let out_path = resolve_repo_relative(&repo_root, Path::new(&out_file_raw));
    let out_path_str = out_path.to_string_lossy();
    if !out_path_str.ends_with("-plan.md") {
        return die_usage("--file must end with -plan.md");
    }

    if out_path.exists() && !force {
        eprintln!(
            "scaffold_plan: error: output already exists: {}",
            out_path.to_string_lossy()
        );
        return 1;
    }

    if let Some(parent) = out_path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        eprintln!("scaffold_plan: error: failed to create parent dir: {err}");
        return 1;
    }

    if let Err(err) = write_template(&out_path, title.as_deref()) {
        eprintln!("scaffold_plan: error: {err}");
        return 1;
    }

    let created = relativize_for_created(&out_path, &repo_root);
    println!("created: {created}");
    0
}

fn is_kebab_case(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let parts: Vec<&str> = s.split('-').collect();
    if parts.iter().any(|p| p.is_empty()) {
        return false;
    }
    parts.iter().all(|p| {
        p.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    })
}

fn default_slug_plan_path(slug: &str) -> String {
    let filename = if slug.ends_with("-plan") {
        format!("{slug}.md")
    } else {
        format!("{slug}-plan.md")
    };
    format!("docs/plans/{slug}/{filename}")
}

fn resolve_repo_relative(repo_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    repo_root.join(path)
}

fn write_template(dest: &Path, title: Option<&str>) -> anyhow::Result<()> {
    if let Some(title) = title {
        let mut lines = TEMPLATE.lines();
        let _first = lines.next();
        let rest: String = lines.collect::<Vec<&str>>().join("\n");

        let mut out = String::new();
        out.push_str("# Plan: ");
        out.push_str(title);
        out.push('\n');
        if !rest.is_empty() {
            out.push_str(&rest);
            out.push('\n');
        }
        std::fs::write(dest, out)?;
        return Ok(());
    }

    std::fs::write(dest, TEMPLATE)?;
    Ok(())
}

fn relativize_for_created(path: &Path, repo_root: &Path) -> String {
    if let Ok(rel) = path.strip_prefix(repo_root) {
        return rel
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
    }
    path.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        TEMPLATE, default_slug_plan_path, is_kebab_case, relativize_for_created,
        resolve_repo_relative, write_template,
    };
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;

    #[test]
    fn is_kebab_case_enforces_expected_shape() {
        assert!(is_kebab_case("hello-world"));
        assert!(is_kebab_case("a1-b2-c3"));
        assert!(!is_kebab_case(""));
        assert!(!is_kebab_case("Hello-world"));
        assert!(!is_kebab_case("hello--world"));
        assert!(!is_kebab_case("-hello"));
    }

    #[test]
    fn default_slug_plan_path_uses_plan_bundle_folder() {
        assert_eq!(
            default_slug_plan_path("hello-world"),
            "docs/plans/hello-world/hello-world-plan.md"
        );
        assert_eq!(
            default_slug_plan_path("hello-world-plan"),
            "docs/plans/hello-world-plan/hello-world-plan.md"
        );
    }

    #[test]
    fn resolve_repo_relative_joins_relative_and_preserves_absolute() {
        let repo = PathBuf::from("/tmp/repo");
        assert_eq!(
            resolve_repo_relative(&repo, std::path::Path::new("docs/plans/p.md")),
            PathBuf::from("/tmp/repo/docs/plans/p.md")
        );
        assert_eq!(
            resolve_repo_relative(&repo, std::path::Path::new("/opt/plan.md")),
            PathBuf::from("/opt/plan.md")
        );
    }

    #[test]
    fn relativize_for_created_prefers_repo_relative_display() {
        let repo = PathBuf::from("/tmp/repo");
        let inside = PathBuf::from("/tmp/repo/docs/plans/demo-plan.md");
        let outside = PathBuf::from("/tmp/other/demo-plan.md");
        assert_eq!(
            relativize_for_created(&inside, &repo),
            "docs/plans/demo-plan.md"
        );
        assert_eq!(
            relativize_for_created(&outside, &repo),
            "/tmp/other/demo-plan.md"
        );
    }

    #[test]
    fn write_template_overrides_title_when_provided() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("plan.md");
        write_template(&path, Some("Custom title")).expect("write");
        let rendered = std::fs::read_to_string(&path).expect("read");
        assert_eq!(
            rendered.lines().next().unwrap_or_default(),
            "# Plan: Custom title"
        );
        assert!(rendered.contains("## Sprint 1:"));
    }

    #[test]
    fn write_template_without_title_writes_raw_template() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("plan.md");
        write_template(&path, None).expect("write");
        let rendered = std::fs::read_to_string(&path).expect("read");
        assert_eq!(rendered, TEMPLATE);
    }
}
