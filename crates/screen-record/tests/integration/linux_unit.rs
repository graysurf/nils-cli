#[cfg(target_os = "linux")]
mod linux_unit {
    use std::fs;

    use nils_test_support::{EnvGuard, GlobalStateLock, StubBinDir, cmd, prepend_path};
    use screen_record::cli::{AudioMode, ContainerFormat, ImageFormat};
    use screen_record::linux::ffmpeg;
    use screen_record::linux::portal::PortalCapture;
    use screen_record::types::{Rect, WindowInfo};

    fn window(id: u32) -> WindowInfo {
        WindowInfo {
            id,
            owner_name: "Test".to_string(),
            title: "Window".to_string(),
            bounds: Rect {
                x: 0,
                y: 0,
                width: 640,
                height: 480,
            },
            on_screen: true,
            active: true,
            owner_pid: 1,
            z_order: 0,
        }
    }

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

    fn write_ffmpeg_stub_with_devices(dir: &StubBinDir) {
        dir.write_exe(
            "ffmpeg",
            r#"#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${AGENTS_FFMPEG_LOG:-}" ]]; then
  printf 'CALL\n' >> "${AGENTS_FFMPEG_LOG}"
  printf '%s\n' "$@" >> "${AGENTS_FFMPEG_LOG}"
  printf 'END\n' >> "${AGENTS_FFMPEG_LOG}"
fi

for arg in "$@"; do
  if [[ "$arg" == "-devices" ]]; then
    cat <<'EOF'
Devices:
 D  pipewire           PipeWire audio and video capture
EOF
    exit 0
  fi
done

out="${@: -1}"
mkdir -p "$(dirname "$out")"
printf "stub" > "$out"
"#,
        );
    }

    fn write_ffmpeg_stub_with_pipewire_fd_support(dir: &StubBinDir) {
        dir.write_exe(
            "ffmpeg",
            r#"#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${AGENTS_FFMPEG_LOG:-}" ]]; then
  printf 'CALL\n' >> "${AGENTS_FFMPEG_LOG}"
  printf '%s\n' "$@" >> "${AGENTS_FFMPEG_LOG}"
  printf 'END\n' >> "${AGENTS_FFMPEG_LOG}"
fi

if [[ "$*" == *"-devices"* ]]; then
  cat <<'EOF'
Devices:
 D  pipewire           PipeWire audio and video capture
EOF
  exit 0
fi

if [[ "$*" == *"-h demuxer=pipewire"* ]]; then
  cat <<'EOF'
pipewire demuxer options:
  -pipewire_fd <fd>    Use already-open PipeWire fd
EOF
  exit 0
fi

out="${@: -1}"
mkdir -p "$(dirname "$out")"
printf "stub" > "$out"
"#,
        );
    }

    fn write_ffmpeg_stub_with_pipewire_help_but_no_fd_flag(dir: &StubBinDir) {
        dir.write_exe(
            "ffmpeg",
            r#"#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${AGENTS_FFMPEG_LOG:-}" ]]; then
  printf 'CALL\n' >> "${AGENTS_FFMPEG_LOG}"
  printf '%s\n' "$@" >> "${AGENTS_FFMPEG_LOG}"
  printf 'END\n' >> "${AGENTS_FFMPEG_LOG}"
fi

if [[ "$*" == *"-devices"* ]]; then
  cat <<'EOF'
Devices:
 D  pipewire           PipeWire audio and video capture
EOF
  exit 0
fi

if [[ "$*" == *"-h demuxer=pipewire"* ]]; then
  cat <<'EOF'
pipewire demuxer options:
  -video_size <WxH>    Set capture frame size
EOF
  exit 0
fi

out="${@: -1}"
mkdir -p "$(dirname "$out")"
printf "stub" > "$out"
"#,
        );
    }

