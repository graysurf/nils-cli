use screen_record::error::CliError;

#[test]
fn runtime_error_prefixes_message() {
    let err = CliError::runtime("boom");
    assert_eq!(err.exit_code(), 1);
    assert_eq!(err.to_string(), "error: boom");
}

#[test]
fn unsupported_platform_is_usage_error() {
    let err = CliError::unsupported_platform();
    assert_eq!(err.exit_code(), 2);
    assert!(
        err.to_string()
            .contains("only supported on macOS (12+) and Linux (X11)")
    );
}
