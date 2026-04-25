use tempfile::TempDir;

use screen_record::cli::ContainerFormat;
use screen_record::test_mode::TestWriter;

#[cfg(all(target_os = "macos", not(coverage)))]
use screen_record::macos::writer::{
    write_diagnostics_contact_sheet, write_diagnostics_motion_intervals,
};

#[test]
fn test_writer_creates_non_empty_file() {
    let dir = TempDir::new().expect("tempdir");
    let output_path = dir.path().join("writer.mov");

    let mut writer = TestWriter::new(&output_path, ContainerFormat::Mov);
    writer.append_frame(b"frame").expect("append frame");
    writer.finish().expect("finish writer");

    let metadata = std::fs::metadata(&output_path).expect("output exists");
    assert!(metadata.len() > 0);
}

#[test]
fn test_writer_finish_without_frame_returns_error() {
    let dir = TempDir::new().expect("tempdir");
    let output_path = dir.path().join("writer.mov");

    let writer = TestWriter::new(&output_path, ContainerFormat::Mov);
    let err = writer
        .finish()
        .expect_err("finish should fail without frames");
    assert_eq!(err.exit_code(), 1);
    assert!(err.to_string().contains("no frames appended"));
}

#[cfg(all(target_os = "macos", not(coverage)))]
#[test]
fn diagnostics_artifact_writers_create_readable_files() {
    let dir = TempDir::new().expect("tempdir");
    let output_path = dir.path().join("writer.mov");
    std::fs::write(&output_path, b"fixture-output").expect("write source output");

    let contact_sheet_path = dir.path().join("writer-contact-sheet.svg");
    let motion_intervals_path = dir.path().join("writer-motion-intervals.json");

    write_diagnostics_contact_sheet(&contact_sheet_path, &output_path, 1200)
        .expect("write contact sheet");
    let interval_count =
        write_diagnostics_motion_intervals(&motion_intervals_path, &output_path, 1200)
            .expect("write intervals");

    assert_eq!(interval_count, 1);
    let contact_sheet = std::fs::read_to_string(&contact_sheet_path).expect("read contact sheet");
    assert!(contact_sheet.contains("<svg"));
    assert!(contact_sheet.contains("screen-record diagnostics"));

    let intervals = std::fs::read_to_string(&motion_intervals_path).expect("read intervals");
    assert!(intervals.contains("\"intervals\""));
    assert!(intervals.contains("\"end_ms\": 1200"));
}
