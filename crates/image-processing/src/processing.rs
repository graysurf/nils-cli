use crate::cli::Operation;
use crate::convert;
use crate::model::{ImageInfo, ItemResult, SCHEMA_VERSION, SourceContext, Summary, SummaryOptions};
use crate::report::render_report_md;
use crate::util;
use crate::{svg_validate, toolchain};
use nils_term::progress::Progress;
use std::path::{Path, PathBuf};

pub fn expand_inputs(inputs: &[String]) -> Result<Vec<PathBuf>, util::UsageError> {
    if inputs.is_empty() {
        return Err(util::UsageError {
            message: "missing --in".to_string(),
        });
    }

    let mut out = Vec::with_capacity(inputs.len());
    for raw in inputs {
        let expanded = util::expand_user(raw);
        if !expanded.exists() {
            return Err(util::UsageError {
                message: format!("input not found: {raw}"),
            });
        }
        if expanded.is_dir() {
            return Err(util::UsageError {
                message: format!("input is a directory: {raw}"),
            });
        }

        let canonicalized = std::fs::canonicalize(&expanded).map_err(|err| util::UsageError {
            message: format!("failed to resolve input: {raw}: {err}"),
        })?;
        out.push(canonicalized);
    }

    Ok(out)
}

pub struct ProcessArgs<'a> {
    pub repo_root: &'a Path,
    pub run_dir: Option<&'a Path>,
    pub progress: Progress,
    pub subcommand: Operation,
    pub input_path: &'a Path,
    pub output_path: &'a Path,
    pub convert_to: Option<&'a str>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub overwrite: bool,
    pub dry_run: bool,
    pub report_enabled: bool,
    pub json_enabled: bool,
}

pub fn process_items(args: ProcessArgs<'_>) -> anyhow::Result<Summary> {
    let ProcessArgs {
        repo_root,
        run_dir,
        progress,
        subcommand,
        input_path,
        output_path,
        convert_to,
        width,
        height,
        overwrite,
        dry_run,
        report_enabled,
        json_enabled,
    } = args;

    let _ = json_enabled;

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let input_abs = util::abs_path(input_path, &cwd);
    let output_abs = util::abs_path(output_path, &cwd);

    if subcommand == Operation::SvgValidate && ext_normalize(&output_abs) != "svg" {
        return Err(util::usage_err("svg-validate --out must end with .svg"));
    }

    let convert_target = if subcommand == Operation::Convert {
        Some(convert::parse_convert_target(convert_to)?)
    } else {
        None
    };

    if let Some(target) = convert_target {
        let ext = ext_normalize(&output_abs);
        if ext != target {
            return Err(util::usage_err(format!(
                "--out extension must match --to {target}: {}",
                output_abs.display()
            )));
        }
    }

    util::check_overwrite(&output_abs, overwrite)?;
    if report_enabled && let Some(run_dir) = run_dir {
        util::check_overwrite(&run_dir.join("report.md"), overwrite)?;
    }

    if !dry_run {
        util::ensure_parent_dir(&output_abs, false)?;
    }

    progress.set_message(util::maybe_relpath(&input_abs, repo_root));

    let input_rel = util::maybe_relpath(&input_abs, repo_root);
    let output_rel = util::maybe_relpath(&output_abs, repo_root);

    let mut commands: Vec<String> = Vec::new();
    let mut item_commands: Vec<String> = Vec::new();
    let mut item_error: Option<String> = None;
    let mut output_info: Option<ImageInfo> = None;
    let mut backend = toolchain::RUST_SVG_VALIDATE_BACKEND;
    let source: SourceContext;
    let mut input_info: ImageInfo;
    let input_size_bytes = std::fs::metadata(&input_abs).ok().map(|m| m.len());

    match subcommand {
        Operation::Convert => {
            let loaded = convert::LoadedInput::load(&input_abs)?;
            let target = convert_target.expect("convert target pre-validated");
            backend = loaded.backend();
            source = SourceContext {
                mode: loaded.source_mode().to_string(),
                input_path: Some(input_rel.clone()),
                input_format: Some(loaded.input_format().to_string()),
            };
            input_info = loaded.input_info(input_size_bytes);
            let to_raw = convert_to.unwrap_or(target);

            let mut cmd = vec![
                "image-processing".to_string(),
                "convert".to_string(),
                "--in".to_string(),
                input_rel.clone(),
                "--to".to_string(),
                to_raw.to_string(),
                "--out".to_string(),
                output_rel.clone(),
            ];
            if let Some(width) = width {
                cmd.extend(["--width".to_string(), width.to_string()]);
            }
            if let Some(height) = height {
                cmd.extend(["--height".to_string(), height.to_string()]);
            }
            if dry_run {
                cmd.push("--dry-run".to_string());
            }
            item_commands.push(util::command_str(&cmd));

            let render_result = convert_raster_size_hint(width, height)
                .and_then(|hint| loaded.render_to_output(target, &output_abs, hint, dry_run));

            match render_result {
                Ok(info) => {
                    if !dry_run {
                        output_info = Some(info);
                    }
                }
                Err(err) => {
                    item_error = Some(err.to_string());
                }
            }
        }
        Operation::SvgValidate => {
            source = SourceContext {
                mode: "svg".to_string(),
                input_path: Some(input_rel.clone()),
                input_format: Some("svg".to_string()),
            };
            input_info = ImageInfo {
                format: Some("SVG".to_string()),
                size_bytes: input_size_bytes,
                ..Default::default()
            };
            let mut cmd = vec![
                "image-processing".to_string(),
                "svg-validate".to_string(),
                "--in".to_string(),
                input_rel.clone(),
                "--out".to_string(),
                output_rel.clone(),
            ];
            if dry_run {
                cmd.push("--dry-run".to_string());
            }
            item_commands.push(util::command_str(&cmd));

            match svg_validate::run_svg_validate_command(&input_abs, &output_abs, dry_run) {
                Ok(validation) => {
                    input_info.width = validation.width.map(|width| width as i32);
                    input_info.height = validation.height.map(|height| height as i32);
                    input_info.alpha = Some(false);

                    if validation.sanitized {
                        if !dry_run {
                            output_info = Some(ImageInfo {
                                format: Some("SVG".to_string()),
                                width: validation.width.map(|width| width as i32),
                                height: validation.height.map(|height| height as i32),
                                channels: None,
                                alpha: Some(false),
                                exif_orientation: None,
                                size_bytes: std::fs::metadata(&output_abs).ok().map(|m| m.len()),
                            });
                        }
                    } else {
                        item_error = Some(
                            svg_validate::diagnostics_to_error(&input_abs, &validation.diagnostics)
                                .to_string(),
                        );
                    }
                }
                Err(err) => {
                    item_error = Some(err.to_string());
                }
            }
        }
    }

    for command in &item_commands {
        commands.push(command.clone());
    }

    let items = vec![ItemResult {
        input_path: input_rel,
        output_path: Some(output_rel),
        status: if item_error.is_some() {
            "error".to_string()
        } else {
            "ok".to_string()
        },
        input_info,
        output_info,
        commands: item_commands,
        warnings: Vec::new(),
        error: item_error,
    }];

    progress.inc(1);

    let mut report_path: Option<String> = None;
    if report_enabled && let Some(run_dir) = run_dir {
        let run_id = run_dir
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        let report_md = render_report_md(
            &run_id,
            subcommand.as_str(),
            &source,
            &items,
            &commands,
            dry_run,
        );
        let report_file = run_dir.join("report.md");
        std::fs::write(&report_file, report_md)?;
        report_path = Some(util::maybe_relpath(&report_file, repo_root));
    }

    let summary = Summary {
        schema_version: SCHEMA_VERSION,
        run_id: run_dir.and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().to_string())
        }),
        cwd: cwd.to_string_lossy().to_string(),
        operation: subcommand.as_str().to_string(),
        backend: backend.to_string(),
        source,
        report_path: report_path.clone(),
        dry_run,
        options: SummaryOptions {
            overwrite,
            auto_orient: None,
            strip_metadata: false,
            background: if convert_target == Some("jpg") {
                Some("white".to_string())
            } else {
                None
            },
            report: report_enabled,
        },
        commands,
        collisions: Vec::new(),
        skipped: Vec::new(),
        warnings: Vec::new(),
        items,
    };

    if let Some(run_dir) = run_dir {
        let summary_file = run_dir.join("summary.json");
        let json = serde_json::to_string_pretty(&summary)?;
        std::fs::write(&summary_file, json)?;
    }

    progress.finish_with_message("done");
    Ok(summary)
}

