//! `when` predicate parsing and evaluation.
//!
//! A predicate is a disjunction (`||`) of conjunctions (`&&`) of atoms. The
//! only atom kinds are `path-exists:<glob>` and the literal `always`. `&&`
//! binds tighter than `||`. The grammar is deliberately small (see the
//! redesign source): it is not a general expression language.

use std::path::Path;

use crate::model::{When, WhenAtom};

/// Directories that are never descended while evaluating `path-exists` globs.
/// They are large, noisy, and never the signal a `when` predicate is after.
const PRUNED_DIRS: [&str; 4] = [".git", "target", "node_modules", ".jj"];

/// The maximum directory depth walked while evaluating a `**` glob.
const MAX_WALK_DEPTH: usize = 24;

/// Parse a `when` string into a [`When`]. An empty string or `always` yields
/// [`When::Always`].
pub fn parse_when(raw: &str) -> Result<When, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("always") {
        return Ok(When::Always);
    }

    let mut clauses = Vec::new();
    for or_part in trimmed.split("||") {
        let or_part = or_part.trim();
        if or_part.is_empty() {
            return Err("empty clause around `||`".to_string());
        }
        let mut atoms = Vec::new();
        for and_part in or_part.split("&&") {
            atoms.push(parse_atom(and_part.trim())?);
        }
        clauses.push(atoms);
    }

    Ok(When::Any(clauses))
}

fn parse_atom(raw: &str) -> Result<WhenAtom, String> {
    if raw.is_empty() {
        return Err("empty atom around `&&`".to_string());
    }
    if raw.eq_ignore_ascii_case("always") {
        return Ok(WhenAtom::Always);
    }
    if let Some(glob) = raw.strip_prefix("path-exists:") {
        let glob = glob.trim();
        if glob.is_empty() {
            return Err("`path-exists:` requires a glob".to_string());
        }
        return Ok(WhenAtom::PathExists {
            glob: glob.to_string(),
        });
    }
    Err(format!(
        "unsupported atom `{raw}`; expected `path-exists:<glob>` or `always`"
    ))
}

/// Evaluate a predicate against the resolved project root.
pub fn evaluate(when: &When, project_root: &Path) -> bool {
    match when {
        When::Always => true,
        When::Any(clauses) => clauses
            .iter()
            .any(|atoms| atoms.iter().all(|atom| evaluate_atom(atom, project_root))),
    }
}

fn evaluate_atom(atom: &WhenAtom, project_root: &Path) -> bool {
    match atom {
        WhenAtom::Always => true,
        WhenAtom::PathExists { glob } => path_exists(project_root, glob),
    }
}

/// True when at least one filesystem path matching `glob` exists under
/// `project_root`.
fn path_exists(project_root: &Path, glob: &str) -> bool {
    let glob = glob.trim_start_matches("./");
    // Fast path: a literal path (no glob metacharacters) is a direct lookup.
    if !has_glob_meta(glob) {
        return project_root.join(glob).exists();
    }

    let segments: Vec<&str> = glob.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return false;
    }
    match_segments(project_root, &segments, 0)
}

fn has_glob_meta(pattern: &str) -> bool {
    pattern.contains(['*', '?', '['])
}

/// Recursively match the remaining glob `segments` starting at `dir`.
fn match_segments(dir: &Path, segments: &[&str], depth: usize) -> bool {
    let Some((segment, rest)) = segments.split_first() else {
        // All segments consumed: the directory itself is the match target.
        return dir.exists();
    };

    if depth > MAX_WALK_DEPTH {
        return false;
    }

    if *segment == "**" {
        // `**` matches zero or more directories. Try consuming it for zero
        // segments here, then descend into each child trying to match `rest`
        // (and `**` again) deeper down.
        if match_segments(dir, rest, depth) {
            return true;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            if is_pruned(&entry.file_name()) {
                continue;
            }
            if match_segments(&entry.path(), segments, depth + 1) {
                return true;
            }
        }
        return false;
    }

    // A normal segment: match each direct child name against the segment glob.
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !segment_matches(segment, name) {
            continue;
        }
        if rest.is_empty() {
            return true;
        }
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
            && match_segments(&entry.path(), rest, depth + 1)
        {
            return true;
        }
    }
    false
}

fn is_pruned(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .map(|name| PRUNED_DIRS.contains(&name))
        .unwrap_or(false)
}

/// Match a single path segment glob (`*`, `?`, `[...]` within one segment)
/// against a concrete file name.
fn segment_matches(pattern: &str, name: &str) -> bool {
    glob_match(pattern.as_bytes(), name.as_bytes())
}

