use crate::model::ImageInfo;
use crate::util;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType as PngCompression, FilterType as PngFilter, PngEncoder};
use image::codecs::webp::WebPEncoder;
use image::{ExtendedColorType, ImageEncoder};
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

const JPEG_QUALITY: u8 = 90;
const PNG_COMPRESSION: PngCompression = PngCompression::Best;
const PNG_FILTER: PngFilter = PngFilter::NoFilter;

#[derive(Clone, Debug, Serialize)]
pub struct SvgDiagnostic {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct SanitizedSvgDocument {
    pub content: String,
    pub width: u32,
    pub height: u32,
    pub uses_alpha: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RasterSizeHint {
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SvgValidateCommandResult {
    pub input_path: String,
    pub output_path: String,
    pub sanitized: bool,
    pub diagnostics: Vec<SvgDiagnostic>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

pub fn sanitize_svg_file(path: &Path) -> anyhow::Result<SanitizedSvgDocument> {
    let raw = std::fs::read_to_string(path).map_err(|err| {
        util::usage_err(format!(
            "failed to read svg input {}: {err}",
            path.display()
        ))
    })?;
    sanitize_svg_text(&raw).map_err(|diagnostics| diagnostics_to_error(path, &diagnostics))
}

pub fn sanitize_svg_text(raw: &str) -> Result<SanitizedSvgDocument, Vec<SvgDiagnostic>> {
    let normalized = normalize_svg_text(raw);

    let doc = match roxmltree::Document::parse(&normalized) {
        Ok(doc) => doc,
        Err(err) => {
            return Err(vec![SvgDiagnostic {
                code: "parse_error".to_string(),
                message: format!("xml parse error: {err}"),
            }]);
        }
    };

    let root = doc.root_element();
    if root.tag_name().name() != "svg" {
        return Err(vec![SvgDiagnostic {
            code: "root_not_svg".to_string(),
            message: "root element must be <svg>".to_string(),
        }]);
    }

    let mut diagnostics: Vec<SvgDiagnostic> = Vec::new();

    let view_box_raw = match root.attribute("viewBox") {
        Some(v) => v,
        None => {
            diagnostics.push(SvgDiagnostic {
                code: "missing_viewbox".to_string(),
                message: "svg root must include viewBox=\"minX minY width height\"".to_string(),
            });
            ""
        }
    };

    let (width, height) = if diagnostics.is_empty() {
        match parse_view_box(view_box_raw) {
            Ok(v) => v,
            Err(msg) => {
                diagnostics.push(SvgDiagnostic {
                    code: "invalid_viewbox".to_string(),
                    message: msg,
                });
                (0, 0)
            }
        }
    } else {
        (0, 0)
    };

    let allowed_tags: BTreeSet<&str> = [
        "svg",
        "g",
        "path",
        "circle",
        "ellipse",
        "rect",
        "line",
        "polyline",
        "polygon",
        "defs",
        "linearGradient",
        "radialGradient",
        "stop",
        "title",
        "desc",
        "clipPath",
        "mask",
    ]
    .into_iter()
    .collect();

    for node in doc.descendants().filter(|n| n.is_element()) {
        let tag = node.tag_name().name();
        if !allowed_tags.contains(tag) {
            diagnostics.push(SvgDiagnostic {
                code: "disallowed_tag".to_string(),
                message: format!("tag <{tag}> is not allowed by policy"),
            });
        }

        if tag == "script" || tag == "foreignObject" {
            diagnostics.push(SvgDiagnostic {
                code: "unsafe_tag".to_string(),
                message: format!("tag <{tag}> is forbidden"),
            });
        }

        for attr in node.attributes() {
            let name = attr.name();
            let value = attr.value().trim();
            let lower_name = name.to_ascii_lowercase();
            if lower_name.starts_with("on") {
                diagnostics.push(SvgDiagnostic {
                    code: "unsafe_attribute".to_string(),
                    message: format!("attribute {name} is not allowed"),
                });
            }

            if lower_name == "href" || lower_name.ends_with(":href") {
                let lower_value = value.to_ascii_lowercase();
                if lower_value.starts_with("http:")
                    || lower_value.starts_with("https:")
                    || lower_value.starts_with("data:")
                    || lower_value.starts_with("file:")
                {
                    diagnostics.push(SvgDiagnostic {
                        code: "external_href".to_string(),
                        message: format!("attribute {name} must not reference external/data urls"),
                    });
                }
            }
        }
    }

    diagnostics = dedupe_diagnostics(diagnostics);
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }

    let uses_alpha = detect_alpha_usage(&doc);

    Ok(SanitizedSvgDocument {
        content: normalized,
        width,
        height,
        uses_alpha,
    })
}

pub fn run_svg_validate_command(
    input_path: &Path,
    output_path: &Path,
    dry_run: bool,
) -> anyhow::Result<SvgValidateCommandResult> {
    let raw = std::fs::read_to_string(input_path).map_err(|err| {
        util::usage_err(format!(
            "failed to read svg input {}: {err}",
            input_path.display()
        ))
    })?;

    match sanitize_svg_text(&raw) {
        Ok(doc) => {
            if !dry_run {
                util::ensure_parent_dir(output_path, false)?;
                std::fs::write(output_path, doc.content.as_bytes())?;
            }
            Ok(SvgValidateCommandResult {
                input_path: input_path.to_string_lossy().to_string(),
                output_path: output_path.to_string_lossy().to_string(),
                sanitized: true,
                diagnostics: Vec::new(),
                width: Some(doc.width),
                height: Some(doc.height),
            })
        }
        Err(diagnostics) => Ok(SvgValidateCommandResult {
            input_path: input_path.to_string_lossy().to_string(),
            output_path: output_path.to_string_lossy().to_string(),
            sanitized: false,
            diagnostics,
            width: None,
            height: None,
        }),
    }
}

pub fn render_svg_to_output(
    doc: &SanitizedSvgDocument,
    output_format: &str,
    output_path: &Path,
    raster_size_hint: RasterSizeHint,
    dry_run: bool,
) -> anyhow::Result<ImageInfo> {
    match output_format {
        "png" | "webp" | "jpg" => {
            let tree = resvg::usvg::Tree::from_str(&doc.content, &resvg::usvg::Options::default())
                .map_err(|err| anyhow::anyhow!("failed to parse sanitized svg: {err}"))?;
            let size = tree.size().to_int_size();
            let base_width = size.width();
            let base_height = size.height();
            let (width, height) =
                resolve_raster_dimensions(base_width, base_height, raster_size_hint)?;

            if !dry_run {
                util::ensure_parent_dir(output_path, false)?;
                let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)
                    .ok_or_else(|| anyhow::anyhow!("failed to allocate raster surface"))?;
                resvg::render(
                    &tree,
                    resvg::tiny_skia::Transform::from_scale(
                        width as f32 / base_width as f32,
                        height as f32 / base_height as f32,
                    ),
                    &mut pixmap.as_mut(),
                );

                let rgba = demultiply_rgba(&pixmap);
                match output_format {
                    "png" => write_png(output_path, width, height, &rgba)?,
                    "webp" => write_webp(output_path, width, height, &rgba)?,
                    "jpg" => write_jpg(output_path, width, height, &rgba)?,
                    _ => unreachable!(),
                }
            }
            output_info(
                output_format,
                width,
                height,
                output_format != "jpg" && doc.uses_alpha,
                output_path,
                dry_run,
            )
        }
        _ => Err(util::usage_err(
            "unsupported --to for convert (expected png|webp|jpg)",
        )),
    }
}

pub fn diagnostics_to_error(path: &Path, diagnostics: &[SvgDiagnostic]) -> anyhow::Error {
    let first = diagnostics
        .first()
        .map(|d| format!("{}: {}", d.code, d.message))
        .unwrap_or_else(|| "unknown svg validation failure".to_string());
    util::usage_err(format!(
        "invalid svg {}: {first} ({} issue(s))",
        path.display(),
        diagnostics.len()
    ))
}

fn output_info(
    output_format: &str,
    width: u32,
    height: u32,
    uses_alpha: bool,
    output_path: &Path,
    dry_run: bool,
) -> anyhow::Result<ImageInfo> {
    let format = match output_format {
        "png" => "PNG",
        "webp" => "WEBP",
        "jpg" => "JPEG",
        _ => "UNKNOWN",
    };

    let channels = if uses_alpha {
        Some("rgba".to_string())
    } else {
        Some("rgb".to_string())
    };

    let size_bytes = if dry_run {
        None
    } else {
        Some(std::fs::metadata(output_path)?.len())
    };

    Ok(ImageInfo {
        format: Some(format.to_string()),
        width: Some(width as i32),
        height: Some(height as i32),
        channels,
        alpha: Some(uses_alpha),
        exif_orientation: None,
        size_bytes,
    })
}

fn resolve_raster_dimensions(
    base_width: u32,
    base_height: u32,
    hint: RasterSizeHint,
) -> anyhow::Result<(u32, u32)> {
    if base_width == 0 || base_height == 0 {
        return Err(anyhow::anyhow!("sanitized svg has invalid dimensions"));
    }
    let dims = match (hint.width, hint.height) {
        (Some(width), Some(height)) => (width, height),
        (Some(width), None) => {
            let height = ((width as f64 * base_height as f64) / base_width as f64)
                .round()
                .max(1.0) as u32;
            (width, height)
        }
        (None, Some(height)) => {
            let width = ((height as f64 * base_width as f64) / base_height as f64)
                .round()
                .max(1.0) as u32;
            (width, height)
        }
        (None, None) => (base_width, base_height),
    };
    Ok((dims.0.max(1), dims.1.max(1)))
}

fn parse_view_box(raw: &str) -> Result<(u32, u32), String> {
    let parts = raw
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>();
    if parts.len() != 4 {
        return Err("viewBox must contain 4 numbers: minX minY width height".to_string());
    }
    let width = parts[2]
        .parse::<f64>()
        .map_err(|_| "viewBox width must be numeric".to_string())?;
    let height = parts[3]
        .parse::<f64>()
        .map_err(|_| "viewBox height must be numeric".to_string())?;
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return Err("viewBox width/height must be finite and > 0".to_string());
    }
    Ok((
        width.round().max(1.0) as u32,
        height.round().max(1.0) as u32,
    ))
}

fn normalize_svg_text(raw: &str) -> String {
    raw.replace('\r', "").trim().to_string()
}

fn detect_alpha_usage(doc: &roxmltree::Document<'_>) -> bool {
    for node in doc.descendants().filter(|n| n.is_element()) {
        for attr in node.attributes() {
            let key = attr.name().to_ascii_lowercase();
            let value = attr.value().trim().to_ascii_lowercase();

            if matches!(
                key.as_str(),
                "opacity" | "fill-opacity" | "stroke-opacity" | "stop-opacity"
            ) && value.parse::<f64>().ok().is_some_and(|v| v < 1.0)
            {
                return true;
            }

            if matches!(key.as_str(), "fill" | "stroke" | "stop-color") && color_has_alpha(&value) {
                return true;
            }
        }
    }
    false
}

fn color_has_alpha(value: &str) -> bool {
    let Some(hex) = value.strip_prefix('#') else {
        return false;
    };
    // Guard: only process pure-ASCII hex strings.  Non-ASCII bytes would
    // make `str::len()` return the *byte* count (not the char count), so a
    // 2-byte UTF-8 character could satisfy the length check while leaving
    // the byte-based slice indices on a continuation byte → panic.
    if !hex.is_ascii() {
        return false;
    }
    // After the ASCII guard, byte length == char length, so slicing is safe.
    match hex.len() {
        4 => !hex[3..4].eq_ignore_ascii_case("f"),
        8 => !hex[6..8].eq_ignore_ascii_case("ff"),
        _ => false,
    }
}

fn dedupe_diagnostics(mut diagnostics: Vec<SvgDiagnostic>) -> Vec<SvgDiagnostic> {
    let mut seen = BTreeSet::new();
    diagnostics.retain(|d| seen.insert((d.code.clone(), d.message.clone())));
    diagnostics
}

fn demultiply_rgba(pixmap: &resvg::tiny_skia::Pixmap) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((pixmap.width() * pixmap.height() * 4) as usize);
    for pixel in pixmap.pixels() {
        let c = pixel.demultiply();
        rgba.extend([c.red(), c.green(), c.blue(), c.alpha()]);
    }
    rgba
}

fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> anyhow::Result<()> {
    let file = File::create(path)?;
    let writer = BufWriter::new(file);
    let encoder = PngEncoder::new_with_quality(writer, PNG_COMPRESSION, PNG_FILTER);
    encoder
        .write_image(rgba, width, height, ExtendedColorType::Rgba8)
        .map_err(|err| anyhow::anyhow!("failed to encode png: {err}"))
}

fn write_webp(path: &Path, width: u32, height: u32, rgba: &[u8]) -> anyhow::Result<()> {
    let file = File::create(path)?;
    let writer = BufWriter::new(file);
    WebPEncoder::new_lossless(writer)
        .encode(rgba, width, height, ExtendedColorType::Rgba8)
        .map_err(|err| anyhow::anyhow!("failed to encode webp: {err}"))
}

fn write_jpg(path: &Path, width: u32, height: u32, rgba: &[u8]) -> anyhow::Result<()> {
    let file = File::create(path)?;
    let writer = BufWriter::new(file);
    let rgb = flatten_rgba_to_rgb(rgba);
    JpegEncoder::new_with_quality(writer, JPEG_QUALITY)
        .encode(&rgb, width, height, ExtendedColorType::Rgb8)
        .map_err(|err| anyhow::anyhow!("failed to encode jpg: {err}"))
}

