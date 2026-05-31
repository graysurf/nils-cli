//! Integration tests for the `managed_block` helper that drive the
//! contract through real on-disk read / write / remove cycles. The
//! in-tree unit tests in `src/managed_block.rs` cover marker math and
//! error variants; this suite focuses on the file-system level
//! idempotence guarantees the installer (Sprint 1 Task 1.2) depends on.

use agent_runtime::managed_block::{CommentStyle, ManagedBlock, ManagedBlockError};
use std::fs;
use tempfile::TempDir;

fn write_then_read(path: &std::path::Path, body: &str, force: bool, block: &ManagedBlock) {
    let before = fs::read_to_string(path).unwrap();
    let after = block.write(&before, body, force).unwrap();
    fs::write(path, &after).unwrap();
}

#[test]
fn first_install_appends_block_and_round_trips() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("config.toml");
    fs::write(&path, "model = \"sonnet\"\n").unwrap();

    let block = ManagedBlock::new("install", CommentStyle::Hash);
    let body = "tag = \"agent-runtime\"\nlive_home = \"/tmp/sandbox\"";

    // First install requires `force` because no markers exist yet.
    write_then_read(&path, body, true, &block);
    let installed = fs::read_to_string(&path).unwrap();
    assert!(installed.starts_with("model = \"sonnet\"\n"));
    assert!(
        installed.contains("# >>> agent-runtime-kit:install >>>\n"),
        "open marker missing: {installed:?}"
    );
    assert!(
        installed.contains("# <<< agent-runtime-kit:install <<<\n"),
        "close marker missing: {installed:?}"
    );
    assert_eq!(block.read(&installed).unwrap().as_deref(), Some(body));

    // Re-running the install with the same body is byte-identical
    // (idempotence on the apply path).
    let second = block.write(&installed, body, false).unwrap();
    assert_eq!(second, installed);
}

#[test]
fn write_without_force_fails_on_clean_file() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("settings.json");
    fs::write(&path, "{\n  \"model\": \"sonnet\"\n}\n").unwrap();

    let block = ManagedBlock::new("install", CommentStyle::DoubleSlash);
    let before = fs::read_to_string(&path).unwrap();
    let err = block.write(&before, "{}", false).unwrap_err();
    assert_eq!(
        err,
        ManagedBlockError::NotPresent {
            surface: "install".to_string()
        }
    );

    // File is untouched after a refused write.
    let after = fs::read_to_string(&path).unwrap();
    assert_eq!(before, after);
}

#[test]
fn replace_preserves_outside_bytes_byte_for_byte() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("config.toml");
    let outside_head = "[server]\nport = 4242\n\n";
    let outside_tail = "\n[client]\nlanguage = \"en\"\n";
    let initial = format!(
        "{outside_head}# >>> agent-runtime-kit:install >>>\nold = 1\n# <<< agent-runtime-kit:install <<<{outside_tail}"
    );
    fs::write(&path, &initial).unwrap();

    let block = ManagedBlock::new("install", CommentStyle::Hash);
    let before = fs::read_to_string(&path).unwrap();
    let after = block.write(&before, "fresh = 1\nmore = 2", false).unwrap();

    assert!(after.starts_with(outside_head));
    assert!(after.ends_with(outside_tail));
    assert_eq!(
        block.read(&after).unwrap().as_deref(),
        Some("fresh = 1\nmore = 2")
    );
}

#[test]
fn remove_strips_block_and_leaves_outside_bytes_intact() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("config.toml");
    let initial = "alpha\n# >>> agent-runtime-kit:install >>>\ngarbage\n# <<< agent-runtime-kit:install <<<\nbeta\n";
    fs::write(&path, initial).unwrap();

    let block = ManagedBlock::new("install", CommentStyle::Hash);
    let cleaned = block.remove(&fs::read_to_string(&path).unwrap()).unwrap();
    fs::write(&path, &cleaned).unwrap();

    assert_eq!(fs::read_to_string(&path).unwrap(), "alpha\nbeta\n");
}

#[test]
fn write_refuses_when_body_contains_close_marker_line() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("config.toml");
    let initial = "alpha\n# >>> agent-runtime-kit:install >>>\nold = 1\n# <<< agent-runtime-kit:install <<<\nbeta\n";
    fs::write(&path, initial).unwrap();

    let block = ManagedBlock::new("install", CommentStyle::Hash);
    let before = fs::read_to_string(&path).unwrap();
    let body = "tag = 1\n# <<< agent-runtime-kit:install <<<\nmore = 2";
    let err = block.write(&before, body, true).unwrap_err();
    match err {
        ManagedBlockError::BodyContainsMarker { surface, which } => {
            assert_eq!(surface, "install");
            assert_eq!(which, "close");
        }
        other => panic!("expected BodyContainsMarker, got {other:?}"),
    }

    // File on disk must be byte-for-byte unchanged after the refusal.
    assert_eq!(fs::read_to_string(&path).unwrap(), initial);
}

#[test]
fn unbalanced_markers_refuse_and_do_not_mutate_file() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("config.toml");
    // Open marker without a paired close — must refuse with a typed error.
    let initial = "model = \"sonnet\"\n# >>> agent-runtime-kit:install >>>\nfoo = 1\n";
    fs::write(&path, initial).unwrap();

    let block = ManagedBlock::new("install", CommentStyle::Hash);
    let before = fs::read_to_string(&path).unwrap();
    let err = block.write(&before, "next = 2", true).unwrap_err();
    match err {
        ManagedBlockError::Unbalanced {
            open,
            close,
            surface,
        } => {
            assert_eq!(surface, "install");
            assert_eq!((open, close), (1, 0));
        }
        other => panic!("expected Unbalanced, got {other:?}"),
    }
    // Original file must still be byte-for-byte identical.
    assert_eq!(fs::read_to_string(&path).unwrap(), initial);
}

#[test]
fn jsonc_surface_round_trips_through_settings_file() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("settings.json");
    let initial = "{\n  \"model\": \"sonnet\"\n}\n";
    fs::write(&path, initial).unwrap();

    let block = ManagedBlock::new("install", CommentStyle::DoubleSlash);
    let body = "\"tag\": \"agent-runtime\",\n\"live_home\": \"/tmp/sandbox\"";

    let installed = block
        .write(&fs::read_to_string(&path).unwrap(), body, true)
        .unwrap();
    fs::write(&path, &installed).unwrap();
    assert!(installed.contains("// >>> agent-runtime-kit:install >>>\n"));
    assert_eq!(
        block
            .read(&fs::read_to_string(&path).unwrap())
            .unwrap()
            .as_deref(),
        Some(body)
    );

    // Removing it must restore the original file byte-for-byte.
    let cleaned = block.remove(&fs::read_to_string(&path).unwrap()).unwrap();
    fs::write(&path, &cleaned).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), initial);
}
