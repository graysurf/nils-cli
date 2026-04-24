#[cfg(target_os = "linux")]
mod linux_request_permission {
    use nils_test_support::bin::resolve;
    use nils_test_support::cmd::{CmdOptions, run_with};
    use nils_test_support::fs::write_executable;
    use tempfile::TempDir;

    fn ffmpeg_stub_dir() -> TempDir {
        let temp_dir = TempDir::new().expect("tempdir");
        let ffmpeg_path = temp_dir.path().join("ffmpeg");
        write_executable(&ffmpeg_path, "#!/bin/sh\nexit 0\n");

        temp_dir
    }

    #[test]
    fn request_permission_missing_ffmpeg_reports_install_hint() {
        let bin = resolve("screen-record");
        let empty_path = TempDir::new().expect("tempdir");

        let options = CmdOptions::new()
            .with_env_remove("AGENTS_SCREEN_RECORD_TEST_MODE")
            .with_env_remove("AGENTS_SCREEN_RECORD_PORTAL_FORCE_AVAILABLE")
            .with_env_remove("AGENTS_SCREEN_RECORD_PORTAL_FORCE_MISSING")
            .with_env_remove("DISPLAY")
            .with_env_remove("WAYLAND_DISPLAY")
            .with_env("PATH", &empty_path.path().to_string_lossy());

        let out = run_with(&bin, &["--request-permission"], &options);
        assert_eq!(out.code, 1);
        let stderr = out.stderr_text();
        assert!(stderr.contains("ffmpeg"));
        assert!(stderr.contains("apt-get install ffmpeg"));
    }

    #[test]
    fn request_permission_wayland_without_portal_is_actionable() {
        let bin = resolve("screen-record");
        let temp_dir = ffmpeg_stub_dir();

        let options = CmdOptions::new()
            .with_env_remove("AGENTS_SCREEN_RECORD_TEST_MODE")
            .with_env_remove("AGENTS_SCREEN_RECORD_PORTAL_FORCE_AVAILABLE")
            .with_env("AGENTS_SCREEN_RECORD_PORTAL_FORCE_MISSING", "1")
            .with_env_remove("DISPLAY")
            .with_env("WAYLAND_DISPLAY", "wayland-0")
            .with_env("PATH", &temp_dir.path().to_string_lossy());

        let out = run_with(&bin, &["--request-permission"], &options);
        assert_eq!(out.code, 1);
        let stderr = out.stderr_text();
        assert!(stderr.contains("xdg-desktop-portal"));
        assert!(stderr.contains("org.freedesktop.portal.Desktop"));
    }

    #[test]
    fn request_permission_wayland_with_portal_available_succeeds() {
        let bin = resolve("screen-record");
        let temp_dir = ffmpeg_stub_dir();

        let options = CmdOptions::new()
            .with_env_remove("AGENTS_SCREEN_RECORD_TEST_MODE")
            .with_env("AGENTS_SCREEN_RECORD_PORTAL_FORCE_AVAILABLE", "1")
            .with_env_remove("AGENTS_SCREEN_RECORD_PORTAL_FORCE_MISSING")
            .with_env_remove("DISPLAY")
            .with_env("WAYLAND_DISPLAY", "wayland-0")
            .with_env("PATH", &temp_dir.path().to_string_lossy());

        let out = run_with(&bin, &["--request-permission"], &options);
        assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
        assert!(out.stderr_text().trim().is_empty());
    }

    #[test]
    fn request_permission_without_display_or_wayland_reports_runtime_error() {
        let bin = resolve("screen-record");
        let temp_dir = ffmpeg_stub_dir();

        let options = CmdOptions::new()
            .with_env_remove("AGENTS_SCREEN_RECORD_TEST_MODE")
            .with_env_remove("AGENTS_SCREEN_RECORD_PORTAL_FORCE_AVAILABLE")
            .with_env_remove("AGENTS_SCREEN_RECORD_PORTAL_FORCE_MISSING")
            .with_env_remove("DISPLAY")
            .with_env_remove("WAYLAND_DISPLAY")
            .with_env("PATH", &temp_dir.path().to_string_lossy());

        let out = run_with(&bin, &["--request-permission"], &options);
        assert_eq!(out.code, 1);
        assert!(
            out.stderr_text()
                .contains("X11 display not detected (DISPLAY is unset).")
        );
    }
}