fn flatten_rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity((rgba.len() / 4) * 3);
    for pixel in rgba.chunks_exact(4) {
        let alpha = pixel[3] as u16;
        let blend = |channel: u8| -> u8 {
            (((channel as u16 * alpha) + (255 * (255 - alpha)) + 127) / 255) as u8
        };
        rgb.extend([blend(pixel[0]), blend(pixel[1]), blend(pixel[2])]);
    }
    rgb
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_svg_text_accepts_simple_svg() {
        let svg = r#"<svg viewBox="0 0 32 32" xmlns="http://www.w3.org/2000/svg"><path d="M1 1L31 31"/></svg>"#;
        let doc = sanitize_svg_text(svg).unwrap();
        assert_eq!(doc.width, 32);
        assert_eq!(doc.height, 32);
    }

    #[test]
    fn sanitize_svg_text_rejects_script_and_missing_viewbox() {
        let invalid = r#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;
        let diagnostics = sanitize_svg_text(invalid).unwrap_err();
        assert!(diagnostics.iter().any(|d| d.code == "missing_viewbox"));
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "disallowed_tag" || d.code == "unsafe_tag")
        );
    }

    #[test]
    fn color_has_alpha_returns_true_for_4_and_8_digit_hex_with_alpha() {
        assert!(color_has_alpha("#fff0"));
        assert!(color_has_alpha("#FFF0"));
        assert!(color_has_alpha("#ffffff00"));
        assert!(color_has_alpha("#FFFFFF00"));
    }

    #[test]
    fn color_has_alpha_returns_false_for_opaque_and_non_hex() {
        assert!(!color_has_alpha("#ffff"));
        assert!(!color_has_alpha("#ffffffff"));
        assert!(!color_has_alpha("#fff"));
        assert!(!color_has_alpha("#ffffff"));
        assert!(!color_has_alpha("red"));
        assert!(!color_has_alpha(""));
    }

    #[test]
    fn color_has_alpha_does_not_panic_on_non_ascii_input() {
        // A 2-byte UTF-8 char (é = 0xC3 0xA9) after two ASCII chars gives
        // a 4-byte string that would previously match the len==4 arm and
        // panic when slicing at byte index 3 (a continuation byte).
        assert!(!color_has_alpha("#ffé"));
        assert!(!color_has_alpha("#🎨"));
        assert!(!color_has_alpha("#abcö1234"));
    }

    #[test]
    fn resolve_raster_dimensions_preserves_aspect_when_single_dimension_is_set() {
        let by_width = resolve_raster_dimensions(
            80,
            40,
            RasterSizeHint {
                width: Some(200),
                height: None,
            },
        )
        .unwrap();
        assert_eq!(by_width, (200, 100));

        let by_height = resolve_raster_dimensions(
            80,
            40,
            RasterSizeHint {
                width: None,
                height: Some(120),
            },
        )
        .unwrap();
        assert_eq!(by_height, (240, 120));
    }
}
