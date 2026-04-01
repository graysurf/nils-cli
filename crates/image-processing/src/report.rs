use crate::model::{ItemResult, SourceContext};

pub fn render_report_md(
    run_id: &str,
    subcommand: &str,
    source: &SourceContext,
    items: &[ItemResult],
    commands: &[String],
    dry_run: bool,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("# Image Processing Report ({run_id})"));
    lines.push(String::new());
    lines.push(format!("- Operation: `{subcommand}`"));
    lines.push(format!("- Source mode: `{}`", source.mode));
    if let Some(input_path) = source.input_path.as_deref() {
        lines.push(format!("- Source input: `{input_path}`"));
    }
    if let Some(input_format) = source.input_format.as_deref() {
        lines.push(format!("- Source format: `{input_format}`"));
    }
    lines.push(format!(
        "- Dry run: `{}`",
        if dry_run { "true" } else { "false" }
    ));
    lines.push(String::new());

    lines.push("## Commands".to_string());
    for c in commands {
        lines.push(format!("- `{c}`"));
    }
    lines.push(String::new());

    lines.push("## Results".to_string());
    for item in items {
        let status = item.status.as_str();
        let inp = item.input_path.as_str();
        let outp = item.output_path.as_deref().unwrap_or("None");
        lines.push(format!("- `{status}`: `{inp}` -> `{outp}`"));

        let in_size = item.input_info.size_bytes;
        let out_size = item.output_info.as_ref().and_then(|x| x.size_bytes);

        if let Some(bytes) = in_size {
            lines.push(format!("  - input_bytes: {bytes}"));
        }
        if let Some(bytes) = out_size {
            lines.push(format!("  - output_bytes: {bytes}"));
        }
        if let (Some(in_b), Some(out_b)) = (in_size, out_size)
            && in_b > 0
        {
            let delta = out_b as i64 - in_b as i64;
            let pct = (delta as f64 / in_b as f64) * 100.0;
            lines.push(format!("  - delta_bytes: {delta} ({pct:.2}%)"));
        }
        if let Some(err) = item.error.as_deref() {
            lines.push(format!("  - error: {err}"));
        }
    }
    lines.push(String::new());

    format!("{}\n", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ImageInfo;

    #[test]
    fn report_renders_commands_and_item_deltas() {
        let input_info = ImageInfo {
            size_bytes: Some(100),
            ..Default::default()
        };
        let output_info = ImageInfo {
            size_bytes: Some(80),
            ..Default::default()
        };

        let items = vec![ItemResult {
            input_path: "in.png".to_string(),
            output_path: Some("out.png".to_string()),
            status: "ok".to_string(),
            input_info,
            output_info: Some(output_info),
            commands: Vec::new(),
            warnings: Vec::new(),
            error: None,
        }];

        let out = render_report_md(
            "run123",
            "resize",
            &SourceContext {
                mode: "inputs".to_string(),
                input_path: Some("in.png".to_string()),
                input_format: Some("png".to_string()),
            },
            &items,
            &["magick in.png out.png".to_string()],
            true,
        );

        assert!(out.contains("# Image Processing Report (run123)"));
        assert!(out.contains("- Operation: `resize`"));
        assert!(out.contains("- Source mode: `inputs`"));
        assert!(out.contains("- Source input: `in.png`"));
        assert!(out.contains("- Source format: `png`"));
        assert!(out.contains("- Dry run: `true`"));
        assert!(out.contains("## Commands"));
        assert!(out.contains("- `magick in.png out.png`"));
        assert!(out.contains("`ok`: `in.png` -> `out.png`"));
        assert!(out.contains("input_bytes: 100"));
        assert!(out.contains("output_bytes: 80"));
        assert!(out.contains("delta_bytes: -20"));
    }

    #[test]
    fn report_renders_errors_and_none_output_paths() {
        let input_info = ImageInfo {
            size_bytes: Some(10),
            ..Default::default()
        };

        let items = vec![ItemResult {
            input_path: "a.jpg".to_string(),
            output_path: None,
            status: "error".to_string(),
            input_info,
            output_info: None,
            commands: Vec::new(),
            warnings: Vec::new(),
            error: Some("boom".to_string()),
        }];

        let out = render_report_md(
            "run456",
            "convert",
            &SourceContext {
                mode: "raster".to_string(),
                input_path: Some("fixtures/demo.jpg".to_string()),
                input_format: Some("jpg".to_string()),
            },
            &items,
            &[],
            false,
        );

        assert!(out.contains("- Dry run: `false`"));
        assert!(out.contains("- Source input: `fixtures/demo.jpg`"));
        assert!(out.contains("- Source format: `jpg`"));
        assert!(out.contains("`error`: `a.jpg` -> `None`"));
        assert!(out.contains("error: boom"));
    }
}
