use crate::model::ImageInfo;
use crate::svg_validate::{self, RasterSizeHint, SanitizedSvgDocument};
use crate::{toolchain, util};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType as PngCompression, FilterType as PngFilter, PngEncoder};
use image::codecs::webp::WebPEncoder;
use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{
    DynamicImage, ExtendedColorType, ImageDecoder, ImageEncoder, ImageFormat, ImageReader,
};
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

/// Default JPEG quality applied when `--quality` is not supplied. Shared with the
/// SVG-render path so both convert backends encode JPEG identically by default.
pub const DEFAULT_JPEG_QUALITY: u8 = 90;
const PNG_COMPRESSION: PngCompression = PngCompression::Best;
const PNG_FILTER: PngFilter = PngFilter::NoFilter;
const SUPPORTED_CONVERT_INPUT_FORMATS: &str = "svg|png|jpg|jpeg|webp";

pub enum LoadedInput {
    Svg(SanitizedSvgDocument),
    Raster(RasterInput),
}

pub struct RasterInput {
    image: DynamicImage,
    format: &'static str,
    orientation: Orientation,
}

impl LoadedInput {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        if normalize_ext(path) == "svg" {
            return Ok(Self::Svg(svg_validate::sanitize_svg_file(path)?));
        }

        let reader = ImageReader::open(path)
            .map_err(|err| anyhow::anyhow!("failed to open input {}: {err}", path.display()))?;
        let reader = reader.with_guessed_format().map_err(|err| {
            anyhow::anyhow!(
                "failed to detect input format for {}: {err}",
                path.display()
            )
        })?;
        let Some(format) = reader.format() else {
            return Err(util::usage_err(format!(
                "unsupported convert input format (expected {SUPPORTED_CONVERT_INPUT_FORMATS}): {}",
                path.display()
            )));
        };

        let normalized = match format {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
            ImageFormat::WebP => "webp",
            _ => {
                return Err(util::usage_err(format!(
                    "unsupported convert input format (expected {SUPPORTED_CONVERT_INPUT_FORMATS}): {}",
                    path.display()
                )));
            }
        };

        // Decode through the explicit decoder so we can read EXIF orientation
        // and apply it before any sizing/encoding. Without this, portrait phone
        // photos (orientation 6/8) transcode rotated.
        let mut decoder = reader
            .into_decoder()
            .map_err(|err| anyhow::anyhow!("failed to decode input {}: {err}", path.display()))?;
        let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
        let mut image = DynamicImage::from_decoder(decoder)
            .map_err(|err| anyhow::anyhow!("failed to decode input {}: {err}", path.display()))?;
        image.apply_orientation(orientation);

        Ok(Self::Raster(RasterInput {
            image,
            format: normalized,
            orientation,
        }))
    }

    pub fn backend(&self) -> &'static str {
        match self {
            Self::Svg(_) => toolchain::RUST_CONVERT_SVG_BACKEND,
            Self::Raster(_) => toolchain::RUST_CONVERT_RASTER_BACKEND,
        }
    }

    pub fn source_mode(&self) -> &'static str {
        match self {
            Self::Svg(_) => "svg",
            Self::Raster(_) => "raster",
        }
    }

    pub fn input_format(&self) -> &'static str {
        match self {
            Self::Svg(_) => "svg",
            Self::Raster(raster) => raster.format,
        }
    }

    pub fn input_info(&self, size_bytes: Option<u64>) -> ImageInfo {
        match self {
            Self::Svg(doc) => ImageInfo {
                format: Some("SVG".to_string()),
                width: Some(doc.width as i32),
                height: Some(doc.height as i32),
                channels: None,
                alpha: Some(doc.uses_alpha),
                exif_orientation: None,
                size_bytes,
            },
            Self::Raster(raster) => {
                let has_alpha = raster.image.color().has_alpha();
                ImageInfo {
                    format: Some(display_format(raster.format).to_string()),
                    width: Some(raster.image.width() as i32),
                    height: Some(raster.image.height() as i32),
                    channels: Some(if has_alpha { "rgba" } else { "rgb" }.to_string()),
                    alpha: Some(has_alpha),
                    exif_orientation: orientation_report(raster.orientation),
                    size_bytes,
                }
            }
        }
    }

    pub fn render_to_output(
        &self,
        output_format: &str,
        output_path: &Path,
        raster_size_hint: RasterSizeHint,
        jpeg_quality: u8,
        dry_run: bool,
    ) -> anyhow::Result<ImageInfo> {
        match self {
            Self::Svg(doc) => svg_validate::render_svg_to_output(
                doc,
                output_format,
                output_path,
                raster_size_hint,
                jpeg_quality,
                dry_run,
            ),
            Self::Raster(raster) => raster.render_to_output(
                output_format,
                output_path,
                raster_size_hint,
                jpeg_quality,
                dry_run,
            ),
        }
    }
}

