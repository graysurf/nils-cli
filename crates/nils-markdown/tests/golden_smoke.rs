//! Smoke test for `nils_markdown::golden::assert_render`.
//!
//! The positive case proves the harness reads a fixture from disk
//! and accepts a byte-equal render. The negative case proves the
//! harness surfaces a `pretty_assertions` panic when the render
//! drifts from the fixture; it uses `panic::catch_unwind` so the
//! test itself stays green while still asserting that a panic
//! happens and that the panic message names the fixture path.

use std::io::Write;
use std::panic;

use serde::Serialize;
use tempfile::TempDir;

use nils_markdown::Engine;
use nils_markdown::golden::assert_render;

#[derive(Serialize)]
struct View {
    name: String,
}

fn engine_with_template() -> Engine {
    let mut engine = Engine::builder().build();
    engine
        .register_template("greet", "hello, {{ name }}!\n")
        .expect("register template");
    engine
}

fn write_fixture(dir: &TempDir, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    let mut f = std::fs::File::create(&path).expect("create fixture");
    f.write_all(body.as_bytes()).expect("write fixture");
    path
}

#[test]
fn assert_render_accepts_byte_equal_fixture() {
    let dir = TempDir::new().expect("tempdir");
    let fixture = write_fixture(&dir, "greet.golden.md", "hello, tera!\n");
    let mut engine = engine_with_template();
    let view = View {
        name: "tera".into(),
    };
    assert_render(&fixture, &mut engine, "greet", &view);
}

#[test]
fn assert_render_panics_with_diff_when_fixture_drifts() {
    let dir = TempDir::new().expect("tempdir");
    let fixture = write_fixture(&dir, "greet.golden.md", "hello, drifted!\n");
    let fixture_path = fixture.clone();

    let result = panic::catch_unwind(panic::AssertUnwindSafe(move || {
        let mut engine = engine_with_template();
        let view = View {
            name: "tera".into(),
        };
        assert_render(&fixture, &mut engine, "greet", &view);
    }));

    let err = result.expect_err("assert_render must panic on golden drift");
    let msg = if let Some(s) = err.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = err.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else {
        String::from("<non-string panic payload>")
    };
    assert!(
        msg.contains(&fixture_path.display().to_string()),
        "panic message should reference fixture path, got: {msg}"
    );
    assert!(
        msg.contains("golden mismatch") || msg.contains("Diff"),
        "panic message should embed a pretty_assertions diff, got: {msg}"
    );
}
