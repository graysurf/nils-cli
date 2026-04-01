use serde::Serialize;

pub const SCHEMA_VERSION: i32 = 1;

#[derive(Clone, Debug, Default, Serialize)]
pub struct ImageInfo {
    pub format: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub channels: Option<String>,
    pub alpha: Option<bool>,
    pub exif_orientation: Option<String>,
    pub size_bytes: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Collision {
    pub path: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct SourceContext {
    pub mode: String,
    pub input_path: Option<String>,
    pub input_format: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SummaryOptions {
    pub overwrite: bool,
    pub auto_orient: Option<bool>,
    pub strip_metadata: bool,
    pub background: Option<String>,
    pub report: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ItemResult {
    pub input_path: String,
    pub output_path: Option<String>,
    pub status: String,
    pub input_info: ImageInfo,
    pub output_info: Option<ImageInfo>,
    pub commands: Vec<String>,
    pub warnings: Vec<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Summary {
    pub schema_version: i32,
    pub run_id: Option<String>,
    pub cwd: String,
    pub operation: String,
    pub backend: String,
    pub source: SourceContext,
    pub report_path: Option<String>,
    pub dry_run: bool,
    pub options: SummaryOptions,
    pub commands: Vec<String>,
    pub collisions: Vec<Collision>,
    pub skipped: Vec<serde_json::Value>,
    pub warnings: Vec<String>,
    pub items: Vec<ItemResult>,
}
