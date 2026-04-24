use crate::common;
use common::{fzf_stub_script, make_stub_dir, run_fzf_cli_with_stub_path, write_exe};
use std::fs;
use tempfile::TempDir;

#[test]
fn file_opens_in_vscode_workspace() {
    let temp = TempDir::new().unwrap();
    fs::create_dir_all(temp.path().join(".git")).unwrap();
    fs::create_dir_all(temp.path().join("src")).unwrap();
    fs::write(temp.path().join("src/notes.txt"), "hello").unwrap();

    let stub = make_stub_dir();
    let out_dir = stub.path().join("fzf-out");
    fs::create_dir_all(&out_dir).unwrap();
    fs::write(out_dir.join("1.out"), "src/notes.txt\n").unwrap();

    let code_log = temp.path().join("code.log");
    write_exe(stub.path(), "fzf", fzf_stub_script());
    write_exe(
        stub.path(),
        "code",
        r#"#!/bin/bash
set -euo pipefail
echo "$@" > "${CODE_STUB_LOG:?}"
exit 0
"#,
    );

    let envs = [
        ("FZF_STUB_OUT_DIR", out_dir.to_str().unwrap()),
        ("FZF_FILE_OPEN_WITH", "vscode"),
        ("CODE_STUB_LOG", code_log.to_str().unwrap()),
    ];

    let output = run_fzf_cli_with_stub_path(temp.path(), stub.path(), &["file"], &envs, None);
    assert_eq!(output.code, 0);

    let code_args = fs::read_to_string(&code_log).unwrap();
    assert!(code_args.contains("--goto"));
    assert!(code_args.contains("src/notes.txt"));
    assert!(code_args.contains("--"));
}

#[test]
fn file_falls_back_to_vi_when_code_fails() {
    let temp = TempDir::new().unwrap();
    fs::create_dir_all(temp.path().join("src")).unwrap();
    fs::write(temp.path().join("src/notes.txt"), "hello").unwrap();

    let stub = make_stub_dir();
    let out_dir = stub.path().join("fzf-out");
    fs::create_dir_all(&out_dir).unwrap();
    fs::write(out_dir.join("1.out"), "src/notes.txt\n").unwrap();

    let code_log = temp.path().join("code.log");
    let vi_log = temp.path().join("vi.log");
    write_exe(stub.path(), "fzf", fzf_stub_script());
    write_exe(
        stub.path(),
        "code",
        r#"#!/bin/bash
set -euo pipefail
echo "$@" > "${CODE_STUB_LOG:?}"
exit 1
"#,
    );
    write_exe(
        stub.path(),
        "vi",
        r#"#!/bin/bash
set -euo pipefail
echo "$@" > "${VI_STUB_LOG:?}"
exit 0
"#,
    );

    let envs = [
        ("FZF_STUB_OUT_DIR", out_dir.to_str().unwrap()),
        ("FZF_FILE_OPEN_WITH", "vscode"),
        ("CODE_STUB_LOG", code_log.to_str().unwrap()),
        ("VI_STUB_LOG", vi_log.to_str().unwrap()),
    ];

    let output = run_fzf_cli_with_stub_path(temp.path(), stub.path(), &["file"], &envs, None);
    assert_eq!(output.code, 0);

    let vi_args = fs::read_to_string(&vi_log).unwrap();
    assert!(vi_args.contains("--"));
    assert!(vi_args.contains("src/notes.txt"));
}
