use clap::ValueEnum;
use screen_record::types::{PermissionState, PermissionStatusSchema};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{CliError, ErrorCategory};
use crate::screen_record_adapter::{AppInfo, WindowInfo};

#[derive(Debug, Clone, Serialize)]
pub struct SuccessEnvelope<T>
where
    T: Serialize,
{
    pub schema_version: u8,
    pub ok: bool,
    pub command: &'static str,
    pub result: T,
}

impl<T> SuccessEnvelope<T>
where
    T: Serialize,
{
    pub fn new(command: &'static str, result: T) -> Self {
        Self {
            schema_version: 1,
            ok: true,
            command,
            result,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorEnvelope {
    pub schema_version: u8,
    pub ok: bool,
    pub error: ErrorResult,
}

impl ErrorEnvelope {
    pub fn from_error(err: &CliError) -> Self {
        Self {
            schema_version: 1,
            ok: false,
            error: ErrorResult::from(err),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorResult {
    pub category: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
}

impl From<&CliError> for ErrorResult {
    fn from(err: &CliError) -> Self {
        let category = match err.category() {
            ErrorCategory::Usage => "usage",
            ErrorCategory::Runtime => "runtime",
        };
        Self {
            category,
            operation: err.operation().map(str::to_string),
            message: err.message().to_string(),
            hints: err.hints().to_vec(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionStateResult {
    Ready,
    Blocked,
    Unknown,
}

impl From<PermissionState> for PermissionStateResult {
    fn from(value: PermissionState) -> Self {
        match value {
            PermissionState::Ready => Self::Ready,
            PermissionState::Blocked => Self::Blocked,
            PermissionState::Unknown => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PermissionStatusResult {
    pub screen_recording: PermissionStateResult,
    pub accessibility: PermissionStateResult,
    pub automation: PermissionStateResult,
    pub ready: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
}

impl From<&PermissionStatusSchema> for PermissionStatusResult {
    fn from(value: &PermissionStatusSchema) -> Self {
        Self {
            screen_recording: value.screen_recording.into(),
            accessibility: value.accessibility.into(),
            automation: value.automation.into(),
            ready: value.ready,
            hints: value.hints.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ActionMeta {
    pub action_id: String,
    pub elapsed_ms: u64,
    pub dry_run: bool,
    pub retries: u8,
    pub attempts_used: u8,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ActionPolicyResult {
    pub dry_run: bool,
    pub retries: u8,
    pub retry_delay_ms: u64,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WindowRow {
    pub window_id: u32,
    pub owner_name: String,
    pub window_title: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub on_screen: bool,
    pub active: bool,
    pub owner_pid: i32,
    pub z_order: usize,
}

impl From<&WindowInfo> for WindowRow {
    fn from(window: &WindowInfo) -> Self {
        Self {
            window_id: window.id,
            owner_name: window.owner_name.clone(),
            window_title: window.title.clone(),
            x: window.bounds.x,
            y: window.bounds.y,
            width: window.bounds.width,
            height: window.bounds.height,
            on_screen: window.on_screen,
            active: window.active,
            owner_pid: window.owner_pid,
            z_order: window.z_order,
        }
    }
}

impl WindowRow {
    pub fn tsv_line(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            self.window_id,
            normalize_tsv_field(&self.owner_name),
            normalize_tsv_field(&self.window_title),
            self.x,
            self.y,
            self.width,
            self.height,
            if self.on_screen { "true" } else { "false" }
        )
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AppRow {
    pub app_name: String,
    pub pid: i32,
    pub bundle_id: String,
}

impl From<&AppInfo> for AppRow {
    fn from(app: &AppInfo) -> Self {
        Self {
            app_name: app.name.clone(),
            pid: app.pid,
            bundle_id: app.bundle_id.clone(),
        }
    }
}

impl AppRow {
    pub fn tsv_line(&self) -> String {
        format!(
            "{}\t{}\t{}",
            normalize_tsv_field(&self.app_name),
            self.pid,
            normalize_tsv_field(&self.bundle_id)
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ListWindowsResult {
    pub windows: Vec<WindowRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListAppsResult {
    pub apps: Vec<AppRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScreenshotResult {
    pub path: String,
    pub target: WindowRow,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector: Option<ScreenshotSelectorResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub if_changed: Option<IfChangedResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WaitResult {
    pub condition: &'static str,
    pub attempts: u32,
    pub elapsed_ms: u64,
    pub terminal_status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector_explain: Option<AxSelectorExplain>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScreenshotSelectorResult {
    pub node_id: String,
    pub matched_count: usize,
    pub padding: i32,
    pub frame: AxFrame,
    pub capture_region: AxFrame,
}

#[derive(Debug, Clone, Serialize)]
pub struct IfChangedResult {
    pub changed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_hash: Option<String>,
    pub current_hash: String,
    pub threshold: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captured_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WindowActivateResult {
    pub selected_app: String,
    pub selected_window_id: Option<u32>,
    pub wait_ms: Option<u64>,
    pub policy: ActionPolicyResult,
    pub meta: ActionMeta,
}

#[derive(Debug, Clone, Serialize)]
pub struct InputClickResult {
    pub x: i32,
    pub y: i32,
    pub button: &'static str,
    pub count: u8,
    pub mods: Vec<String>,
    pub policy: ActionPolicyResult,
    pub meta: ActionMeta,
}

#[derive(Debug, Clone, Serialize)]
pub struct InputMoveResult {
    pub x: i32,
    pub y: i32,
    pub policy: ActionPolicyResult,
    pub meta: ActionMeta,
}

#[derive(Debug, Clone, Serialize)]
pub struct InputDragResult {
    pub from_x: i32,
    pub from_y: i32,
    pub to_x: i32,
    pub to_y: i32,
    pub duration_ms: u64,
    pub steps: u16,
    pub mods: Vec<String>,
    pub policy: ActionPolicyResult,
    pub meta: ActionMeta,
}

#[derive(Debug, Clone, Serialize)]
pub struct InputScrollResult {
    pub delta_x: i32,
    pub delta_y: i32,
    pub unit: &'static str,
    pub mods: Vec<String>,
    pub policy: ActionPolicyResult,
    pub meta: ActionMeta,
}

#[derive(Debug, Clone, Serialize)]
pub struct InputTypeResult {
    pub text_length: usize,
    pub enter: bool,
    pub delay_ms: Option<u64>,
    pub policy: ActionPolicyResult,
    pub meta: ActionMeta,
}

#[derive(Debug, Clone, Serialize)]
pub struct InputKeyResult {
    pub key: String,
    pub count: u8,
    pub policy: ActionPolicyResult,
    pub meta: ActionMeta,
}

#[derive(Debug, Clone, Serialize)]
pub struct InputHotkeyResult {
    pub mods: Vec<String>,
    pub key: String,
    pub policy: ActionPolicyResult,
    pub meta: ActionMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AxFrame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AxNode {
    pub node_id: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subrole: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_preview: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub focused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame: Option<AxFrame>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AxTarget {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_title_contains: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum AxMatchStrategy {
    #[default]
    Contains,
    Exact,
    Prefix,
    Suffix,
    Regex,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum AxClickFallbackStage {
    AxPress,
    AxConfirm,
    FrameCenter,
    Coordinate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AxSelectorExplainStage {
    pub stage: String,
    pub before_count: usize,
    pub after_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AxSelectorExplain {
    pub strategy: AxMatchStrategy,
    pub total_candidates: usize,
    pub matched_count: usize,
    pub selected_count: usize,
    pub stage_results: Vec<AxSelectorExplainStage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_node_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AxSelector {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_contains: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identifier_contains: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_contains: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subrole: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focused: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nth: Option<usize>,
    #[serde(default, skip_serializing_if = "is_match_strategy_contains")]
    pub match_strategy: AxMatchStrategy,
    #[serde(default, skip_serializing_if = "is_false")]
    pub explain: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AxListRequest {
    #[serde(default)]
    pub target: AxTarget,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_contains: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identifier_contains: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_contains: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subrole: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focused: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AxClickRequest {
    #[serde(default)]
    pub target: AxTarget,
    #[serde(default)]
    pub selector: AxSelector,
    #[serde(default)]
    pub allow_coordinate_fallback: bool,
    #[serde(default)]
    pub reselect_before_click: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_order: Vec<AxClickFallbackStage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AxTypeRequest {
    #[serde(default)]
    pub target: AxTarget,
    #[serde(default)]
    pub selector: AxSelector,
    pub text: String,
    #[serde(default)]
    pub clear_first: bool,
    #[serde(default)]
    pub submit: bool,
    #[serde(default)]
    pub paste: bool,
    #[serde(default)]
    pub allow_keyboard_fallback: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AxListResult {
    pub nodes: Vec<AxNode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AxClickResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    pub matched_count: usize,
    pub action: String,
    #[serde(default)]
    pub used_coordinate_fallback: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_x: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_y: Option<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_order: Vec<AxClickFallbackStage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempted_stages: Vec<AxClickFallbackStage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector_explain: Option<AxSelectorExplain>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gates: Option<AxGateResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub postconditions: Option<AxPostconditionResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AxTypeResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    pub matched_count: usize,
    pub applied_via: String,
    pub text_length: usize,
    #[serde(default)]
    pub submitted: bool,
    #[serde(default)]
    pub used_keyboard_fallback: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector_explain: Option<AxSelectorExplain>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gates: Option<AxGateResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub postconditions: Option<AxPostconditionResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AxGateCheckResult {
    pub gate: String,
    pub terminal_status: String,
    pub attempts: u32,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AxGateResult {
    pub timeout_ms: u64,
    pub poll_ms: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<AxGateCheckResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AxPostconditionCheckResult {
    pub check: String,
    pub terminal_status: String,
    pub attempts: u32,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribute: Option<String>,
    pub expected: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AxPostconditionResult {
    pub timeout_ms: u64,
    pub poll_ms: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<AxPostconditionCheckResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DebugBundleArtifactEntry {
    pub id: String,
    pub path: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DebugBundleResult {
    pub output_dir: String,
    pub artifact_index_path: String,
    pub partial_failure: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<DebugBundleArtifactEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AxAttrGetRequest {
    #[serde(default)]
    pub target: AxTarget,
    #[serde(default)]
    pub selector: AxSelector,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AxAttrSetRequest {
    #[serde(default)]
    pub target: AxTarget,
    #[serde(default)]
    pub selector: AxSelector,
    pub name: String,
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AxActionPerformRequest {
    #[serde(default)]
    pub target: AxTarget,
    #[serde(default)]
    pub selector: AxSelector,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AxAttrGetResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    pub matched_count: usize,
    pub name: String,
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AxAttrSetResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    pub matched_count: usize,
    pub name: String,
    pub applied: bool,
    pub value_type: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AxAttrSetCommandResult {
    #[serde(flatten)]
    pub detail: AxAttrSetResult,
    pub policy: ActionPolicyResult,
    pub meta: ActionMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AxActionPerformResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    pub matched_count: usize,
    pub name: String,
    pub performed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AxActionPerformCommandResult {
    #[serde(flatten)]
    pub detail: AxActionPerformResult,
    pub policy: ActionPolicyResult,
    pub meta: ActionMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AxSessionStartRequest {
    #[serde(default)]
    pub target: AxTarget,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AxSessionStopRequest {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AxSessionInfo {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_title_contains: Option<String>,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AxSessionStartResult {
    #[serde(flatten)]
    pub session: AxSessionInfo,
    pub created: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AxSessionStartCommandResult {
    #[serde(flatten)]
    pub detail: AxSessionStartResult,
    pub policy: ActionPolicyResult,
    pub meta: ActionMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AxSessionListResult {
    pub sessions: Vec<AxSessionInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AxSessionStopResult {
    pub session_id: String,
    pub removed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AxSessionStopCommandResult {
    #[serde(flatten)]
    pub detail: AxSessionStopResult,
    pub policy: ActionPolicyResult,
    pub meta: ActionMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AxWatchStartRequest {
    pub session_id: String,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub max_buffer: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watch_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AxWatchPollRequest {
    pub watch_id: String,
    #[serde(default)]
    pub limit: usize,
    #[serde(default)]
    pub drain: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AxWatchStopRequest {
    pub watch_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AxWatchEvent {
    pub watch_id: String,
    pub event: String,
    pub at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AxWatchStartResult {
    pub watch_id: String,
    pub session_id: String,
    pub events: Vec<String>,
    pub max_buffer: usize,
    pub started: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AxWatchStartCommandResult {
    #[serde(flatten)]
    pub detail: AxWatchStartResult,
    pub policy: ActionPolicyResult,
    pub meta: ActionMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AxWatchPollResult {
    pub watch_id: String,
    pub events: Vec<AxWatchEvent>,
    pub dropped: usize,
    pub running: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AxWatchStopResult {
    pub watch_id: String,
    pub stopped: bool,
    pub drained: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct AxWatchStopCommandResult {
    #[serde(flatten)]
    pub detail: AxWatchStopResult,
    pub policy: ActionPolicyResult,
    pub meta: ActionMeta,
}

#[derive(Debug, Clone, Serialize)]
pub struct AxClickCommandResult {
    #[serde(flatten)]
    pub detail: AxClickResult,
    pub policy: ActionPolicyResult,
    pub meta: ActionMeta,
}

#[derive(Debug, Clone, Serialize)]
pub struct AxTypeCommandResult {
    #[serde(flatten)]
    pub detail: AxTypeResult,
    pub policy: ActionPolicyResult,
    pub meta: ActionMeta,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScenarioStepResult {
    pub step_id: String,
    pub ok: bool,
    pub exit_code: i32,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ax_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_used: Option<bool>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stderr: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScenarioRunResult {
    pub file: String,
    pub total_steps: usize,
    pub passed_steps: usize,
    pub failed_steps: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_failed_step_id: Option<String>,
    pub steps: Vec<ScenarioStepResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfileValidateResult {
    pub file: String,
    pub valid: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfileInitResult {
    pub path: String,
    pub profile_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct InputSourceCurrentResult {
    pub current: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct InputSourceSwitchResult {
    pub previous: String,
    pub current: String,
    pub switched: bool,
}

fn normalize_tsv_field(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch == '\t' || ch == '\n' || ch == '\r' {
                ' '
            } else {
                ch
            }
        })
        .collect()
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_match_strategy_contains(value: &AxMatchStrategy) -> bool {
    *value == AxMatchStrategy::Contains
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::normalize_tsv_field;

    #[test]
    fn normalize_tsv_field_replaces_control_whitespace() {
        assert_eq!(normalize_tsv_field("A\tB\nC\rD"), "A B C D");
    }
}