impl RasterInput {
    fn render_to_output(
        &self,
        output_format: &str,
        output_path: &Path,
        raster_size_hint: RasterSizeHint,
        jpeg_quality: u8,
        dry_run: bool,
    ) -> anyhow::Result<ImageInfo> {
        let base_width = self.image.width();
        let base_height = self.image.height();
        let (width, height) = resolve_raster_dimensions(base_width, base_height, raster_size_hint)?;
        let rendered = if width == base_width && height == base_height {
            self.image.clone()
        } else {
            self.image.resize_exact(width, height, FilterType::Lanczos3)
        };

        if !dry_run {
            util::ensure_parent_dir(output_path, false)?;
            match output_format {
                "png" => write_png(output_path, &rendered)?,
                "webp" => write_webp(output_path, &rendered)?,
                "jpg" => write_jpg(output_path, &rendered, jpeg_quality)?,
                _ => {
                    return Err(util::usage_err(
                        "unsupported --to for convert (expected png|webp|jpg)",
                    ));
                }
            }
        }

        output_info(
            output_format,
            width,
            height,
            output_format != "jpg" && rendered.color().has_alpha(),
            output_path,
            dry_run,
        )
    }
}

pub fn normalize_convert_target(raw: &str) -> Option<&'static str> {
    match raw {
        "png" => Some("png"),
        "webp" => Some("webp"),
        "jpg" | "jpeg" => Some("jpg"),
        _ => None,
    }
}

pub fn parse_convert_target(raw: Option<&str>) -> anyhow::Result<&'static str> {
    let Some(target) = raw else {
        return Err(util::usage_err("convert requires --to png|webp|jpg"));
    };

    normalize_convert_target(target)
        .ok_or_else(|| util::usage_err("convert --to must be one of: png|webp|jpg"))
}

fn output_info(
    output_format: &str,
    width: u32,
    height: u32,
    has_alpha: bool,
    output_path: &Path,
    dry_run: bool,
) -> anyhow::Result<ImageInfo> {
    Ok(ImageInfo {
        format: Some(display_format(output_format).to_string()),
        width: Some(width as i32),
        height: Some(height as i32),
        channels: Some(if has_alpha { "rgba" } else { "rgb" }.to_string()),
        alpha: Some(has_alpha),
        exif_orientation: None,
        size_bytes: if dry_run {
            None
        } else {
            Some(std::fs::metadata(output_path)?.len())
        },
    })
}

fn display_format(raw: &str) -> &'static str {
    match raw {
        "png" => "PNG",
        "webp" => "WEBP",
        "jpg" => "JPEG",
        "svg" => "SVG",
        _ => "UNKNOWN",
    }
}