    fn write_pactl_stub(dir: &StubBinDir) {
        dir.write_exe(
            "pactl",
            r#"#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${AGENTS_PACTL_LOG:-}" ]]; then
  printf '%s\n' "$*" >> "${AGENTS_PACTL_LOG}"
fi

case "$*" in
  "get-default-sink")
    echo "dummy_sink"
    ;;
  "get-default-source")
    echo "dummy_source"
    ;;
  "list short sources")
    printf "0\tdummy_source\tmodule-null-sink.c\ts16le 2ch 44100Hz\tRUNNING\n"
    printf "1\tdummy_sink.monitor\tmodule-null-sink.c\ts16le 2ch 44100Hz\tRUNNING\n"
    ;;
  "info")
    echo "Default Sink: dummy_sink"
    echo "Default Source: dummy_source"
    ;;
  "list sources")
    echo "Source #0"
    echo -e "\tName: dummy_sink.monitor"
    ;;
  *)
    exit 1
    ;;
esac
"#,
        );
    }

    fn read_log(path: &std::path::Path) -> Vec<String> {
        fs::read_to_string(path)
            .expect("read log")
            .lines()
            .map(|line| line.to_string())
            .collect()
    }

    fn portal_capture_with_remote(node_id: u32) -> PortalCapture {
        use std::os::fd::OwnedFd;

        let file = fs::File::open("/dev/null").expect("open /dev/null");
        let fd = OwnedFd::from(file);
        PortalCapture {
            node_id,
            pipewire_remote: Some(zbus::zvariant::OwnedFd::from(fd)),
        }
    }

    #[test]
    fn linux_unit_ffmpeg_record_window_audio_off_has_expected_flags() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        write_ffmpeg_stub(&stubs);
        write_pactl_stub(&stubs);

        let _path_guard = prepend_path(&lock, stubs.path());
        let _display_guard = EnvGuard::set(&lock, "DISPLAY", ":99");

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let out_path = tmp.path().join("out.mp4");
        let log_path = tmp.path().join("ffmpeg-args.txt");
        let _ffmpeg_log = EnvGuard::set(&lock, "AGENTS_FFMPEG_LOG", &log_path.to_string_lossy());

        ffmpeg::record_window(
            &window(123),
            2,
            AudioMode::Off,
            &out_path,
            ContainerFormat::Mp4,
        )
        .expect("record");

        let args = read_log(&log_path);
        assert!(args.contains(&"-window_id".to_string()));
        assert!(args.contains(&format!("0x{:x}", 123)));
        assert!(args.contains(&"-t".to_string()));
        assert!(args.contains(&"2".to_string()));
        assert!(args.contains(&"-f".to_string()));
        assert!(args.contains(&"mp4".to_string()));
        assert!(args.iter().any(|arg| arg == "-movflags"));
        assert!(args.iter().any(|arg| arg == "+faststart"));
        assert!(!args.iter().any(|arg| arg == "pulse"));
        assert!(out_path.exists());
        assert!(fs::metadata(&out_path).expect("metadata").len() > 0);
    }

    #[test]
    fn linux_unit_ffmpeg_record_window_audio_system_adds_pulse_input_and_maps_audio() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        write_ffmpeg_stub(&stubs);
        write_pactl_stub(&stubs);

        let _path_guard = prepend_path(&lock, stubs.path());
        let _display_guard = EnvGuard::set(&lock, "DISPLAY", ":99");

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let out_path = tmp.path().join("out.mov");
        let ffmpeg_log_path = tmp.path().join("ffmpeg-args.txt");
        let pactl_log_path = tmp.path().join("pactl-args.txt");
        let _ffmpeg_log = EnvGuard::set(
            &lock,
            "AGENTS_FFMPEG_LOG",
            &ffmpeg_log_path.to_string_lossy(),
        );
        let _pactl_log =
            EnvGuard::set(&lock, "AGENTS_PACTL_LOG", &pactl_log_path.to_string_lossy());

        ffmpeg::record_window(
            &window(42),
            1,
            AudioMode::System,
            &out_path,
            ContainerFormat::Mov,
        )
        .expect("record");

        let args = read_log(&ffmpeg_log_path);
        assert!(args.contains(&"-f".to_string()));
        assert!(args.contains(&"pulse".to_string()));
        assert!(args.contains(&"dummy_sink.monitor".to_string()));
        assert!(args.contains(&"-map".to_string()));
        assert!(args.contains(&"0:v:0".to_string()));
        assert!(args.contains(&"1:a:0".to_string()));
        assert!(args.contains(&"-c:a".to_string()));
        assert!(args.contains(&"aac".to_string()));

        let pactl_calls = fs::read_to_string(&pactl_log_path).expect("read pactl log");
        assert!(pactl_calls.contains("get-default-sink"));
        assert!(pactl_calls.contains("list short sources"));
        assert!(!pactl_calls.contains("get-default-source"));
    }

    #[test]
    fn linux_unit_ffmpeg_record_window_audio_mic_adds_default_source() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        write_ffmpeg_stub(&stubs);
        write_pactl_stub(&stubs);

        let _path_guard = prepend_path(&lock, stubs.path());
        let _display_guard = EnvGuard::set(&lock, "DISPLAY", ":99");

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let out_path = tmp.path().join("out.mov");
        let ffmpeg_log_path = tmp.path().join("ffmpeg-args.txt");
        let pactl_log_path = tmp.path().join("pactl-args.txt");
        let _ffmpeg_log = EnvGuard::set(
            &lock,
            "AGENTS_FFMPEG_LOG",
            &ffmpeg_log_path.to_string_lossy(),
        );
        let _pactl_log =
            EnvGuard::set(&lock, "AGENTS_PACTL_LOG", &pactl_log_path.to_string_lossy());

        ffmpeg::record_window(
            &window(77),
            1,
            AudioMode::Mic,
            &out_path,
            ContainerFormat::Mov,
        )
        .expect("record");

        let args = read_log(&ffmpeg_log_path);
        assert!(args.contains(&"-f".to_string()));
        assert!(args.contains(&"pulse".to_string()));
        assert!(args.contains(&"dummy_source".to_string()));
        assert!(args.contains(&"1:a:0".to_string()));

        let pactl_calls = fs::read_to_string(&pactl_log_path).expect("read pactl log");
        assert!(pactl_calls.contains("get-default-source"));
        assert!(!pactl_calls.contains("get-default-sink"));
    }

    #[test]
    fn linux_unit_ffmpeg_record_window_audio_both_adds_two_inputs_and_maps_both_tracks() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        write_ffmpeg_stub(&stubs);
        write_pactl_stub(&stubs);

        let _path_guard = prepend_path(&lock, stubs.path());
        let _display_guard = EnvGuard::set(&lock, "DISPLAY", ":99");

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let out_path = tmp.path().join("out.mov");
        let ffmpeg_log_path = tmp.path().join("ffmpeg-args.txt");
        let pactl_log_path = tmp.path().join("pactl-args.txt");
        let _ffmpeg_log = EnvGuard::set(
            &lock,
            "AGENTS_FFMPEG_LOG",
            &ffmpeg_log_path.to_string_lossy(),
        );
        let _pactl_log =
            EnvGuard::set(&lock, "AGENTS_PACTL_LOG", &pactl_log_path.to_string_lossy());

        ffmpeg::record_window(
            &window(88),
            1,
            AudioMode::Both,
            &out_path,
            ContainerFormat::Mov,
        )
        .expect("record");

        let args = read_log(&ffmpeg_log_path);
        let pulse_inputs: Vec<String> = args
            .iter()
            .filter(|arg| *arg == "dummy_sink.monitor" || *arg == "dummy_source")
            .cloned()
            .collect();
        assert_eq!(
            pulse_inputs,
            vec!["dummy_sink.monitor".to_string(), "dummy_source".to_string()]
        );
        assert!(args.contains(&"1:a:0".to_string()));
        assert!(args.contains(&"2:a:0".to_string()));

        let pactl_calls = fs::read_to_string(&pactl_log_path).expect("read pactl log");
        assert!(pactl_calls.contains("get-default-sink"));
        assert!(pactl_calls.contains("get-default-source"));
    }

    #[test]
    fn linux_unit_ffmpeg_record_portal_node_uses_pipewire_input() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        write_ffmpeg_stub_with_devices(&stubs);

        let _path_guard = prepend_path(&lock, stubs.path());

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let out_path = tmp.path().join("out.mp4");
        let log_path = tmp.path().join("ffmpeg-args.txt");
        let _ffmpeg_log = EnvGuard::set(&lock, "AGENTS_FFMPEG_LOG", &log_path.to_string_lossy());

        ffmpeg::record_portal_node(9001, 2, &out_path, ContainerFormat::Mp4).expect("record");

        let args = read_log(&log_path);
        assert!(args.iter().any(|arg| arg == "-devices"));
        assert!(args.iter().any(|arg| arg == "pipewire"));
        assert!(args.iter().any(|arg| arg == "9001"));
        assert!(out_path.exists());
    }

    #[test]
    fn linux_unit_ffmpeg_record_portal_node_errors_when_pipewire_unsupported() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        stubs.write_exe(
            "ffmpeg",
            r#"#!/usr/bin/env bash
set -euo pipefail

for arg in "$@"; do
  if [[ "$arg" == "-devices" ]]; then
    echo "Devices:"
    echo " D  alsa            ALSA audio capture"
    exit 0
  fi
done

exit 1
"#,
        );

        let _path_guard = prepend_path(&lock, stubs.path());

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let out_path = tmp.path().join("out.mp4");
        let err = ffmpeg::record_portal_node(1, 1, &out_path, ContainerFormat::Mp4)
            .expect_err("unsupported");
        assert_eq!(err.exit_code(), 1);
        assert!(err.to_string().contains("PipeWire"));
    }

    #[test]
    fn linux_unit_ffmpeg_record_window_audio_system_errors_when_pactl_missing() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        write_ffmpeg_stub(&stubs);

        let path = cmd::path_with_prepend_excluding_program(stubs.path(), "pactl");
        let _path_guard = EnvGuard::set(&lock, "PATH", &path);
        let _display_guard = EnvGuard::set(&lock, "DISPLAY", ":99");

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let out_path = tmp.path().join("out.mp4");
        let err = ffmpeg::record_window(
            &window(500),
            1,
            AudioMode::System,
            &out_path,
            ContainerFormat::Mp4,
        )
        .expect_err("missing pactl");
        assert_eq!(err.exit_code(), 1);
        assert!(err.to_string().contains("pactl not found on PATH"));
    }

    #[test]
    fn linux_unit_ffmpeg_record_window_audio_system_errors_on_pactl_spawn_failure() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        write_ffmpeg_stub(&stubs);
        stubs.write_exe("pactl", "this is not an executable format\n");

        let _path_guard = prepend_path(&lock, stubs.path());
        let _display_guard = EnvGuard::set(&lock, "DISPLAY", ":99");

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let out_path = tmp.path().join("out.mp4");
        let err = ffmpeg::record_window(
            &window(501),
            1,
            AudioMode::System,
            &out_path,
            ContainerFormat::Mp4,
        )
        .expect_err("spawn failure");
        assert_eq!(err.exit_code(), 1);
        assert!(err.to_string().contains("failed to spawn pactl"));
    }

    #[test]
    fn linux_unit_ffmpeg_record_window_audio_system_errors_when_pactl_outputs_are_empty() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        write_ffmpeg_stub(&stubs);
        stubs.write_exe(
            "pactl",
            r#"#!/usr/bin/env bash
set -euo pipefail

case "$*" in
  "get-default-sink")
    printf "\n"
    ;;
  "info")
    echo "Server Name: PulseAudio (on PipeWire 1.0.0)"
    ;;
  *)
    exit 1
    ;;
