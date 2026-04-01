use crate::model::ImageInfo;
use crate::svg_validate::{self, RasterSizeHint, SanitizedSvgDocument};
use crate::{toolchain, util};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType as PngCompression, FilterType as PngFilter, PngEncoder};
use image::codecs::webp::WebPEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ExtendedColorType, ImageEncoder, ImageFormat, ImageReader};
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

const JPEG_QUALITY: u8 = 90;
const PNG_COMPRESSION: PngCompression = PngCompression::Best;
const PNG_FILTER: PngFilter = PngFilter::NoFilter;

pub enum LoadedInput {
    Svg(SanitizedSvgDocument),
    Raster(RasterInput),
}

pub struct RasterInput {
    image: DynamicImage,
    format: &'static str,
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
                "unsupported convert input format (expected svg|png|jpg|webp): {}",
                path.display()
            )));
        };

        let normalized = match format {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
            ImageFormat::WebP => "webp",
            _ => {
                return Err(util::usage_err(format!(
                    "unsupported convert input format (expected svg|png|jpg|webp): {}",
                    path.display()
                )));
            }
        };

        let image = reader
            .decode()
            .map_err(|err| anyhow::anyhow!("failed to decode input {}: {err}", path.display()))?;

        Ok(Self::Raster(RasterInput {
            image,
            format: normalized,
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
                    exif_orientation: None,
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
        dry_run: bool,
    ) -> anyhow::Result<ImageInfo> {
        match self {
            Self::Svg(doc) => svg_validate::render_svg_to_output(
                doc,
                output_format,
                output_path,
                raster_size_hint,
                dry_run,
            ),
            Self::Raster(raster) => {
                raster.render_to_output(output_format, output_path, raster_size_hint, dry_run)
            }
        }
    }
}

impl RasterInput {
    fn render_to_output(
        &self,
        output_format: &str,
        output_path: &Path,
        raster_size_hint: RasterSizeHint,
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
                "jpg" => write_jpg(output_path, &rendered)?,
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

fn write_jpg(path: &Path, image: &DynamicImage) -> anyhow::Result<()> {
    let rgb = flatten_to_rgb(image);
    let file = File::create(path)?;
    let writer = BufWriter::new(file);
    JpegEncoder::new_with_quality(writer, JPEG_QUALITY)
        .encode(&rgb, image.width(), image.height(), ExtendedColorType::Rgb8)
        .map_err(|err| anyhow::anyhow!("failed to encode jpg: {err}"))
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
}
