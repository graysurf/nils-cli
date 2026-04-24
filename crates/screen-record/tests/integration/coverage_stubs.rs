#![cfg(coverage)]

use tempfile::TempDir;

use screen_record::cli::{AudioMode, ContainerFormat, ImageFormat};
use screen_record::macos::{permissions, screenshot, shareable, stream};

#[test]
fn coverage_stubs_are_invokable() {
    permissions::preflight().expect("preflight");
    permissions::request_permission().expect("request permission");

    let content = shareable::fetch_shareable().expect("shareable content");
    let window = content.windows.first().expect("window fixture");

    let tmp = TempDir::new().expect("tempdir");
    let screenshot_path = tmp.path().join("stub.png");
    screenshot::screenshot_window(window, &screenshot_path, ImageFormat::Png)
        .expect("screenshot stub");

    let record_path = tmp.path().join("stub.mov");
    stream::record_window(
        window,
        1,
        AudioMode::Off,
        &record_path,
        ContainerFormat::Mov,
    )
    .expect("record window stub");
    stream::record_display(1, 1, AudioMode::Off, &record_path, ContainerFormat::Mov)
        .expect("record display stub");
    stream::record_main_display(1, AudioMode::Off, &record_path, ContainerFormat::Mov)
        .expect("record main display stub");
}
