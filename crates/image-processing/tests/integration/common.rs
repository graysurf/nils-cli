#![allow(dead_code)]

use image::{DynamicImage, ImageBuffer, Rgb, RgbImage, Rgba, RgbaImage};
use nils_test_support::cmd;
use std::path::Path;

use nils_test_support::StubBinDir;

pub struct CmdOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub fn run_image_processing(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> CmdOutput {
    let output = cmd::run_resolved_in_dir("image-processing", dir, args, envs, None);
    CmdOutput {
        code: output.code,
        stdout: output.stdout_text(),
        stderr: output.stderr_text(),
    }
}

#[allow(dead_code)]
pub fn make_stub_dir() -> StubBinDir {
    StubBinDir::new()
}

pub fn write_sample_png(path: &Path) {
    sample_rgba_image()
        .save_with_format(path, image::ImageFormat::Png)
        .unwrap();
}

pub fn write_sample_webp(path: &Path) {
    sample_rgba_image()
        .save_with_format(path, image::ImageFormat::WebP)
        .unwrap();
}

pub fn write_sample_jpg(path: &Path) {
    sample_rgb_image()
        .save_with_format(path, image::ImageFormat::Jpeg)
        .unwrap();
}

fn sample_rgba_image() -> DynamicImage {
    let image: RgbaImage = ImageBuffer::from_fn(80, 60, |x, y| {
        let alpha = if (x + y) % 7 == 0 { 180 } else { 255 };
        Rgba([
            ((x * 3) % 255) as u8,
            ((y * 5) % 255) as u8,
            (((x + y) * 2) % 255) as u8,
            alpha,
        ])
    });
    DynamicImage::ImageRgba8(image)
}

fn sample_rgb_image() -> DynamicImage {
    let image: RgbImage = ImageBuffer::from_fn(80, 60, |x, y| {
        Rgb([
            ((x * 3) % 255) as u8,
            ((y * 5) % 255) as u8,
            (((x + y) * 2) % 255) as u8,
        ])
    });
    DynamicImage::ImageRgb8(image)
}