fn ext_normalize(path: &Path) -> String {
    let ext = path
        .extension()
        .map(|value| value.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if ext == "jpeg" {
        return "jpg".to_string();
    }
    ext
}

fn convert_raster_size_hint(
    width: Option<i32>,
    height: Option<i32>,
) -> anyhow::Result<svg_validate::RasterSizeHint> {
    Ok(svg_validate::RasterSizeHint {
        width: parse_positive_dimension(width, "--width")?,
        height: parse_positive_dimension(height, "--height")?,
    })
}

fn parse_positive_dimension(value: Option<i32>, flag: &str) -> anyhow::Result<Option<u32>> {
    match value {
        Some(value) if value > 0 => Ok(Some(value as u32)),
        Some(_) => Err(util::usage_err(format!("convert {flag} must be > 0"))),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::{expand_inputs, ext_normalize, parse_positive_dimension};
    use std::path::Path;

    #[test]
    fn expand_inputs_requires_existing_file_paths() {
        let dir = tempfile::TempDir::new().unwrap();
        let svg = dir.path().join("icon.svg");
        std::fs::write(&svg, "<svg viewBox=\"0 0 1 1\"/>").unwrap();

        let out = expand_inputs(&[svg.to_string_lossy().to_string()]).unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].ends_with("icon.svg"));

        let err = expand_inputs(&[]).unwrap_err();
        assert!(err.to_string().contains("missing --in"));

        let err = expand_inputs(&[dir.path().join("missing.svg").to_string_lossy().to_string()])
            .unwrap_err();
        assert!(err.to_string().contains("input not found"));

        let err = expand_inputs(&[dir.path().to_string_lossy().to_string()]).unwrap_err();
        assert!(err.to_string().contains("input is a directory"));
    }

    #[test]
    fn parse_positive_dimension_enforces_gt_zero() {
        assert_eq!(parse_positive_dimension(None, "--width").unwrap(), None);
        assert_eq!(
            parse_positive_dimension(Some(128), "--width").unwrap(),
            Some(128)
        );

        let err = parse_positive_dimension(Some(0), "--width").unwrap_err();
        assert!(err.to_string().contains("--width must be > 0"));
    }

    #[test]
    fn ext_normalize_normalizes_jpeg_extension() {
        assert_eq!(ext_normalize(Path::new("out/icon.jpeg")), "jpg");
        assert_eq!(ext_normalize(Path::new("out/icon.PNG")), "png");
        assert_eq!(ext_normalize(Path::new("out/icon")), "");
    }
}
