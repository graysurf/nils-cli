#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod non_macos {
    use nils_test_support::bin::resolve;
    use nils_test_support::cmd::{CmdOptions, run_with};

    #[test]
    fn exits_with_usage_error_without_test_mode() {
        let bin = resolve("screen-record");
        let options = CmdOptions::new().with_env_remove("AGENTS_SCREEN_RECORD_TEST_MODE");
        let out = run_with(&bin, &["--list-windows"], &options);
        assert_eq!(out.code, 2);
        assert!(
            out.stderr_text()
                .contains("only supported on macOS (12+) and Linux (X11)")
        );
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use nils_test_support::bin::resolve;
    use nils_test_support::cmd::{CmdOptions, run_with};
    use tempfile::TempDir;

    #[test]
    fn preflight_missing_ffmpeg_is_actionable() {
        let bin = resolve("screen-record");
        let empty_path = TempDir::new().expect("tempdir");

        let options = CmdOptions::new()
            .with_env_remove("AGENTS_SCREEN_RECORD_TEST_MODE")
            .with_env_remove("WAYLAND_DISPLAY")
            .with_env_remove("DISPLAY")
            .with_env("PATH", &empty_path.path().to_string_lossy());

        let out = run_with(&bin, &["--preflight"], &options);
        assert_eq!(out.code, 1);
        let stderr = out.stderr_text();
        assert!(stderr.contains("ffmpeg"));
        assert!(stderr.contains("apt-get install ffmpeg"));
    }
}
