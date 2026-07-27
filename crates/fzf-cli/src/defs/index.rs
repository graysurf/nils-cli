use crate::util;
use anyhow::Result;
use nils_term::progress::{Progress, ProgressFinish, ProgressOptions};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const EXTRA_ROOTS_ENV: &str = "FZF_DEF_EXTRA_ROOTS";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AliasDef {
    pub name: String,
    pub value: String,
    pub doc: Option<String>,
    pub source_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDef {
    pub name: String,
    pub source: String,
    pub doc: Option<String>,
    pub source_file: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DefIndex {
    pub aliases: Vec<AliasDef>,
    pub functions: Vec<FunctionDef>,
}

pub fn build_index() -> Result<DefIndex> {
    let spinner = Progress::spinner(
        ProgressOptions::default()
            .with_prefix("index ")
            .with_finish(ProgressFinish::Clear),
    );
    spinner.set_message("defs");
    spinner.tick();

    let root = util::zsh_root()?;
    let files = list_definition_files(&root, &spinner)?;
    let mut index = DefIndex::default();

    for (i, file) in files.into_iter().enumerate() {
        if i % 8 == 0 {
            spinner.tick();
        }
        let content = match fs::read(&file) {
            Ok(v) => String::from_utf8_lossy(&v).to_string(),
            Err(_) => continue,
        };
        index_file(&file, &content, &mut index)?;
    }

    index.aliases.sort_by(|a, b| a.name.cmp(&b.name));
    index.functions.sort_by(|a, b| a.name.cmp(&b.name));

    spinner.finish_and_clear();
    Ok(index)
}

fn first_party_file_map(root: &Path, spinner: &Progress) -> Result<BTreeMap<PathBuf, PathBuf>> {
    let mut files = BTreeMap::new();

    let zshrc = root.join(".zshrc");
    if zshrc.is_file() {
        insert_canonical(&mut files, zshrc);
    }
    let zprofile = root.join(".zprofile");
    if zprofile.is_file() {
        insert_canonical(&mut files, zprofile);
    }

    for dir in ["scripts", "bootstrap", "tools"] {
        let d = root.join(dir);
        if !d.is_dir() {
            continue;
        }
        scan_zsh_dir(&d, spinner, true, &[], &mut files);
    }

    Ok(files)
}

#[cfg(test)]
fn list_first_party_files(root: &Path, spinner: &Progress) -> Result<Vec<PathBuf>> {
    Ok(first_party_file_map(root, spinner)?.into_values().collect())
}

fn list_definition_files(root: &Path, spinner: &Progress) -> Result<Vec<PathBuf>> {
    let mut files = first_party_file_map(root, spinner)?;
    add_extra_roots(extra_roots(), spinner, &mut files);

    Ok(files.into_values().collect())
}

fn add_extra_roots(
    roots: Vec<PathBuf>,
    spinner: &Progress,
    files: &mut BTreeMap<PathBuf, PathBuf>,
) -> usize {
    let mut scanned_dirs = Vec::new();
    let mut visited_files = 0;
    for extra_root in roots {
        if extra_root.is_file() {
            if is_zsh_file(&extra_root) {
                let canonical = canonical_or_original(extra_root.clone());
                if !is_below_git_dir(&canonical) {
                    files.entry(canonical).or_insert(extra_root);
                }
            }
            continue;
        }
        if extra_root.is_dir() {
            let canonical_root = canonical_or_original(extra_root.clone());
            if scanned_dirs
                .iter()
                .any(|scanned: &PathBuf| canonical_root.starts_with(scanned))
            {
                continue;
            }
            let excluded_dirs = scanned_dirs
                .iter()
                .filter(|scanned| scanned.starts_with(&canonical_root))
                .cloned()
                .collect::<Vec<_>>();
            visited_files += scan_zsh_dir(&extra_root, spinner, false, &excluded_dirs, files);
            scanned_dirs.push(canonical_root);
        }
    }
    visited_files
}

pub(crate) fn source_roots_identity() -> Result<Vec<String>> {
    let roots = vec![util::zsh_root()?]
        .into_iter()
        .chain(extra_roots())
        .map(|logical| {
            let canonical = canonical_or_original(logical.clone());
            format!(
                "{}\u{1f}{}",
                logical.to_string_lossy(),
                canonical.to_string_lossy()
            )
        })
        .collect();
    Ok(roots)
}

fn extra_roots() -> Vec<PathBuf> {
    env::var_os(EXTRA_ROOTS_ENV)
        .map(|value| {
            env::split_paths(&value)
                .filter(|path| !path.as_os_str().is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn scan_zsh_dir(
    dir: &Path,
    spinner: &Progress,
    skip_plugins: bool,
    excluded_dirs: &[PathBuf],
    files: &mut BTreeMap<PathBuf, PathBuf>,
) -> usize {
    let mut scanned: usize = 0;
    let mut visited_files: usize = 0;
    let entries = WalkDir::new(dir)
        .follow_links(true)
        .into_iter()
        .filter_entry(|entry| {
            if entry.file_name() == ".git" {
                return false;
            }
            if entry.path_is_symlink() && entry.path().is_dir() {
                let canonical = canonical_or_original(entry.path().to_path_buf());
                if is_below_git_dir(&canonical) {
                    return false;
                }
            }
            if excluded_dirs.is_empty() || entry.depth() == 0 || !entry.path().is_dir() {
                return true;
            }
            let canonical = canonical_or_original(entry.path().to_path_buf());
            !excluded_dirs.contains(&canonical)
        });
    for entry in entries {
        scanned = scanned.saturating_add(1);
        if scanned.is_multiple_of(64) {
            spinner.tick();
        }
        let entry = match entry {
            Ok(value) => value,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() || !is_zsh_file(entry.path()) {
            continue;
        }
        // tempdir-leak-audit: allow — walkdir::DirEntry::into_path, not a temp-dir handle.
        let path = entry.into_path();
        let canonical = canonical_or_original(path.clone());
        if is_below_git_dir(&canonical) {
            continue;
        }
        visited_files = visited_files.saturating_add(1);
        if skip_plugins
            && path
                .components()
                .any(|component| component.as_os_str() == "plugins")
        {
            continue;
        }
        files.entry(canonical).or_insert(path);
    }
    visited_files
}

fn is_zsh_file(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("zsh")
}

fn is_below_git_dir(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == ".git")
}

fn insert_canonical(files: &mut BTreeMap<PathBuf, PathBuf>, path: PathBuf) {
    let canonical = canonical_or_original(path.clone());
    files.entry(canonical).or_insert(path);
}

fn canonical_or_original(path: PathBuf) -> PathBuf {
    fs::canonicalize(&path).unwrap_or(path)
}

fn index_file(path: &Path, content: &str, out: &mut DefIndex) -> Result<()> {
    let mut comment_buf: Vec<String> = Vec::new();
    let mut lines = content.lines().peekable();

    while let Some(line) = lines.next() {
        if is_comment_line(line) {
            comment_buf.push(line.to_string());
            continue;
        }

        if line.trim().is_empty() {
            comment_buf.clear();
            continue;
        }

        if let Some(name) = parse_alias_name(line) {
            let value = parse_alias_value(line).unwrap_or_default();
            out.aliases.push(AliasDef {
                name,
                value,
                doc: if comment_buf.is_empty() {
                    None
                } else {
                    Some(comment_buf.join("\n"))
                },
                source_file: path.to_string_lossy().to_string(),
            });
            comment_buf.clear();
            continue;
        }

        if let Some(name) = parse_function_name(line) {
            let mut source_lines: Vec<String> = vec![line.to_string()];
            let mut depth = brace_delta(line);
            while depth > 0 {
                let Some(next) = lines.next() else {
                    break;
                };
                source_lines.push(next.to_string());
                depth += brace_delta(next);
            }

            out.functions.push(FunctionDef {
                name,
                source: source_lines.join("\n"),
                doc: if comment_buf.is_empty() {
                    None
                } else {
                    Some(comment_buf.join("\n"))
                },
                source_file: path.to_string_lossy().to_string(),
            });
            comment_buf.clear();
            continue;
        }

        comment_buf.clear();
    }

    Ok(())
}

fn is_comment_line(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

fn parse_alias_name(line: &str) -> Option<String> {
    let s = line.trim_start();
    if !s.starts_with("alias ") {
        return None;
    }
    let rest = s.strip_prefix("alias ")?;
    let rest = rest.strip_prefix("-g ").unwrap_or(rest);
    let eq = rest.find('=')?;
    let name = rest[..eq].trim();
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}

fn parse_alias_value(line: &str) -> Option<String> {
    let s = line.trim_start();
    let eq = s.find('=')?;
    let raw = s[eq + 1..].trim();
    Some(strip_surrounding_quotes(raw).to_string())
}

fn strip_surrounding_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'')
            || (bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"'))
    {
        return &s[1..bytes.len() - 1];
    }
    s
}

fn parse_function_name(line: &str) -> Option<String> {
    let s = line.trim_start();
    if s.starts_with("function ") {
        let rest = s.strip_prefix("function ")?.trim_start();
        let name = rest
            .split(|c: char| c.is_whitespace() || c == '(' || c == '{')
            .next()
            .unwrap_or("");
        if !name.is_empty() {
            return Some(name.to_string());
        }
    }

    if let Some((name, rest)) = s.split_once("()") {
        let name = name.trim();
        if !name.is_empty() && rest.trim_start().starts_with('{') {
            return Some(name.to_string());
        }
    }

    None
}

fn brace_delta(line: &str) -> i32 {
    let mut delta = 0;
    for ch in line.chars() {
        match ch {
            '{' => delta += 1,
            '}' => delta -= 1,
            _ => {}
        }
    }
    delta
}

#[cfg(test)]
mod tests {
    use super::*;
    use nils_test_support::{EnvGuard, GlobalStateLock};
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }

    #[test]
    fn defs_index_lists_first_party_files_and_ignores_plugins() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write(&root.join(".zshrc"), "alias ll='ls'\n");
        write(&root.join(".zprofile"), "export PATH=/bin\n");
        write(&root.join("scripts/a.zsh"), "alias ga='git add'\n");
        write(&root.join("scripts/plugins/skip.zsh"), "alias nope='no'\n");
        write(&root.join("bootstrap/b.zsh"), "function f() { echo hi }\n");
        write(&root.join("tools/c.zsh"), "foo() { echo ok }\n");
        write(&root.join("scripts/readme.txt"), "ignore\n");

        let spinner = Progress::spinner(
            ProgressOptions::default().with_enabled(nils_term::progress::ProgressEnabled::Off),
        );
        let files = list_first_party_files(root, &spinner).unwrap();
        let names = files
            .iter()
            .map(|p| p.strip_prefix(root).unwrap().to_string_lossy().to_string())
            .collect::<Vec<_>>();

        assert!(names.contains(&".zshrc".to_string()));
        assert!(names.contains(&".zprofile".to_string()));
        assert!(names.contains(&"scripts/a.zsh".to_string()));
        assert!(names.contains(&"bootstrap/b.zsh".to_string()));
        assert!(names.contains(&"tools/c.zsh".to_string()));
        assert!(!names.contains(&"scripts/plugins/skip.zsh".to_string()));
    }

    #[test]
    fn defs_index_parses_aliases_and_functions() {
        let path = Path::new("/tmp/example.zsh");
        let content = r#"
# alias doc
alias ll='ls -la'
alias -g G="| grep"

# function doc
function hello() {
  echo hi
}

world() {
  echo there
}
"#;

        let mut index = DefIndex::default();
        index_file(path, content, &mut index).unwrap();

        assert_eq!(index.aliases.len(), 2);
        let alias_g = index.aliases.iter().find(|a| a.name == "G").unwrap();
        assert_eq!(alias_g.value, "| grep");
        let alias_ll = index.aliases.iter().find(|a| a.name == "ll").unwrap();
        assert_eq!(alias_ll.value, "ls -la");

        assert_eq!(index.functions.len(), 2);
        let func_hello = index.functions.iter().find(|f| f.name == "hello").unwrap();
        assert!(func_hello.source.contains("function hello"));
        let func_world = index.functions.iter().find(|f| f.name == "world").unwrap();
        assert!(func_world.source.contains("world()"));
    }

    #[test]
    fn defs_index_reads_extra_file_and_directory_roots_once() {
        let lock = GlobalStateLock::new();
        let tmp = TempDir::new().unwrap();
        let zsh_root = tmp.path().join("zsh");
        let private_root = tmp.path().join("private");
        let private_link = tmp.path().join("private-link");
        let visible_hook = tmp.path().join("visible-hook.zsh");
        let standalone = tmp.path().join("standalone.zsh");

        write(&zsh_root.join(".zshrc"), "alias base='echo base'\n");
        write(
            &private_root.join("_lib/codex.zsh"),
            "codex-offline() { echo offline }\n",
        );
        write(
            &private_root.join(".git/hooks/skip.zsh"),
            "should-not-index() { echo no }\n",
        );
        std::os::unix::fs::symlink(private_root.join(".git"), private_root.join("visible-git"))
            .expect("git alias symlink");
        std::os::unix::fs::symlink(private_root.join(".git/hooks/skip.zsh"), &visible_hook)
            .expect("git file alias symlink");
        write(&standalone, "alias private-standalone='echo standalone'\n");
        std::os::unix::fs::symlink(&private_root, &private_link).expect("symlink");

        let extra_roots =
            std::env::join_paths([&private_root, &private_link, &standalone, &visible_hook])
                .expect("join paths");
        let _guard_zdot = EnvGuard::set(&lock, "ZDOTDIR", zsh_root.to_string_lossy().as_ref());
        let _guard_cache = EnvGuard::set(&lock, "FZF_DEF_DOC_CACHE_ENABLED", "0");
        let _guard_extra = EnvGuard::set(
            &lock,
            "FZF_DEF_EXTRA_ROOTS",
            extra_roots.to_string_lossy().as_ref(),
        );

        let index = build_index().expect("build index");
        assert_eq!(
            index
                .functions
                .iter()
                .filter(|function| function.name == "codex-offline")
                .count(),
            1
        );
        assert!(
            index
                .aliases
                .iter()
                .any(|alias| alias.name == "private-standalone")
        );
        assert!(
            !index
                .functions
                .iter()
                .any(|function| function.name == "should-not-index")
        );
    }

    #[test]
    fn extra_root_scan_skips_duplicate_and_overlapping_physical_trees() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("private");
        let child = root.join("child");
        let root_link = tmp.path().join("private-link");
        write(&child.join("child.zsh"), "child-fn() { echo child }\n");
        write(&root.join("sibling.zsh"), "sibling-fn() { echo sibling }\n");
        std::os::unix::fs::symlink(&root, &root_link).expect("symlink");

        let spinner = Progress::spinner(
            ProgressOptions::default().with_enabled(nils_term::progress::ProgressEnabled::Off),
        );
        let mut files = BTreeMap::new();
        let visited = add_extra_roots(
            vec![child.clone(), root.clone(), root_link],
            &spinner,
            &mut files,
        );

        assert_eq!(visited, 2);
        assert_eq!(files.len(), 2);
        assert!(files.values().any(|path| path.starts_with(&child)));
        assert!(files.values().any(|path| path == &root.join("sibling.zsh")));
    }
}
