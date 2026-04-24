#[cfg(target_os = "linux")]
mod linux_x11_integration {
    use std::fs;
    use std::time::{Duration, Instant};

    use nils_test_support::bin::resolve;
    use nils_test_support::cmd::{CmdOptions, run_with};
    use nils_test_support::{GlobalStateLock, StubBinDir};
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        AtomEnum, ConnectionExt, CreateWindowAux, EventMask, PropMode, WindowClass,
    };
    use x11rb::wrapper::ConnectionExt as WrapperConnectionExt;

    fn write_ffmpeg_stub(dir: &StubBinDir) {
        dir.write_exe(
            "ffmpeg",
            r#"#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${AGENTS_FFMPEG_LOG:-}" ]]; then
  printf '%s\n' "$@" > "${AGENTS_FFMPEG_LOG}"
fi

out="${@: -1}"
mkdir -p "$(dirname "$out")"
printf "stub" > "$out"
"#,
        );
    }

    fn wait_until_viewable<C: Connection>(
        conn: &C,
        window: u32,
        timeout: Duration,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let deadline = Instant::now() + timeout;
        loop {
            let attrs = conn.get_window_attributes(window)?.reply()?;
            if attrs.map_state == x11rb::protocol::xproto::MapState::VIEWABLE {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("timed out waiting for window to become viewable".into());
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn parse_first_tsv_field(line: &str) -> &str {
        line.split('\t').next().unwrap_or_default()
    }

    fn read_ffmpeg_args(path: &std::path::Path) -> Vec<String> {
        fs::read_to_string(path)
            .expect("read ffmpeg args log")
            .lines()
            .map(|line| line.to_string())
            .collect()
    }

    #[test]
    fn linux_x11_integration_lists_windows_and_routes_to_ffmpeg() {
        let _lock = GlobalStateLock::new();

        let (conn, screen_num) = match x11rb::connect(None) {
            Ok(connection) => connection,
            Err(err) => {
                eprintln!("skipping linux_x11_integration test (X11 unavailable): {err}");
                return;
            }
        };
        let setup = conn.setup();
        let screen = &setup.roots[screen_num];
        let root = screen.root;

        let win = conn.generate_id().expect("generate id");
        conn.create_window(
            x11rb::COPY_FROM_PARENT as u8,
            win,
            root,
            0,
            0,
            320,
            240,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().event_mask(EventMask::EXPOSURE),
        )
        .expect("create window");

        let title = b"Linux X11 Integration";
        conn.change_property8(
            PropMode::REPLACE,
            win,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            title,
        )
        .expect("set WM_NAME");

        // WM_CLASS = instance\0class\0
        let mut wm_class = Vec::new();
        wm_class.extend_from_slice(b"screen-record-test\0");
        wm_class.extend_from_slice(b"ScreenRecordTest\0");
        conn.change_property8(
            PropMode::REPLACE,
            win,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            &wm_class,
        )
        .expect("set WM_CLASS");

        conn.map_window(win).expect("map window");
        conn.flush().expect("flush");
        wait_until_viewable(&conn, win, Duration::from_secs(2)).expect("window viewable");

        let bin = resolve("screen-record");
        let stubs = StubBinDir::new();
        write_ffmpeg_stub(&stubs);

        let options_base = CmdOptions::new()
            .with_env_remove("AGENTS_SCREEN_RECORD_TEST_MODE")
            .with_path_prepend(stubs.path());

        let list = run_with(&bin, &["--list-windows"], &options_base);
        assert_eq!(list.code, 0, "stderr: {}", list.stderr_text());
        let list_stdout = list.stdout_text();
        assert!(
            list_stdout.contains("\tScreenRecordTest\tLinux X11 Integration\t"),
            "expected window in list output, got:\n{list_stdout}"
        );
        assert!(
            list_stdout
                .lines()
                .any(|line| parse_first_tsv_field(line).parse::<u32>().ok() == Some(win)),
            "expected window id {win} in list output, got:\n{list_stdout}"
        );

        let tmp = tempfile::TempDir::new().expect("tempdir");

        // Window recording should use -window_id.
        let win_out = tmp.path().join("window.mov");
        let win_log = tmp.path().join("ffmpeg-window.txt");
        let win_log_value = win_log.to_string_lossy().to_string();
        let options = options_base
            .clone()
            .with_env("AGENTS_FFMPEG_LOG", &win_log_value);
        let win_out_value = win_out.to_string_lossy().to_string();
        let out = run_with(
            &bin,
            &[
                "--window-id",
                &win.to_string(),
                "--duration",
                "1",
                "--audio",
                "off",
                "--path",
                &win_out_value,
            ],
            &options,
        );
        assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
        assert_eq!(out.stderr_text().trim(), "");
        assert_eq!(out.stdout_text().trim_end(), win_out_value);
        assert!(win_out.exists());
        assert!(fs::metadata(&win_out).expect("metadata").len() > 0);

        let args = read_ffmpeg_args(&win_log);
        assert!(args.contains(&"-window_id".to_string()));
        assert!(args.contains(&format!("0x{:x}", win)));

        // Main display recording should use region capture (no -window_id).
        let display_out = tmp.path().join("display.mov");
        let display_log = tmp.path().join("ffmpeg-display.txt");
        let display_log_value = display_log.to_string_lossy().to_string();
        let options = options_base
            .clone()
            .with_env("AGENTS_FFMPEG_LOG", &display_log_value);
        let display_out_value = display_out.to_string_lossy().to_string();
        let out = run_with(
            &bin,
            &[
                "--display",
                "--duration",
                "1",
                "--audio",
                "off",
                "--path",
                &display_out_value,
            ],
            &options,
        );
        assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
        let args = read_ffmpeg_args(&display_log);
        assert!(!args.contains(&"-window_id".to_string()));
        assert!(args.contains(&"-video_size".to_string()));
        assert!(
            args.iter()
                .any(|arg| arg.contains('+') && arg.contains(','))
        );

        // Display-id recording should also use region capture.
        let list = run_with(&bin, &["--list-displays"], &options_base);
        assert_eq!(list.code, 0, "stderr: {}", list.stderr_text());
        let display_id =
            parse_first_tsv_field(list.stdout_text().lines().next().unwrap_or("")).to_string();
        assert!(!display_id.is_empty(), "expected at least one display row");

        let display_id_out = tmp.path().join("display-id.mov");
        let display_id_log = tmp.path().join("ffmpeg-display-id.txt");
        let display_id_log_value = display_id_log.to_string_lossy().to_string();
        let options = options_base.with_env("AGENTS_FFMPEG_LOG", &display_id_log_value);
        let display_id_out_value = display_id_out.to_string_lossy().to_string();
        let out = run_with(
            &bin,
            &[
                "--display-id",
                &display_id,
                "--duration",
                "1",
                "--audio",
                "off",
                "--path",
                &display_id_out_value,
            ],
            &options,
        );
        assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
        let args = read_ffmpeg_args(&display_id_log);
        assert!(!args.contains(&"-window_id".to_string()));
        assert!(args.contains(&"-video_size".to_string()));
        assert!(
            args.iter()
                .any(|arg| arg.contains('+') && arg.contains(','))
        );
    }
}