fn resolve_raster_dimensions(
    base_width: u32,
    base_height: u32,
    hint: RasterSizeHint,
) -> anyhow::Result<(u32, u32)> {
    if base_width == 0 || base_height == 0 {
        return Err(anyhow::anyhow!("input image has invalid dimensions"));
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

fn write_png(path: &Path, image: &DynamicImage) -> anyhow::Result<()> {
    let rgba = image.to_rgba8();
    let file = File::create(path)?;
    let writer = BufWriter::new(file);
    let encoder = PngEncoder::new_with_quality(writer, PNG_COMPRESSION, PNG_FILTER);
    encoder
        .write_image(
            &rgba,
            image.width(),
            image.height(),
            ExtendedColorType::Rgba8,
        )
        .map_err(|err| anyhow::anyhow!("failed to encode png: {err}"))
}

fn write_webp(path: &Path, image: &DynamicImage) -> anyhow::Result<()> {
    let rgba = image.to_rgba8();
    let file = File::create(path)?;
    let writer = BufWriter::new(file);
    WebPEncoder::new_lossless(writer)
        .encode(
            &rgba,
            image.width(),
            image.height(),
            ExtendedColorType::Rgba8,
        )
        .map_err(|err| anyhow::anyhow!("failed to encode webp: {err}"))
}

fn write_jpg(path: &Path, image: &DynamicImage, quality: u8) -> anyhow::Result<()> {
    let rgb = flatten_to_rgb(image);
    let file = File::create(path)?;
    let writer = BufWriter::new(file);
    JpegEncoder::new_with_quality(writer, quality)
        .encode(&rgb, image.width(), image.height(), ExtendedColorType::Rgb8)
        .map_err(|err| anyhow::anyhow!("failed to encode jpg: {err}"))
}

/// Report the applied EXIF orientation as its canonical Exif code (2-8) when a
/// transform was applied, or `None` for `NoTransforms` so unoriented images
/// stay backward compatible (the field was always `None` before).
fn orientation_report(orientation: Orientation) -> Option<String> {
    match orientation {
        Orientation::NoTransforms => None,
        other => Some(other.to_exif().to_string()),
    }
}

fn flatten_to_rgb(image: &DynamicImage) -> Vec<u8> {
    if !image.color().has_alpha() {
        return image.to_rgb8().into_raw();
    }

    let rgba = image.to_rgba8();
    let mut rgb = Vec::with_capacity((image.width() * image.height() * 3) as usize);
    for pixel in rgba.pixels() {
        let alpha = pixel[3] as u16;
        let blend = |channel: u8| -> u8 {
            (((channel as u16 * alpha) + (255 * (255 - alpha)) + 127) / 255) as u8
        };
        rgb.extend([blend(pixel[0]), blend(pixel[1]), blend(pixel[2])]);
    }
    rgb
}

fn normalize_ext(path: &Path) -> String {
    let ext = path
        .extension()
        .map(|value| value.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if ext == "jpeg" {
        return "jpg".to_string();
    }
    ext
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_convert_target_accepts_expected_values() {
        assert_eq!(normalize_convert_target("png"), Some("png"));
        assert_eq!(normalize_convert_target("webp"), Some("webp"));
        assert_eq!(normalize_convert_target("jpg"), Some("jpg"));
        assert_eq!(normalize_convert_target("jpeg"), Some("jpg"));
        assert_eq!(normalize_convert_target("gif"), None);
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

    #[test]
    fn jpg_quality_changes_output_size() {
        use image::{Rgb, RgbImage};

        // A gradient gives the DCT real content to compress, so quality matters.
        let mut buffer = RgbImage::new(64, 64);
        for (x, y, px) in buffer.enumerate_pixels_mut() {
            *px = Rgb([(x * 4) as u8, (y * 4) as u8, ((x ^ y) * 3) as u8]);
        }
        let img = DynamicImage::ImageRgb8(buffer);

        let dir = tempfile::TempDir::new().unwrap();
        let low = dir.path().join("low.jpg");
        let high = dir.path().join("high.jpg");
        write_jpg(&low, &img, 20).unwrap();
        write_jpg(&high, &img, 95).unwrap();

        let low_size = std::fs::metadata(&low).unwrap().len();
        let high_size = std::fs::metadata(&high).unwrap().len();
        assert!(
            low_size < high_size,
            "quality 20 ({low_size} bytes) should be smaller than quality 95 ({high_size} bytes)"
        );
    }

    #[test]
    fn load_applies_exif_orientation() {
        use image::{ImageFormat, Rgb, RgbImage};
        use std::io::Cursor;

        // 4x2 solid JPEG with an EXIF APP1 (orientation 6 = Rotate90) injected
        // right after the SOI marker.
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 2, Rgb([10, 120, 200])));
        let mut jpeg = Vec::new();
        img.write_to(&mut Cursor::new(&mut jpeg), ImageFormat::Jpeg)
            .unwrap();

        let app1 = exif_app1_orientation(6);
        let mut with_exif = Vec::with_capacity(jpeg.len() + app1.len());
        with_exif.extend_from_slice(&jpeg[..2]); // SOI (0xFFD8)
        with_exif.extend_from_slice(&app1);
        with_exif.extend_from_slice(&jpeg[2..]);

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("rot.jpg");
        std::fs::write(&path, &with_exif).unwrap();

        let loaded = LoadedInput::load(&path).expect("load oriented jpeg");
        let info = loaded.input_info(None);
        // Orientation 6 rotates 90deg clockwise: 4x2 logical -> 2x4.
        assert_eq!(info.width, Some(2));
        assert_eq!(info.height, Some(4));
        assert_eq!(info.exif_orientation.as_deref(), Some("6"));
    }

    fn exif_app1_orientation(orientation: u8) -> Vec<u8> {
        // Little-endian TIFF with a single IFD entry: Orientation (0x0112),
        // type SHORT (3), count 1, value `orientation`.
        let tiff: [u8; 26] = [
            0x49,
            0x49, // "II" little-endian byte order
            0x2A,
            0x00, // TIFF magic (42)
            0x08,
            0x00,
            0x00,
            0x00, // offset to IFD0
            0x01,
            0x00, // entry count
            0x12,
            0x01, // tag 0x0112 (Orientation)
            0x03,
            0x00, // type SHORT
            0x01,
            0x00,
            0x00,
            0x00, // count 1
            orientation,
            0x00,
            0x00,
            0x00, // value (left-aligned, padded)
            0x00,
            0x00,
            0x00,
            0x00, // next-IFD offset (none)
        ];
        let mut payload = Vec::new();
        payload.extend_from_slice(b"Exif\x00\x00");
        payload.extend_from_slice(&tiff);
        let len = (payload.len() + 2) as u16; // include the 2-byte length field itself
        let mut app1 = vec![0xFF, 0xE1, (len >> 8) as u8, (len & 0xFF) as u8];
        app1.extend_from_slice(&payload);
        app1
    }
}
