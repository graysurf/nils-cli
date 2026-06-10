use nils_common::fs as common_fs;
use nils_common::git as common_git;
use nils_common::shell::{SingleQuoteEscapeStyle, quote_posix_single_with_style};
use std::path::{Component, Path, PathBuf};

#[derive(Debug)]
pub struct UsageError {
    pub message: String,
}

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for UsageError {}

pub fn usage_err(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(UsageError {
        message: message.into(),
    })
}

pub fn now_run_id() -> String {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let short = uuid::Uuid::new_v4().simple().to_string();
    format!("{stamp}-{}", &short[..6])
}

pub fn find_repo_root() -> PathBuf {
    normalize_path(&common_git::repo_root_or_cwd())
}

pub fn expand_user(raw: &str) -> PathBuf {
    common_fs::expand_home(Path::new(raw))
}

pub fn abs_path(path: &Path, cwd: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    normalize_path(&joined)
}

pub fn normalize_path(path: &Path) -> PathBuf {
    let mut prefix: Option<PathBuf> = None;
    let mut has_root = false;
    let mut stack: Vec<PathBuf> = Vec::new();

    for comp in path.components() {
        match comp {
            Component::Prefix(p) => {
                prefix = Some(PathBuf::from(p.as_os_str()));
            }
            Component::RootDir => {
                has_root = true;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !stack.is_empty() {
                    stack.pop();
                } else if !has_root && prefix.is_none() {
                    stack.push(PathBuf::from(".."));
                }
            }
            Component::Normal(c) => stack.push(PathBuf::from(c)),
        }
    }

    let mut out = PathBuf::new();
    if let Some(p) = prefix {
        out.push(p);
    }
    if has_root {
        #[cfg(windows)]
        out.push("\\");
        #[cfg(not(windows))]
        out.push("/");
    }
    for part in stack {
        out.push(part);
    }
    out
}

pub fn to_posix_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

pub fn maybe_relpath(path: &Path, repo_root: &Path) -> String {
    let repo_root = normalize_path(repo_root);
    let abs = if path.is_absolute() {
        normalize_path(path)
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        abs_path(path, &cwd)
    };

    match abs.strip_prefix(&repo_root) {
        Ok(rel) => to_posix_string(rel),
        Err(_) => to_posix_string(&abs),
    }
}

pub fn ensure_parent_dir(path: &Path, dry_run: bool) -> anyhow::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.exists() {
        return Ok(());
    }
    if dry_run {
        return Ok(());
    }
    std::fs::create_dir_all(parent)?;
    Ok(())
}

pub fn check_overwrite(path: &Path, overwrite: bool) -> anyhow::Result<()> {
    if path.exists() && !overwrite {
        return Err(usage_err(format!(
            "output exists (pass --overwrite to replace): {}",
            path.display()
        )));
    }
    Ok(())
}

pub fn command_str(argv: &[String]) -> String {
    argv.iter()
        .map(|a| shell_escape(a))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_escape(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }

    let safe = |c: char| c.is_ascii_alphanumeric() || "-_./:@+=".contains(c);
    if arg.chars().all(safe) {
        return arg.to_string();
    }

    quote_posix_single_with_style(arg, SingleQuoteEscapeStyle::DoubleQuoteBoundary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nils_test_support::{EnvGuard, GlobalStateLock};
    use pretty_assertions::assert_eq;

    #[test]
    fn expand_user_supports_tilde_and_home_prefix() {
        let lock = GlobalStateLock::new();
        let temp = tempfile::TempDir::new().expect("tempdir");
        let home = temp.path().to_string_lossy().to_string();
        let _guard = EnvGuard::set(&lock, "HOME", &home);

        assert_eq!(expand_user("~"), temp.path().to_path_buf());
        assert_eq!(expand_user("~/x/y"), temp.path().join("x/y"));
        assert_eq!(expand_user("relative/path"), PathBuf::from("relative/path"));
    }

    #[test]
    fn normalize_and_abs_path_remove_dot_segments() {
        let normalized = normalize_path(Path::new("/tmp/repo/./a/../b"));
        assert_eq!(normalized, PathBuf::from("/tmp/repo/b"));

        let abs = abs_path(Path::new("a/../b"), Path::new("/tmp/repo"));
        assert_eq!(abs, PathBuf::from("/tmp/repo/b"));
    }

    #[test]
    fn maybe_relpath_prefers_repo_relative_when_possible() {
        let repo_root = PathBuf::from("/tmp/repo");
        assert_eq!(
            maybe_relpath(Path::new("/tmp/repo/src/main.rs"), &repo_root),
            "src/main.rs"
        );
        assert_eq!(
            maybe_relpath(Path::new("/opt/elsewhere/main.rs"), &repo_root),
            "/opt/elsewhere/main.rs"
        );
    }

    #[test]
    fn ensure_parent_dir_and_check_overwrite_behave_as_expected() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let output = temp.path().join("nested/out.txt");

        ensure_parent_dir(&output, true).expect("dry-run create should be noop");
        assert!(!temp.path().join("nested").exists());

        ensure_parent_dir(&output, false).expect("create parent");
        assert!(temp.path().join("nested").is_dir());

        std::fs::write(&output, "x").expect("write output");
        assert!(check_overwrite(&output, false).is_err());
        assert!(check_overwrite(&output, true).is_ok());
    }

    #[test]
    fn command_str_quotes_arguments_for_shell_safety() {
        let cmd = command_str(&[
            "convert".to_string(),
            "a b.png".to_string(),
            "quote's".to_string(),
            "".to_string(),
        ]);
        assert!(cmd.starts_with("convert "));
        assert!(cmd.contains("'a b.png'"));
        assert!(cmd.contains("'quote'\"'\"'s'"));
        assert!(cmd.ends_with(" ''"));
    }
}
