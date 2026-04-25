use crate::common;
#[test]
fn version_flag_exits_zero() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let out = common::run_image_processing(dir.path(), &["--version"], &[]);

    assert_eq!(out.code, 0);
    assert!(
        out.stdout.contains("image-processing"),
        "stdout: {}",
        out.stdout
    );
}

#[test]
fn completion_zsh_exports_script() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let out = common::run_image_processing(dir.path(), &["completion", "zsh"], &[]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("#compdef image-processing"),
        "stdout: {}",
        out.stdout
    );
}

#[test]
fn completion_rejects_unknown_shell() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let out = common::run_image_processing(dir.path(), &["completion", "fish"], &[]);

    assert_eq!(out.code, 64, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("unsupported completion shell"),
        "stderr: {}",
        out.stderr
    );
}
