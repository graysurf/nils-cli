use crate::common;
use common::{fzf_stub_script, make_stub_dir, run_fzf_cli_with_stub_path, write_exe};
use nils_test_support::stubs::STUB_LOG_ENV;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn setup_fzf_outputs(out_dir: &Path, key: &str, file_line: &str) {
    fs::create_dir_all(out_dir).unwrap();
    fs::write(
        out_dir.join("1.out"),
        "query\nabcdef1 01-01 00:00 User subject\n",
    )
    .unwrap();
    fs::write(out_dir.join("2.out"), format!("{key}\n{file_line}\n")).unwrap();
}

fn write_git_stub(stub: &Path) {
    write_exe(
        stub,
        "git",
        r#"#!/bin/bash
set -euo pipefail
cmd="$1"; shift || true
case "$cmd" in
  rev-parse)
    if [[ "${1:-}" == "--is-inside-work-tree" ]]; then
      echo "true"
      exit 0
    fi
    if [[ "${1:-}" == "--show-toplevel" ]]; then
      echo "${REPO_ROOT:?}"
      exit 0
    fi
    ;;
  log)
    echo "abcdef1 01-01 00:00 User subject"
    exit 0
    ;;
  diff-tree)
    echo -e "M\tmissing.txt"
    exit 0
    ;;
  show)
    if [[ "${1:-}" == "--numstat" ]]; then
      echo -e "1\t0\tmissing.txt"
      exit 0
    fi
    if [[ "${1:-}" == *:* ]]; then
      echo "snapshot contents"
      exit 0
    fi
    ;;
esac
exit 0
"#,
    );
}

#[test]
fn git_commit_missing_file_abort_snapshot() {
    let temp = TempDir::new().unwrap();
    let repo_root = temp.path().join("repo");
    fs::create_dir_all(&repo_root).unwrap();

    let stub = make_stub_dir();
    let out_dir = stub.path().join("fzf-out");
    setup_fzf_outputs(&out_dir, "ctrl-o", "[M] missing.txt  [+1 / -0]");

    let vi_log = temp.path().join("vi.log");
    fs::write(&vi_log, "").unwrap();

    write_exe(stub.path(), "fzf", fzf_stub_script());
    write_git_stub(stub.path());
    write_exe(
        stub.path(),
        "vi",
        r#"#!/bin/bash
set -euo pipefail
echo "$@" >> "${VI_LOG:?}"
exit 0
"#,
    );

    let out_dir_s = out_dir.to_string_lossy().to_string();
    let repo_root_s = repo_root.to_string_lossy().to_string();
    let vi_log_s = vi_log.to_string_lossy().to_string();
    let envs = [
        ("FZF_STUB_OUT_DIR", out_dir_s.as_str()),
        ("FZF_FILE_OPEN_WITH", "vi"),
        ("REPO_ROOT", repo_root_s.as_str()),
        ("VI_LOG", vi_log_s.as_str()),
    ];

    let out = run_fzf_cli_with_stub_path(
        temp.path(),
        stub.path(),
        &["git-commit"],
        &envs,
        Some("n\n"),
    );
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("File no longer exists in working tree"));
    let log = fs::read_to_string(&vi_log).unwrap();
    assert!(log.trim().is_empty());
}

#[test]
fn git_commit_missing_file_accepts_snapshot() {
    let temp = TempDir::new().unwrap();
    let repo_root = temp.path().join("repo");
    fs::create_dir_all(&repo_root).unwrap();

    let stub = make_stub_dir();
    let out_dir = stub.path().join("fzf-out");
    setup_fzf_outputs(&out_dir, "ctrl-o", "[M] missing.txt  [+1 / -0]");

    let vi_log = temp.path().join("vi.log");
    fs::write(&vi_log, "").unwrap();

    write_exe(stub.path(), "fzf", fzf_stub_script());
    write_git_stub(stub.path());
    write_exe(
        stub.path(),
        "vi",
        r#"#!/bin/bash
set -euo pipefail
echo "$@" >> "${VI_LOG:?}"
file="${@: -1}"
if [[ -f "$file" ]]; then
  /bin/cat "$file" >> "${VI_LOG:?}"
fi
exit 0
"#,
    );

    let out_dir_s = out_dir.to_string_lossy().to_string();
    let repo_root_s = repo_root.to_string_lossy().to_string();
    let vi_log_s = vi_log.to_string_lossy().to_string();
    let envs = [
        ("FZF_STUB_OUT_DIR", out_dir_s.as_str()),
        ("FZF_FILE_OPEN_WITH", "vi"),
        ("REPO_ROOT", repo_root_s.as_str()),
        ("VI_LOG", vi_log_s.as_str()),
    ];

    let out = run_fzf_cli_with_stub_path(
        temp.path(),
        stub.path(),
        &["git-commit"],
        &envs,
        Some("y\n"),
    );
    assert_eq!(out.code, 0);
    let log = fs::read_to_string(&vi_log).unwrap();
    assert!(log.contains("--"));
    assert!(log.contains("snapshot contents"));
}

#[test]
fn git_commit_restore_query_after_esc_uses_single_bracket_change_query() {
    let temp = TempDir::new().unwrap();
    let repo_root = temp.path().join("repo");
    fs::create_dir_all(&repo_root).unwrap();

    let stub = make_stub_dir();
    let out_dir = stub.path().join("fzf-out");
    fs::create_dir_all(&out_dir).unwrap();

    fs::write(
        out_dir.join("1.out"),
        "'gray\nabcdef1 01-01 00:00 User subject\n",
    )
    .unwrap();
    fs::write(out_dir.join("2.out"), "").unwrap();
    fs::write(out_dir.join("2.code"), "1\n").unwrap();
    fs::write(out_dir.join("3.out"), "").unwrap();
    fs::write(out_dir.join("3.code"), "1\n").unwrap();

    let stub_log = temp.path().join("stub.log");
    fs::write(&stub_log, "").unwrap();

    write_exe(stub.path(), "fzf", fzf_stub_script());
    write_git_stub(stub.path());
    write_exe(
        stub.path(),
        "vi",
        "#!/bin/bash\nset -euo pipefail\nexit 0\n",
    );

    let out_dir_s = out_dir.to_string_lossy().to_string();
    let repo_root_s = repo_root.to_string_lossy().to_string();
    let stub_log_s = stub_log.to_string_lossy().to_string();
    let envs = [
        ("FZF_STUB_OUT_DIR", out_dir_s.as_str()),
        ("FZF_FILE_OPEN_WITH", "vi"),
        ("REPO_ROOT", repo_root_s.as_str()),
        (STUB_LOG_ENV, stub_log_s.as_str()),
    ];

    let out = run_fzf_cli_with_stub_path(temp.path(), stub.path(), &["git-commit"], &envs, None);
    assert_eq!(out.code, 1);

    let log = fs::read_to_string(&stub_log).unwrap();
    assert!(
        log.contains("--bind focus:unbind(focus)+change-query['gray]"),
        "expected query restore bind in fzf args, got:\n{log}"
    );
    assert!(
        !log.contains("change-query[['gray]]"),
        "invalid old-style restore bind must not appear, got:\n{log}"
    );
}