esac
"#,
        );

        let _path_guard = prepend_path(&lock, stubs.path());
        let _display_guard = EnvGuard::set(&lock, "DISPLAY", ":99");

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let out_path = tmp.path().join("out.mp4");
        let err = ffmpeg::record_window(
            &window(502),
            1,
            AudioMode::System,
            &out_path,
            ContainerFormat::Mp4,
        )
        .expect_err("empty pactl output");
        assert_eq!(err.exit_code(), 1);
        assert!(
            err.to_string()
                .contains("failed to resolve default sink via pactl")
        );
    }

    #[test]
    fn linux_unit_ffmpeg_record_window_audio_system_errors_when_sink_monitor_source_missing() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        write_ffmpeg_stub(&stubs);
        stubs.write_exe(
            "pactl",
            r#"#!/usr/bin/env bash
set -euo pipefail

case "$*" in
  "get-default-sink")
    echo "dummy_sink"
    ;;
  "list short sources")
    printf "0\tdummy_source\tmodule-null-sink.c\ts16le 2ch 44100Hz\tRUNNING\n"
    ;;
  *)
    exit 1
    ;;
esac
"#,
        );

        let _path_guard = prepend_path(&lock, stubs.path());
        let _display_guard = EnvGuard::set(&lock, "DISPLAY", ":99");

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let out_path = tmp.path().join("out.mp4");
        let err = ffmpeg::record_window(
            &window(503),
            1,
            AudioMode::System,
            &out_path,
            ContainerFormat::Mp4,
        )
        .expect_err("missing monitor");
        assert_eq!(err.exit_code(), 1);
        assert!(err.to_string().contains("dummy_sink.monitor not found"));
    }

    #[test]
    fn linux_unit_ffmpeg_record_portal_node_errors_when_ffmpeg_devices_probe_fails() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        stubs.write_exe(
            "ffmpeg",
            r#"#!/usr/bin/env bash
set -euo pipefail

if [[ "$*" == *"-devices"* ]]; then
  echo "device probe broke" >&2
  exit 17
fi

exit 0
"#,
        );

        let _path_guard = prepend_path(&lock, stubs.path());

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let out_path = tmp.path().join("out.mp4");
        let err = ffmpeg::record_portal_node(2, 1, &out_path, ContainerFormat::Mp4)
            .expect_err("devices probe failure");
        assert_eq!(err.exit_code(), 1);
        assert!(
            err.to_string()
                .contains("ffmpeg -devices failed (exit code 17)")
        );
        assert!(err.to_string().contains("device probe broke"));
    }

    #[test]
    fn linux_unit_ffmpeg_record_portal_errors_when_ffmpeg_pipewire_demuxer_probe_fails() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        stubs.write_exe(
            "ffmpeg",
            r#"#!/usr/bin/env bash
set -euo pipefail

if [[ "$*" == *"-devices"* ]]; then
  cat <<'EOF'
Devices:
 D  pipewire           PipeWire audio and video capture
EOF
  exit 0
fi

if [[ "$*" == *"-h demuxer=pipewire"* ]]; then
  echo "demuxer probe exploded" >&2
  exit 23
fi

out="${@: -1}"
mkdir -p "$(dirname "$out")"
printf "stub" > "$out"
"#,
        );

        let _path_guard = prepend_path(&lock, stubs.path());
        let capture = portal_capture_with_remote(4242);

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let out_path = tmp.path().join("out.mp4");
        let err = ffmpeg::record_portal(&capture, 1, &out_path, ContainerFormat::Mp4)
            .expect_err("demuxer probe failure");
        assert_eq!(err.exit_code(), 1);
        assert!(
            err.to_string()
                .contains("ffmpeg -h demuxer=pipewire failed (exit code 23)")
        );
        assert!(err.to_string().contains("demuxer probe exploded"));
    }

    #[test]
    fn linux_unit_ffmpeg_record_window_reports_exit_code_and_stderr_on_failure() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        stubs.write_exe(
            "ffmpeg",
            r#"#!/usr/bin/env bash
set -euo pipefail

echo "encoder failed in stub" >&2
exit 9
"#,
        );

        let _path_guard = prepend_path(&lock, stubs.path());
        let _display_guard = EnvGuard::set(&lock, "DISPLAY", ":99");

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let out_path = tmp.path().join("out.mp4");
        let err = ffmpeg::record_window(
            &window(504),
            1,
            AudioMode::Off,
            &out_path,
            ContainerFormat::Mp4,
        )
        .expect_err("ffmpeg non-zero");
        assert_eq!(err.exit_code(), 1);
        assert!(err.to_string().contains("ffmpeg failed (exit code 9)"));
        assert!(err.to_string().contains("encoder failed in stub"));
    }

    #[test]
    fn linux_unit_ffmpeg_screenshot_window_uses_single_frame_capture() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        write_ffmpeg_stub(&stubs);

        let _path_guard = prepend_path(&lock, stubs.path());
        let _display_guard = EnvGuard::set(&lock, "DISPLAY", ":99");

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let out_path = tmp.path().join("shot.png");
        let log_path = tmp.path().join("ffmpeg-args.txt");
        let _ffmpeg_log = EnvGuard::set(&lock, "AGENTS_FFMPEG_LOG", &log_path.to_string_lossy());

        ffmpeg::screenshot_window(&window(321), &out_path, ImageFormat::Png).expect("screenshot");

        let args = read_log(&log_path);
        assert!(args.contains(&"-window_id".to_string()));
        assert!(args.contains(&format!("0x{:x}", 321)));
        assert!(args.contains(&"-frames:v".to_string()));
        assert!(args.contains(&"1".to_string()));
        assert!(!args.contains(&"-t".to_string()));
        assert!(out_path.exists());
    }

    #[test]
    fn linux_unit_ffmpeg_screenshot_window_errors_when_display_missing() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        write_ffmpeg_stub(&stubs);

        let _path_guard = prepend_path(&lock, stubs.path());
        let _display_guard = EnvGuard::remove(&lock, "DISPLAY");

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let out_path = tmp.path().join("shot.png");
        let err = ffmpeg::screenshot_window(&window(322), &out_path, ImageFormat::Png)
            .expect_err("missing DISPLAY");
        assert_eq!(err.exit_code(), 1);
        assert!(err.to_string().contains("DISPLAY is unset"));
    }

    #[test]
    fn linux_unit_ffmpeg_record_portal_with_remote_uses_pipewire_fd_flag_when_supported() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        write_ffmpeg_stub_with_pipewire_fd_support(&stubs);

        let _path_guard = prepend_path(&lock, stubs.path());
        let capture = portal_capture_with_remote(4242);

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let out_path = tmp.path().join("portal-record.mp4");
        let log_path = tmp.path().join("ffmpeg-args.txt");
        let _ffmpeg_log = EnvGuard::set(&lock, "AGENTS_FFMPEG_LOG", &log_path.to_string_lossy());

        ffmpeg::record_portal(&capture, 1, &out_path, ContainerFormat::Mp4).expect("record portal");

        let args = read_log(&log_path);
        assert!(args.iter().any(|arg| arg == "-devices"));
        assert!(args.iter().any(|arg| arg == "demuxer=pipewire"));
        assert!(args.iter().any(|arg| arg == "-pipewire_fd"));
        assert!(args.iter().any(|arg| arg == "3"));
        assert!(args.iter().any(|arg| arg == "4242"));
        assert!(args.iter().any(|arg| arg == "-t"));
        assert!(args.iter().any(|arg| arg == "1"));
        assert!(out_path.exists());
    }

    #[test]
    fn linux_unit_ffmpeg_record_portal_with_remote_errors_when_pipewire_fd_flag_missing() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        write_ffmpeg_stub_with_pipewire_help_but_no_fd_flag(&stubs);

        let _path_guard = prepend_path(&lock, stubs.path());
        let capture = portal_capture_with_remote(4242);

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let out_path = tmp.path().join("portal-record.mp4");
        let err = ffmpeg::record_portal(&capture, 1, &out_path, ContainerFormat::Mp4)
            .expect_err("missing pipewire fd flag");
        assert_eq!(err.exit_code(), 1);
        assert!(err.to_string().contains("missing -pipewire_fd"));
    }

    #[test]
    fn linux_unit_ffmpeg_screenshot_portal_without_remote_falls_back_to_pipewire_node() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        write_ffmpeg_stub_with_devices(&stubs);

        let _path_guard = prepend_path(&lock, stubs.path());

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let out_path = tmp.path().join("portal-shot.png");
        let log_path = tmp.path().join("ffmpeg-args.txt");
        let _ffmpeg_log = EnvGuard::set(&lock, "AGENTS_FFMPEG_LOG", &log_path.to_string_lossy());

        let capture = PortalCapture {
            node_id: 7007,
            pipewire_remote: None,
        };

        ffmpeg::screenshot_portal(&capture, &out_path, ImageFormat::Png)
            .expect("screenshot portal");

        let args = read_log(&log_path);
        assert!(args.iter().any(|arg| arg == "-devices"));
        assert!(args.iter().any(|arg| arg == "-f"));
        assert!(args.iter().any(|arg| arg == "pipewire"));
        assert!(args.iter().any(|arg| arg == "7007"));
        assert!(args.iter().any(|arg| arg == "-frames:v"));
        assert!(args.iter().any(|arg| arg == "1"));
        assert!(out_path.exists());
    }

    #[test]
    fn linux_unit_ffmpeg_screenshot_portal_with_remote_uses_pipewire_fd_flag_when_supported() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        write_ffmpeg_stub_with_pipewire_fd_support(&stubs);

        let _path_guard = prepend_path(&lock, stubs.path());
        let capture = portal_capture_with_remote(5555);

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let out_path = tmp.path().join("portal-shot.png");
        let log_path = tmp.path().join("ffmpeg-args.txt");
        let _ffmpeg_log = EnvGuard::set(&lock, "AGENTS_FFMPEG_LOG", &log_path.to_string_lossy());

        ffmpeg::screenshot_portal(&capture, &out_path, ImageFormat::Png)
            .expect("screenshot portal");

        let args = read_log(&log_path);
        assert!(args.iter().any(|arg| arg == "demuxer=pipewire"));
        assert!(args.iter().any(|arg| arg == "-pipewire_fd"));
        assert!(args.iter().any(|arg| arg == "3"));
        assert!(args.iter().any(|arg| arg == "5555"));
        assert!(args.iter().any(|arg| arg == "-frames:v"));
        assert!(args.iter().any(|arg| arg == "1"));
        assert!(out_path.exists());
    }

    #[test]
    fn linux_unit_ffmpeg_screenshot_portal_with_remote_errors_when_pipewire_fd_flag_missing() {
        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        write_ffmpeg_stub_with_pipewire_help_but_no_fd_flag(&stubs);

        let _path_guard = prepend_path(&lock, stubs.path());
        let capture = portal_capture_with_remote(5555);

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let out_path = tmp.path().join("portal-shot.png");
        let err = ffmpeg::screenshot_portal(&capture, &out_path, ImageFormat::Png)
            .expect_err("missing pipewire fd flag");
        assert_eq!(err.exit_code(), 1);
        assert!(err.to_string().contains("missing -pipewire_fd"));
    }
}
