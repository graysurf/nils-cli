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

/// `sibling_or_skip` is the only cross-crate surface of the sibling contract, so
/// it is the only part exercised from outside the crate. The classification it
/// wraps is crate-private and unit-tested next to the code.
///
/// The require flag is dropped explicitly because CI sets it for the whole job,
/// and this case is specifically about the behaviour when it is absent.
#[test]
fn sibling_or_skip_yields_none_for_an_absent_sibling_without_panicking() {
    let lock = GlobalStateLock::new();
    let _hyphen = EnvGuard::remove(&lock, "CARGO_BIN_EXE_nts-absent-sibling");
    let _underscore = EnvGuard::remove(&lock, "CARGO_BIN_EXE_nts_absent_sibling");
    let _require = EnvGuard::remove(&lock, "NILS_TEST_REQUIRE_SIBLING_BINS");

    assert_eq!(
        bin::sibling_or_skip("nts-absent-sibling", "nils-absent"),
        None
    );
}

/// The require flag exists so a lane that should have built every binary cannot
/// report green-but-empty when resolution regresses.
#[test]
#[should_panic(expected = "is not built for this run")]
fn sibling_or_skip_panics_for_an_absent_sibling_when_the_require_flag_is_set() {
    let lock = GlobalStateLock::new();
    let _hyphen = EnvGuard::remove(&lock, "CARGO_BIN_EXE_nts-absent-sibling");
    let _underscore = EnvGuard::remove(&lock, "CARGO_BIN_EXE_nts_absent_sibling");
    let _require = EnvGuard::set(&lock, "NILS_TEST_REQUIRE_SIBLING_BINS", "1");

    bin::sibling_or_skip("nts-absent-sibling", "nils-absent");
}

/// A selected-but-wrong artifact is an operator error, so it must fail rather
/// than skip: skipping would leave the suite green while it never ran against
/// the binary it names.
#[cfg(unix)]
#[test]
#[should_panic(expected = "from an earlier release")]
fn sibling_or_skip_panics_for_a_stale_sibling_instead_of_skipping() {
    let lock = GlobalStateLock::new();
    let temp = TempDir::new().expect("tempdir");
    let script = "#!/bin/sh\nprintf '%s\\n' 'nts-stale-skip 1.0.0 (v1.0.0, rustc 1.0.0)'\n";
    write_exe(temp.path(), "nts-stale-skip", script);
    let path = temp.path().join("nts-stale-skip");
    let _guard = EnvGuard::set(
        &lock,
        "CARGO_BIN_EXE_nts-stale-skip",
        path.to_str().expect("path"),
    );

    bin::sibling_or_skip("nts-stale-skip", "nils-stale");
}
