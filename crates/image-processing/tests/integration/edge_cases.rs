use crate::common;
use std::fs;

use pretty_assertions::assert_eq;

#[test]
fn removed_subcommands_are_usage_errors() {
    let dir = tempfile::TempDir::new().unwrap();

    for removed in [
        "info",
        "auto-orient",
        "resize",
        "rotate",
        "crop",
        "pad",
        "flip",
        "flop",
        "optimize",
    ] {
        let out = common::run_image_processing(dir.path(), &[removed], &[]);
        assert_eq!(out.code, 2, "subcommand={removed}, stderr: {}", out.stderr);
        assert!(
            out.stderr.contains("invalid value")
                || out.stderr.contains("unrecognized subcommand")
                || out.stderr.contains("unknown subcommand"),
            "subcommand={removed}, stderr: {}",
            out.stderr
        );
    }
}

#[test]
fn convert_requires_in() {
    let dir = tempfile::TempDir::new().unwrap();
    fs::write(dir.path().join("a.png"), "img").unwrap();

    let out = common::run_image_processing(
        dir.path(),
        &["convert", "--to", "png", "--out", "out/a.png"],
        &[],
    );
    assert_eq!(out.code, 2);
    assert!(
        out.stderr.contains("convert requires exactly one --in"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn convert_invalid_target_format_is_usage_error() {
    let dir = tempfile::TempDir::new().unwrap();
    fs::write(
        dir.path().join("icon.svg"),
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">
<rect x="2" y="2" width="28" height="28" fill="#0f62fe"/>
</svg>"##,
    )
    .unwrap();

    let out = common::run_image_processing(
        dir.path(),
        &[
            "convert",
            "--in",
            "icon.svg",
            "--to",
            "gif",
            "--out",
            "out/icon.gif",
            "--json",
        ],
        &[],
    );
    assert_eq!(out.code, 2);
    assert!(
        out.stderr.contains("png|webp|jpg"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn convert_rejects_multiple_inputs() {
    let dir = tempfile::TempDir::new().unwrap();
    fs::write(dir.path().join("a.png"), "img").unwrap();
    fs::write(
        dir.path().join("icon.svg"),
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">
<rect x="2" y="2" width="28" height="28" fill="#0f62fe"/>
</svg>"##,
    )
    .unwrap();

    let out = common::run_image_processing(
        dir.path(),
        &[
            "convert",
            "--in",
            "a.png",
            "--in",
            "icon.svg",
            "--to",
            "png",
            "--out",
            "out/icon.png",
            "--json",
        ],
        &[],
    );
    assert_eq!(out.code, 2);
    assert!(
        out.stderr.contains("convert requires exactly one --in"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn convert_rejects_missing_out_or_extension_mismatch() {
    let dir = tempfile::TempDir::new().unwrap();
    fs::write(
        dir.path().join("icon.svg"),
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">
<rect x="2" y="2" width="28" height="28" fill="#0f62fe"/>
</svg>"##,
    )
    .unwrap();

    let missing_out = common::run_image_processing(
        dir.path(),
        &["convert", "--in", "icon.svg", "--to", "png", "--json"],
        &[],
    );
    assert_eq!(missing_out.code, 2);
    assert!(
        missing_out.stderr.contains("convert requires --out"),
        "stderr: {}",
        missing_out.stderr
    );

    let mismatch = common::run_image_processing(
        dir.path(),
        &[
            "convert",
            "--in",
            "icon.svg",
            "--to",
            "png",
            "--out",
            "out/icon.webp",
            "--json",
        ],
        &[],
    );
    assert_eq!(mismatch.code, 2);
    assert!(
        mismatch
            .stderr
            .contains("--out extension must match --to png"),
        "stderr: {}",
        mismatch.stderr
    );
}

#[test]
fn convert_rejects_invalid_dimension_contracts() {
    let dir = tempfile::TempDir::new().unwrap();
    fs::write(
        dir.path().join("icon.svg"),
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">
<rect x="2" y="2" width="28" height="28" fill="#0f62fe"/>
</svg>"##,
    )
    .unwrap();

    let with_zero_width = common::run_image_processing(
        dir.path(),
        &[
            "convert",
            "--in",
            "icon.svg",
            "--to",
            "png",
            "--out",
            "out/icon.png",
            "--width",
            "0",
            "--json",
        ],
        &[],
    );
    assert_eq!(with_zero_width.code, 2);
    assert!(
        with_zero_width.stderr.contains("--width must be > 0"),
        "stderr: {}",
        with_zero_width.stderr
    );

    let with_zero_height = common::run_image_processing(
        dir.path(),
        &[
            "convert",
            "--in",
            "icon.svg",
            "--to",
            "png",
            "--out",
            "out/icon.png",
            "--height",
            "0",
            "--json",
        ],
        &[],
    );
    assert_eq!(with_zero_height.code, 2);
    assert!(
        with_zero_height.stderr.contains("--height must be > 0"),
        "stderr: {}",
        with_zero_height.stderr
    );
}

#[test]
fn svg_validate_requires_single_input_and_out() {
    let dir = tempfile::TempDir::new().unwrap();
    fs::write(dir.path().join("one.svg"), "<svg viewBox='0 0 1 1' />").unwrap();
    fs::write(dir.path().join("two.svg"), "<svg viewBox='0 0 1 1' />").unwrap();

    let missing_out =
        common::run_image_processing(dir.path(), &["svg-validate", "--in", "one.svg"], &[]);
    assert_eq!(missing_out.code, 2);
    assert!(
        missing_out.stderr.contains("svg-validate requires --out"),
        "stderr: {}",
        missing_out.stderr
    );

    let many_inputs = common::run_image_processing(
        dir.path(),
        &[
            "svg-validate",
            "--in",
            "one.svg",
            "--in",
            "two.svg",
            "--out",
            "out/two.svg",
        ],
        &[],
    );
    assert_eq!(many_inputs.code, 2);
    assert!(
        many_inputs.stderr.contains("requires exactly one --in"),
        "stderr: {}",
        many_inputs.stderr
    );
}

#[test]
fn svg_validate_invalid_svg_returns_actionable_error() {
    let dir = tempfile::TempDir::new().unwrap();
    fs::write(
        dir.path().join("invalid.svg"),
        r##"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"##,
    )
    .unwrap();

    let out = common::run_image_processing(
        dir.path(),
        &[
            "svg-validate",
            "--in",
            "invalid.svg",
            "--out",
            "out/invalid.cleaned.svg",
            "--json",
        ],
        &[],
    );
    assert_eq!(out.code, 1, "stderr: {}", out.stderr);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(v["items"][0]["status"], "error");
    let error = v["items"][0]["error"].as_str().unwrap_or("");
    assert!(
        error.contains("missing_viewbox")
            || error.contains("disallowed_tag")
            || error.contains("unsafe_tag"),
        "error: {error}"
    );
}

#[test]
fn convert_overwrite_flag_controls_existing_output_replacement() {
    let dir = tempfile::TempDir::new().unwrap();
    fs::write(
        dir.path().join("icon.svg"),
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">
<rect x="3" y="3" width="26" height="26" rx="3" fill="#0f62fe"/>
</svg>"##,
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("out")).unwrap();
    fs::write(dir.path().join("out/icon.png"), "existing").unwrap();

    let blocked = common::run_image_processing(
        dir.path(),
        &[
            "convert",
            "--in",
            "icon.svg",
            "--to",
            "png",
            "--out",
            "out/icon.png",
            "--json",
        ],
        &[],
    );
    assert_eq!(blocked.code, 2);
    assert!(
        blocked
            .stderr
            .contains("output exists (pass --overwrite to replace)"),
        "stderr: {}",
        blocked.stderr
    );

    let replaced = common::run_image_processing(
        dir.path(),
        &[
            "convert",
            "--in",
            "icon.svg",
            "--to",
            "png",
            "--out",
            "out/icon.png",
            "--overwrite",
            "--json",
        ],
        &[],
    );
    assert_eq!(replaced.code, 0, "stderr: {}", replaced.stderr);
    let rendered = fs::read(dir.path().join("out/icon.png")).unwrap();
    assert!(rendered != b"existing");
    let v: serde_json::Value = serde_json::from_str(&replaced.stdout).unwrap();
    assert_eq!(v["items"][0]["status"], "ok");
    assert_eq!(v["items"][0]["output_info"]["format"], "PNG");
}