/// A minimal glob matcher over a single path segment. Supports `*` (any run of
/// characters), `?` (single character), and `[...]` character classes. `*`
/// here does not cross `/` because callers only pass single segments.
fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    // Iterative backtracking matcher.
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star_p, mut star_t): (Option<usize>, usize) = (None, 0);

    while t < text.len() {
        if p < pattern.len() {
            match pattern[p] {
                b'*' => {
                    star_p = Some(p);
                    star_t = t;
                    p += 1;
                    continue;
                }
                b'?' => {
                    p += 1;
                    t += 1;
                    continue;
                }
                b'[' => {
                    if let Some((matched, next_p)) = match_class(pattern, p, text[t]) {
                        if matched {
                            p = next_p;
                            t += 1;
                            continue;
                        }
                    } else if pattern[p] == text[t] {
                        // Unterminated class: treat `[` literally.
                        p += 1;
                        t += 1;
                        continue;
                    }
                }
                c if c == text[t] => {
                    p += 1;
                    t += 1;
                    continue;
                }
                _ => {}
            }
        }

        // Mismatch: backtrack to the last `*` if any.
        if let Some(sp) = star_p {
            p = sp + 1;
            star_t += 1;
            t = star_t;
        } else {
            return false;
        }
    }

    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

/// Match a `[...]` character class at `pattern[start]` against `ch`. Returns
/// `(matched, index_after_class)` or `None` if the class is unterminated.
fn match_class(pattern: &[u8], start: usize, ch: u8) -> Option<(bool, usize)> {
    debug_assert_eq!(pattern[start], b'[');
    let mut i = start + 1;
    let mut negate = false;
    if i < pattern.len() && (pattern[i] == b'!' || pattern[i] == b'^') {
        negate = true;
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    while i < pattern.len() {
        let c = pattern[i];
        if c == b']' && !first {
            return Some((matched ^ negate, i + 1));
        }
        first = false;
        // Range like a-z.
        if i + 2 < pattern.len() && pattern[i + 1] == b'-' && pattern[i + 2] != b']' {
            let lo = c;
            let hi = pattern[i + 2];
            if lo <= ch && ch <= hi {
                matched = true;
            }
            i += 3;
        } else {
            if c == ch {
                matched = true;
            }
            i += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parse_always_variants() {
        assert_eq!(parse_when("").unwrap(), When::Always);
        assert_eq!(parse_when("  ").unwrap(), When::Always);
        assert_eq!(parse_when("always").unwrap(), When::Always);
        assert_eq!(parse_when("ALWAYS").unwrap(), When::Always);
    }

    #[test]
    fn parse_or_of_ands() {
        let when =
            parse_when("path-exists:Cargo.toml || path-exists:package.json && path-exists:src/**")
                .unwrap();
        match when {
            When::Any(clauses) => {
                assert_eq!(clauses.len(), 2);
                assert_eq!(clauses[0].len(), 1);
                assert_eq!(clauses[1].len(), 2);
            }
            When::Always => panic!("expected Any"),
        }
    }

    #[test]
    fn parse_rejects_unknown_atom() {
        assert!(parse_when("file-exists:foo").is_err());
        assert!(parse_when("path-exists:").is_err());
        assert!(parse_when("path-exists:a ||").is_err());
    }

    #[test]
    fn glob_match_basics() {
        assert!(glob_match(b"*.toml", b"Cargo.toml"));
        assert!(glob_match(b"Cargo.*", b"Cargo.toml"));
        assert!(!glob_match(b"*.toml", b"Cargo.lock"));
        assert!(glob_match(b"?argo.toml", b"Cargo.toml"));
        assert!(glob_match(b"[Cc]argo.toml", b"cargo.toml"));
        assert!(!glob_match(b"[!Cc]argo.toml", b"cargo.toml"));
        assert!(glob_match(b"file[0-9].md", b"file3.md"));
    }

    #[test]
    fn evaluate_literal_and_glob() {
        let dir = std::env::temp_dir().join(format!(
            "agent-docs-predicate-{}-{}",
            std::process::id(),
            "eval"
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src/inner")).unwrap();
        fs::write(dir.join("Cargo.toml"), "[package]\n").unwrap();
        fs::write(dir.join("src/inner/lib.rs"), "fn x() {}\n").unwrap();

        assert!(path_exists(&dir, "Cargo.toml"));
        assert!(!path_exists(&dir, "package.json"));
        assert!(path_exists(&dir, "src/**"));
        assert!(path_exists(&dir, "src/**/*.rs"));
        assert!(!path_exists(&dir, "src/**/*.py"));

        let code = parse_when("path-exists:Cargo.toml || path-exists:package.json").unwrap();
        assert!(evaluate(&code, &dir));
        let py = parse_when("path-exists:pyproject.toml").unwrap();
        assert!(!evaluate(&py, &dir));

        let _ = fs::remove_dir_all(&dir);
    }
}
