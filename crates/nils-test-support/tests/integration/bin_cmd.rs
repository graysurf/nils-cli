use nils_test_support::{EnvGuard, GlobalStateLock, bin, cmd, write_exe};
use pretty_assertions::assert_eq;
use tempfile::TempDir;

#[test]
fn resolve_prefers_env_var_with_hyphen() {
    let lock = GlobalStateLock::new();
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("bin-path");
    let _guard = EnvGuard::set(
        &lock,
        "CARGO_BIN_EXE_test-bin",
        path.to_str().expect("path"),
    );

    assert_eq!(bin::resolve("test-bin"), path);
}

#[test]
fn resolve_prefers_env_var_with_underscore() {
    let lock = GlobalStateLock::new();
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("bin-path");
    let _guard = EnvGuard::set(
        &lock,
        "CARGO_BIN_EXE_test_bin",
        path.to_str().expect("path"),
    );

    assert_eq!(bin::resolve("test-bin"), path);
}

#[test]
fn cmd_output_into_output_preserves_fields_and_exit_code() {
    let output = cmd::CmdOutput {
        code: 7,
        stdout: b"stdout bytes".to_vec(),
        stderr: b"stderr bytes".to_vec(),
    };

    let output = output.into_output();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, b"stdout bytes");
    assert_eq!(output.stderr, b"stderr bytes");
}

#[test]
fn cmd_output_into_output_maps_negative_code_to_failure() {
    let output = cmd::CmdOutput {
        code: -1,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };

    let output = output.into_output();
    assert_eq!(output.status.code(), Some(1));
    assert!(!output.status.success());
}

#[cfg(unix)]
#[test]
fn run_captures_exit_code_stdout_stderr_and_env() {
    let temp = TempDir::new().expect("tempdir");
    let script = r#"#!/bin/sh
printf "%s" "$TEST_ENV"
cat - 1>&2
exit 3
"#;
    write_exe(temp.path(), "cmd-test", script);

    let bin = temp.path().join("cmd-test");
    let output = cmd::run(&bin, &[], &[("TEST_ENV", "hello")], Some(b"world"));

    assert_eq!(output.code, 3);
    assert_eq!(output.success(), false);
    assert_eq!(output.stdout, b"hello");
    assert_eq!(output.stderr, b"world");
}

#[cfg(unix)]
#[test]
fn run_in_dir_sets_working_directory() {
    let temp = TempDir::new().expect("tempdir");
    let script = r#"#!/bin/sh
pwd
"#;
    write_exe(temp.path(), "pwd-test", script);

    let bin = temp.path().join("pwd-test");
    let output = cmd::run_in_dir(temp.path(), &bin, &[], &[], None);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stdout = stdout.trim_end();
    let expected = std::fs::canonicalize(temp.path()).expect("canonical");
    let expected = expected.to_string_lossy();
    assert_eq!(stdout, expected);
}

#[cfg(unix)]
#[test]
fn run_with_env_remove_prefix_clears_matching_variables() {
    let lock = GlobalStateLock::new();
    let temp = TempDir::new().expect("tempdir");
    let script = r#"#!/bin/sh
printf "%s|%s" "${NTS_REMOVE_ME-unset}" "${NTS_KEEP_ME-unset}"
"#;
    write_exe(temp.path(), "env-prefix-test", script);
    let bin = temp.path().join("env-prefix-test");

    let _remove_guard = EnvGuard::set(&lock, "NTS_REMOVE_ME", "present");
    let _keep_guard = EnvGuard::set(&lock, "NTS_KEEP_ME", "present");

    let options = cmd::CmdOptions::new().with_env_remove_prefix("NTS_REMOVE_");
    let output = cmd::run_with(&bin, &[], &options);

    assert_eq!(output.code, 0);
    assert_eq!(output.stdout_text(), "unset|present");
}

#[cfg(unix)]
#[test]
fn run_with_env_remove_many_clears_all_requested_variables() {
    let lock = GlobalStateLock::new();
    let temp = TempDir::new().expect("tempdir");
    let script = r#"#!/bin/sh
printf "%s|%s|%s" "${NTS_REMOVE_A-unset}" "${NTS_REMOVE_B-unset}" "${NTS_KEEP-unset}"
"#;
    write_exe(temp.path(), "env-remove-many-test", script);
    let bin = temp.path().join("env-remove-many-test");

    let _remove_a = EnvGuard::set(&lock, "NTS_REMOVE_A", "present");
    let _remove_b = EnvGuard::set(&lock, "NTS_REMOVE_B", "present");
    let _keep = EnvGuard::set(&lock, "NTS_KEEP", "present");

    let options = cmd::CmdOptions::new().with_env_remove_many(&["NTS_REMOVE_A", "NTS_REMOVE_B"]);
    let output = cmd::run_with(&bin, &[], &options);

    assert_eq!(output.code, 0);
    assert_eq!(output.stdout_text(), "unset|unset|present");
}

