//! The only sanctioned time value the render engine is allowed to use.
//!
//! Per Resolved Decision #9 in
//! `agent-runtime-kit/docs/source/inventory-target-architecture.md`,
//! render output must be a pure function of the source-root contents.
//! `std::time::SystemTime::now()` and `chrono::Utc::now()` are
//! clippy-banned inside `agent-runtime-cli` and `nils-common`. The
//! single escape hatch is [`source_commit_timestamp`], which returns
//! the ISO-8601 commit timestamp of the source-root's `HEAD` — a value
//! that changes only when the source tree itself changes, so two cold
//! processes rendering the same source produce identical output.
//!
//! See `crates/agent-runtime-cli/docs/determinism.md` for the full
//! contract.

use anyhow::{Context, Result, anyhow};
use std::path::Path;
use std::process::Command;

/// Return the ISO-8601 commit timestamp of `HEAD` in the git repository
/// at `source_root`. Equivalent to `git -C <source_root> log -1
/// --format=%cI HEAD`. The output is the only time-shaped value
/// permitted to land in rendered output.
///
/// Returns `Err` when:
/// - `git` is missing from `PATH`,
/// - `source_root` is not a git repository,
/// - `HEAD` cannot be resolved (empty repo).
pub fn source_commit_timestamp(source_root: &Path) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(source_root)
        .args(["log", "-1", "--format=%cI", "HEAD"])
        .output()
        .with_context(|| {
            format!(
                "spawn `git -C {} log -1 --format=%cI HEAD`",
                source_root.display()
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "git log -1 --format=%cI HEAD exited {status:?} at {root}: {stderr}",
            status = output.status.code(),
            root = source_root.display(),
            stderr = stderr.trim(),
        ));
    }
    let raw = String::from_utf8(output.stdout)
        .context("git log -1 --format=%cI HEAD produced non-UTF8 output")?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!(
            "git log -1 --format=%cI HEAD returned empty stdout at {}",
            source_root.display(),
        ));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn run(cmd: &str, args: &[&str], cwd: &Path) {
        let status = Command::new(cmd)
            .args(args)
            .current_dir(cwd)
            .status()
            .unwrap();
        assert!(
            status.success(),
            "{cmd} {args:?} failed at {}",
            cwd.display()
        );
    }

    fn init_git_repo() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        run("git", &["init", "--quiet", "--initial-branch=main"], &root);
        run("git", &["config", "user.email", "test@example.com"], &root);
        run("git", &["config", "user.name", "Test"], &root);
        run("git", &["config", "commit.gpgsign", "false"], &root);
        run("git", &["config", "tag.gpgsign", "false"], &root);
        fs::write(root.join("README"), "seed\n").unwrap();
        run("git", &["add", "README"], &root);
        // Force a fixed committer date so the assertion below is stable.
        let env_args = [
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "--allow-empty-message",
            "-m",
            "seed",
            "--date=2026-05-21T00:00:00+00:00",
        ];
        let status = Command::new("git")
            .args(env_args)
            .env("GIT_COMMITTER_DATE", "2026-05-21T00:00:00+00:00")
            .env("GIT_AUTHOR_DATE", "2026-05-21T00:00:00+00:00")
            .current_dir(&root)
            .status()
            .unwrap();
        assert!(status.success());
        (tmp, root)
    }

    /// Strict RFC 3339 / ISO 8601 check. Matches `YYYY-MM-DDTHH:MM:SS`
    /// followed by either `Z` or `±HH:MM`. This is what `--format=%cI`
    /// produces and what downstream Tera helpers will assume.
    fn is_strict_iso8601(s: &str) -> bool {
        let bytes = s.as_bytes();
        // Minimum length: "YYYY-MM-DDTHH:MM:SSZ" = 20.
        if bytes.len() < 20 {
            return false;
        }
        let digit = |i: usize| bytes[i].is_ascii_digit();
        let sep = |i: usize, c: u8| bytes[i] == c;
        if !(digit(0)
            && digit(1)
            && digit(2)
            && digit(3)
            && sep(4, b'-')
            && digit(5)
            && digit(6)
            && sep(7, b'-')
            && digit(8)
            && digit(9)
            && sep(10, b'T')
            && digit(11)
            && digit(12)
            && sep(13, b':')
            && digit(14)
            && digit(15)
            && sep(16, b':')
            && digit(17)
            && digit(18))
        {
            return false;
        }
        let tz = &bytes[19..];
        match tz {
            [b'Z'] => true,
            [sign, h1, h2, b':', m1, m2]
                if (*sign == b'+' || *sign == b'-')
                    && h1.is_ascii_digit()
                    && h2.is_ascii_digit()
                    && m1.is_ascii_digit()
                    && m2.is_ascii_digit() =>
            {
                true
            }
            _ => false,
        }
    }

    #[test]
    fn returns_iso8601_timestamp_for_head() {
        let (_tmp, root) = init_git_repo();
        let ts = source_commit_timestamp(&root).unwrap();
        assert!(
            is_strict_iso8601(&ts),
            "expected strict ISO-8601 (YYYY-MM-DDTHH:MM:SS[Z|±HH:MM]), got {ts:?}",
        );
        // The fixed committer date pins this exact value.
        assert!(ts.starts_with("2026-05-21T00:00:00"), "got {ts:?}");
    }

    #[test]
    fn is_stable_across_calls_for_same_head() {
        let (_tmp, root) = init_git_repo();
        let first = source_commit_timestamp(&root).unwrap();
        let second = source_commit_timestamp(&root).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn errors_when_source_root_is_not_a_git_repo() {
        let tmp = TempDir::new().unwrap();
        let err = source_commit_timestamp(tmp.path()).unwrap_err();
        let msg = format!("{err:#}");
        // git's error message contains "not a git repository"; we
        // surface whatever git stderr said with the source root.
        assert!(
            msg.contains("git log") || msg.contains("not a git"),
            "{msg}"
        );
    }

    #[test]
    fn errors_when_repo_has_no_head_yet() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        run("git", &["init", "--quiet", "--initial-branch=main"], &root);
        // No commits — `HEAD` is unborn.
        let err = source_commit_timestamp(&root).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("git log") && !msg.is_empty(),
            "expected git log error for unborn HEAD, got {msg:?}"
        );
    }

    #[test]
    fn iso8601_strictness_rejects_obvious_garbage() {
        // Self-test for the matcher — protects against the matcher
        // silently accepting non-ISO output if git ever changes its
        // `%cI` format.
        assert!(is_strict_iso8601("2026-05-21T00:00:00Z"));
        assert!(is_strict_iso8601("2026-05-21T00:00:00+08:00"));
        assert!(is_strict_iso8601("2026-05-21T00:00:00-05:30"));
        assert!(!is_strict_iso8601(""));
        assert!(!is_strict_iso8601("2026-05-21"));
        assert!(!is_strict_iso8601("2026-05-21 00:00:00Z"));
        assert!(!is_strict_iso8601("2026/05/21T00:00:00Z"));
        assert!(!is_strict_iso8601("2026-05-21T00:00:00"));
        assert!(!is_strict_iso8601("2026-05-21T00:00:00+0000"));
    }
}
