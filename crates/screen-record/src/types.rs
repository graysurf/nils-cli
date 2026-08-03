#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayInfo {
    pub id: u32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub id: u32,
    pub owner_name: String,
    pub title: String,
    pub bounds: Rect,
    pub on_screen: bool,
    pub active: bool,
    pub owner_pid: i32,
    pub z_order: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfo {
    pub name: String,
    pub pid: i32,
    pub bundle_id: String,
}

#[derive(Debug, Clone, Default)]
pub struct ShareableContent {
    pub displays: Vec<DisplayInfo>,
    pub windows: Vec<WindowInfo>,
    pub apps: Vec<AppInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PermissionState {
    Ready,
    Blocked,
    #[default]
    Unknown,
}

impl PermissionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Blocked => "blocked",
            Self::Unknown => "unknown",
        }
    }

    pub fn is_blocked(self) -> bool {
        matches!(self, Self::Blocked)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionStatusSchema {
    pub screen_recording: PermissionState,
    pub accessibility: PermissionState,
    pub automation: PermissionState,
    pub ready: bool,
    pub hints: Vec<String>,
}

impl PermissionStatusSchema {
    pub fn from_components(
        screen_recording: PermissionState,
        accessibility: PermissionState,
        automation: PermissionState,
        hints: Vec<String>,
    ) -> Self {
        Self {
            screen_recording,
            accessibility,
            automation,
            ready: Self::compute_ready(screen_recording, accessibility, automation),
            hints: stable_unique_hints(hints),
        }
    }

    pub fn compute_ready(
        screen_recording: PermissionState,
        accessibility: PermissionState,
        automation: PermissionState,
    ) -> bool {
        let any_ready = matches!(screen_recording, PermissionState::Ready)
            || matches!(accessibility, PermissionState::Ready)
            || matches!(automation, PermissionState::Ready);

        any_ready
            && !screen_recording.is_blocked()
            && !accessibility.is_blocked()
            && !automation.is_blocked()
    }
}

impl Default for PermissionStatusSchema {
    fn default() -> Self {
        Self::from_components(
            PermissionState::Unknown,
            PermissionState::Unknown,
            PermissionState::Unknown,
            Vec::new(),
        )
    }
}

fn stable_unique_hints(hints: Vec<String>) -> Vec<String> {
    let mut unique = Vec::with_capacity(hints.len());
    for hint in hints {
        if !unique.iter().any(|existing| existing == &hint) {
            unique.push(hint);
        }
    }
    unique
}

pub const RECORDING_DIAGNOSTICS_SCHEMA_VERSION: u32 = 1;
pub const RECORDING_DIAGNOSTICS_CONTRACT_VERSION: &str = "1.0";
pub const RECORDING_DIAGNOSTICS_ARTIFACT_DIR_SUFFIX: &str = "diagnostics";
pub const CONTACT_SHEET_ARTIFACT_SUFFIX: &str = "contact-sheet.svg";
pub const MOTION_INTERVALS_ARTIFACT_SUFFIX: &str = "motion-intervals.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingDiagnosticsArtifacts {
    pub contact_sheet_path: PathBuf,
    pub motion_intervals_path: PathBuf,
    pub interval_count: usize,
}
use std::path::PathBuf;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_state_labels_are_stable_and_lowercase() {
        assert_eq!(PermissionState::Ready.as_str(), "ready");
        assert_eq!(PermissionState::Blocked.as_str(), "blocked");
        assert_eq!(PermissionState::Unknown.as_str(), "unknown");
        assert_eq!(PermissionState::default(), PermissionState::Unknown);

        assert!(PermissionState::Blocked.is_blocked());
        assert!(!PermissionState::Ready.is_blocked());
        assert!(!PermissionState::Unknown.is_blocked());
    }

    #[test]
    fn readiness_needs_one_granted_permission_and_no_denial() {
        use PermissionState::{Blocked, Ready, Unknown};

        // Nothing observed yet is not readiness.
        assert!(!PermissionStatusSchema::compute_ready(
            Unknown, Unknown, Unknown
        ));
        // One confirmed grant with no denial is enough to proceed.
        assert!(PermissionStatusSchema::compute_ready(
            Ready, Unknown, Unknown
        ));
        assert!(PermissionStatusSchema::compute_ready(
            Unknown, Ready, Unknown
        ));
        assert!(PermissionStatusSchema::compute_ready(
            Unknown, Unknown, Ready
        ));
        assert!(PermissionStatusSchema::compute_ready(Ready, Ready, Ready));
        // A single denial disqualifies the whole probe, whatever else is granted.
        assert!(!PermissionStatusSchema::compute_ready(
            Ready, Blocked, Unknown
        ));
        assert!(!PermissionStatusSchema::compute_ready(
            Blocked, Ready, Ready
        ));
        assert!(!PermissionStatusSchema::compute_ready(
            Ready, Ready, Blocked
        ));
    }

    #[test]
    fn status_schema_derives_readiness_and_de_duplicates_hints() {
        let status = PermissionStatusSchema::from_components(
            PermissionState::Ready,
            PermissionState::Unknown,
            PermissionState::Unknown,
            vec![
                "grant screen recording".to_string(),
                "grant screen recording".to_string(),
                "grant accessibility".to_string(),
            ],
        );

        assert!(status.ready);
        assert_eq!(status.screen_recording, PermissionState::Ready);
        // Hints keep first-seen order so the operator reads them in the order
        // the probes produced them, with repeats collapsed.
        assert_eq!(
            status.hints,
            vec![
                "grant screen recording".to_string(),
                "grant accessibility".to_string()
            ]
        );

        let default = PermissionStatusSchema::default();
        assert!(!default.ready);
        assert!(default.hints.is_empty());
        assert_eq!(default.automation, PermissionState::Unknown);
    }

    #[test]
    fn geometry_and_inventory_types_have_usable_defaults() {
        let rect = Rect::default();
        assert_eq!((rect.x, rect.y, rect.width, rect.height), (0, 0, 0, 0));
        assert_eq!(
            Rect {
                x: 1,
                y: 2,
                width: 3,
                height: 4
            },
            Rect {
                x: 1,
                y: 2,
                width: 3,
                height: 4
            }
        );

        let content = ShareableContent::default();
        assert!(content.displays.is_empty());
        assert!(content.windows.is_empty());
        assert!(content.apps.is_empty());

        let window = WindowInfo {
            id: 7,
            owner_name: "Terminal".to_string(),
            title: "zsh".to_string(),
            bounds: rect,
            on_screen: true,
            active: false,
            owner_pid: 42,
            z_order: 0,
        };
        assert_eq!(window.clone(), window);

        let populated = ShareableContent {
            displays: vec![DisplayInfo {
                id: 1,
                width: 1920,
                height: 1080,
            }],
            windows: vec![window],
            apps: vec![AppInfo {
                name: "Terminal".to_string(),
                pid: 42,
                bundle_id: "com.apple.Terminal".to_string(),
            }],
        };
        assert_eq!(populated.displays[0].width, 1920);
        assert_eq!(populated.apps[0].bundle_id, "com.apple.Terminal");
    }

    #[test]
    fn diagnostics_artifact_names_are_pinned_to_the_published_contract() {
        assert_eq!(RECORDING_DIAGNOSTICS_SCHEMA_VERSION, 1);
        assert_eq!(RECORDING_DIAGNOSTICS_CONTRACT_VERSION, "1.0");
        assert_eq!(RECORDING_DIAGNOSTICS_ARTIFACT_DIR_SUFFIX, "diagnostics");
        assert_eq!(CONTACT_SHEET_ARTIFACT_SUFFIX, "contact-sheet.svg");
        assert_eq!(MOTION_INTERVALS_ARTIFACT_SUFFIX, "motion-intervals.json");

        let artifacts = RecordingDiagnosticsArtifacts {
            contact_sheet_path: PathBuf::from("/out/run-contact-sheet.svg"),
            motion_intervals_path: PathBuf::from("/out/run-motion-intervals.json"),
            interval_count: 3,
        };
        assert_eq!(artifacts.clone(), artifacts);
        assert_eq!(artifacts.interval_count, 3);
    }
}