#[cfg(unix)]
#[test]
fn run_resolved_in_dir_with_stdin_str_supports_optional_text_stdin() {
    let lock = GlobalStateLock::new();
    let temp = TempDir::new().expect("tempdir");
    let script = r#"#!/bin/sh
printf "%s|" "${NTS_VALUE-unset}"
cat -
"#;
    write_exe(temp.path(), "resolved-stdin-test", script);
    let bin = temp.path().join("resolved-stdin-test");
    let _guard = EnvGuard::set(
        &lock,
        "CARGO_BIN_EXE_resolved-stdin-test",
        bin.to_str().expect("path"),
    );

    let with_text = cmd::run_resolved_in_dir_with_stdin_str(
        "resolved-stdin-test",
        temp.path(),
        &[],
        &[("NTS_VALUE", "ok")],
        Some("payload"),
    );
    assert_eq!(with_text.code, 0);
    assert_eq!(with_text.stdout_text(), "ok|payload");

    let without_text = cmd::run_resolved_in_dir_with_stdin_str(
        "resolved-stdin-test",
        temp.path(),
        &[],
        &[("NTS_VALUE", "ok")],
        None,
    );
    assert_eq!(without_text.code, 0);
    assert_eq!(without_text.stdout_text(), "ok|");
}

#[cfg(unix)]
#[test]
fn run_with_env_set_wins_after_env_remove() {
    let lock = GlobalStateLock::new();
    let temp = TempDir::new().expect("tempdir");
    let script = r#"#!/bin/sh
printf "%s" "${NTS_VALUE-unset}"
"#;
    write_exe(temp.path(), "env-override-test", script);
    let bin = temp.path().join("env-override-test");

    let _guard = EnvGuard::set(&lock, "NTS_VALUE", "parent");
    let options = cmd::CmdOptions::new()
        .with_env_remove("NTS_VALUE")
        .with_env("NTS_VALUE", "child");
    let output = cmd::run_with(&bin, &[], &options);

    assert_eq!(output.code, 0);
    assert_eq!(output.stdout_text(), "child");
}

#[cfg(unix)]
#[test]
fn with_path_prepend_places_directory_before_existing_path() {
    let temp = TempDir::new().expect("tempdir");
    let first_dir = temp.path().join("first");
    let second_dir = temp.path().join("second");
    std::fs::create_dir_all(&first_dir).expect("create first dir");
    std::fs::create_dir_all(&second_dir).expect("create second dir");

    write_exe(
        &first_dir,
        "path-pick",
        r#"#!/bin/sh
printf "first"
"#,
    );
    write_exe(
        &second_dir,
        "path-pick",
        r#"#!/bin/sh
printf "second"
"#,
    );
    write_exe(
        temp.path(),
        "path-runner",
        r#"#!/bin/sh
path-pick
"#,
    );

    let bin = temp.path().join("path-runner");
    let base_path = second_dir.to_string_lossy().to_string();
    let base_options = cmd::CmdOptions::new().with_env("PATH", &base_path);
    let base = cmd::run_with(&bin, &[], &base_options);
    assert_eq!(base.code, 0);
    assert_eq!(base.stdout_text(), "second");

    let prefixed_options = base_options.with_path_prepend(&first_dir);
    let prefixed = cmd::run_with(&bin, &[], &prefixed_options);
    assert_eq!(prefixed.code, 0);
    assert_eq!(prefixed.stdout_text(), "first");
}

#[cfg(unix)]
#[test]
fn path_with_prepend_excluding_program_filters_existing_program_and_prepends_stub_dir() {
    let lock = GlobalStateLock::new();
    let temp = TempDir::new().expect("tempdir");
    let existing_program_dir = temp.path().join("existing");
    let kept_dir = temp.path().join("kept");
    let prepend_dir = temp.path().join("prepend");
    std::fs::create_dir_all(&existing_program_dir).expect("create existing dir");
    std::fs::create_dir_all(&kept_dir).expect("create kept dir");
    std::fs::create_dir_all(&prepend_dir).expect("create prepend dir");
    write_exe(
        &existing_program_dir,
        "path-filter-test",
        "#!/bin/sh\nexit 0\n",
    );

    let base_path = std::env::join_paths([existing_program_dir.as_path(), kept_dir.as_path()])
        .expect("join base path")
        .to_string_lossy()
        .to_string();
    let _path = EnvGuard::set(&lock, "PATH", &base_path);

    let filtered = cmd::path_with_prepend_excluding_program(&prepend_dir, "path-filter-test");
    let split = std::env::split_paths(std::ffi::OsStr::new(&filtered)).collect::<Vec<_>>();

    assert_eq!(split[0], prepend_dir);
    assert!(
        split
            .iter()
            .all(|dir| !dir.join("path-filter-test").is_file())
    );
    assert!(split.contains(&kept_dir));
}

#[cfg(unix)]
#[test]
fn run_in_dir_with_overrides_options_cwd() {
    let temp = TempDir::new().expect("tempdir");
    let dir_arg = temp.path().join("arg-dir");
    let option_dir = temp.path().join("option-dir");
    std::fs::create_dir_all(&dir_arg).expect("create arg dir");
    std::fs::create_dir_all(&option_dir).expect("create option dir");

    write_exe(
        temp.path(),
        "pwd-override-test",
        r#"#!/bin/sh
pwd
"#,
    );
    let bin = temp.path().join("pwd-override-test");
    let options = cmd::CmdOptions::new().with_cwd(&option_dir);

    let output = cmd::run_in_dir_with(&dir_arg, &bin, &[], &options);
    let stdout = output.stdout_text();
    let stdout = stdout.trim_end();
    let expected = std::fs::canonicalize(&dir_arg).expect("canonical");
    let expected = expected.to_string_lossy();

    assert_eq!(output.code, 0);
    assert_eq!(stdout, expected);
}
