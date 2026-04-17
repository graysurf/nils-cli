use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::{Local, SecondsFormat, Utc};
use nils_common::env as shared_env;

use crate::cli::{AudioMode, Cli, ContainerFormat, ImageFormat};
use crate::error::CliError;
use crate::select::{SelectionArgs, format_window_tsv, select_window};
use crate::test_mode;
use crate::types::{
    AppInfo, CONTACT_SHEET_ARTIFACT_SUFFIX, DisplayInfo, MOTION_INTERVALS_ARTIFACT_SUFFIX,
    RECORDING_DIAGNOSTICS_ARTIFACT_DIR_SUFFIX, RECORDING_DIAGNOSTICS_CONTRACT_VERSION,
    RECORDING_DIAGNOSTICS_SCHEMA_VERSION, RecordingDiagnosticsArtifacts, ShareableContent,
    WindowInfo,
};

pub fn run(cli: Cli) -> Result<(), CliError> {
    let test_mode_enabled = test_mode::enabled();
    let mode = determine_mode(&cli)?;
    validate_screenshot_flag_usage(&cli, mode)?;
    validate_portal_flag_usage(&cli, mode)?;
    validate_metadata_flag_usage(&cli, mode)?;
    validate_diagnostics_flag_usage(&cli, mode)?;
    validate_if_changed_flag_usage(&cli, mode)?;
    let backend = Backend::detect(test_mode_enabled)?;

    match mode {
        Mode::Preflight => {
            ensure_no_recording_flags(&cli)?;
            backend.preflight()?;
            return Ok(());
        }
        Mode::RequestPermission => {
            ensure_no_recording_flags(&cli)?;
            backend.request_permission()?;
            return Ok(());
        }
        Mode::ListWindows => {
            ensure_no_recording_flags(&cli)?;
            if matches!(backend, Backend::Linux) {
                ensure_linux_x11_only_mode_allowed()?;
            }
            let content = fetch_shareable_content(&backend)?;
            let mut windows = content.windows;
            windows.sort_by(|a, b| {
                a.owner_name
                    .cmp(&b.owner_name)
                    .then_with(|| a.title.cmp(&b.title))
                    .then_with(|| a.id.cmp(&b.id))
            });
            for window in windows {
                println!("{}", format_window_tsv(&window));
            }
        }
        Mode::ListDisplays => {
            ensure_no_recording_flags(&cli)?;
            if matches!(backend, Backend::Linux) {
                ensure_linux_x11_only_mode_allowed()?;
            }
            let content = fetch_shareable_content(&backend)?;
            let mut displays = content.displays;
            displays.sort_by_key(|display| display.id);
            for display in displays {
                println!("{}", format_display_tsv(&display));
            }
        }
        Mode::ListApps => {
            ensure_no_recording_flags(&cli)?;
            if matches!(backend, Backend::Linux) {
                ensure_linux_x11_only_mode_allowed()?;
            }
            let content = fetch_shareable_content(&backend)?;
            let mut apps = content.apps;
            normalize_app_list(&mut apps);
            for app in apps {
                println!("{}", format_app_tsv(&app));
            }
        }
        Mode::Screenshot => {
            validate_screenshot_args(&cli)?;
            if cli.portal {
                let (output_path, format) =
                    resolve_portal_screenshot_output(&cli, test_mode_enabled)?;
                backend.screenshot_portal(&output_path, format)?;
                println!("{}", output_path.display());
                return Ok(());
            }

            if matches!(backend, Backend::Linux) {
                ensure_linux_x11_selectors_allowed()?;
            }

            let content = fetch_shareable_content(&backend)?;
            let args = SelectionArgs {
                window_id: cli.window_id,
                app: cli.app.clone(),
                window_name: cli.window_name.clone(),
                active_window: cli.active_window,
            };
            let selected = select_window(&content.windows, &args)?;

            let (output_path, format) =
                resolve_screenshot_output(&cli, &selected, test_mode_enabled)?;
            if cli.if_changed {
                capture_if_changed_screenshot(&cli, &backend, &selected, format, &output_path)?;
            } else {
                backend.screenshot_window(&selected, &output_path, format)?;
            }
            println!("{}", output_path.display());
            return Ok(());
        }
        Mode::Record => {
            return run_record_mode(&cli, &backend, test_mode_enabled);
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    ListWindows,
    ListDisplays,
    ListApps,
    Preflight,
    RequestPermission,
    Screenshot,
    Record,
}

#[derive(Debug, Clone, Copy)]
enum Backend {
    TestMode,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Macos,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Linux,
}

impl Backend {
    fn detect(test_mode_enabled: bool) -> Result<Self, CliError> {
        if test_mode_enabled {
            return Ok(Backend::TestMode);
        }
        #[cfg(target_os = "macos")]
        {
            Ok(Backend::Macos)
        }
        #[cfg(target_os = "linux")]
        {
            Ok(Backend::Linux)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(CliError::unsupported_platform())
        }
    }

    fn preflight(&self) -> Result<(), CliError> {
        match self {
            Backend::TestMode => Ok(()),
            Backend::Macos => {
                #[cfg(target_os = "macos")]
                {
                    crate::macos::permissions::preflight()
                }
                #[cfg(not(target_os = "macos"))]
                {
                    Err(CliError::unsupported_platform())
                }
            }
            Backend::Linux => {
                #[cfg(target_os = "linux")]
                {
                    crate::linux::preflight::preflight()
                }
                #[cfg(not(target_os = "linux"))]
                {
                    Err(CliError::unsupported_platform())
                }
            }
        }
    }

    fn request_permission(&self) -> Result<(), CliError> {
        match self {
            Backend::TestMode => Ok(()),
            Backend::Macos => {
                #[cfg(target_os = "macos")]
                {
                    crate::macos::permissions::request_permission()
                }
                #[cfg(not(target_os = "macos"))]
                {
                    Err(CliError::unsupported_platform())
                }
            }
            Backend::Linux => {
                #[cfg(target_os = "linux")]
                {
                    crate::linux::preflight::request_permission()
                }
                #[cfg(not(target_os = "linux"))]
                {
                    Err(CliError::unsupported_platform())
                }
            }
        }
    }

    fn shareable_content(&self) -> Result<ShareableContent, CliError> {
        match self {
            Backend::TestMode => Ok(test_mode::shareable_content()),
            Backend::Macos => {
                #[cfg(target_os = "macos")]
                {
                    crate::macos::shareable::fetch_shareable()
                }
                #[cfg(not(target_os = "macos"))]
                {
                    Err(CliError::unsupported_platform())
                }
            }
            Backend::Linux => {
                #[cfg(target_os = "linux")]
                {
                    crate::linux::shareable_content()
                }
                #[cfg(not(target_os = "linux"))]
                {
                    Err(CliError::unsupported_platform())
                }
            }
        }
    }

    fn screenshot_window(
        &self,
        window: &WindowInfo,
        path: &Path,
        format: ImageFormat,
    ) -> Result<(), CliError> {
        match self {
            Backend::TestMode => test_mode::screenshot_fixture(path, format),
            Backend::Macos => {
                #[cfg(target_os = "macos")]
                {
                    crate::macos::screenshot::screenshot_window(window, path, format)
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let _ = (window, path, format);
                    Err(CliError::unsupported_platform())
                }
            }
            Backend::Linux => {
                #[cfg(target_os = "linux")]
                {
                    crate::linux::screenshot_window(window, path, format)
                }
                #[cfg(not(target_os = "linux"))]
                {
                    let _ = (window, path, format);
                    Err(CliError::unsupported_platform())
                }
            }
        }
    }

    fn screenshot_portal(&self, path: &Path, format: ImageFormat) -> Result<(), CliError> {
        match self {
            Backend::TestMode => test_mode::screenshot_fixture(path, format),
            Backend::Macos => Err(CliError::usage(
                "--portal is only supported on Linux (Wayland)",
            )),
            Backend::Linux => {
                #[cfg(target_os = "linux")]
                {
                    crate::linux::screenshot_portal(path, format)
                }
                #[cfg(not(target_os = "linux"))]
                {
                    let _ = (path, format);
                    Err(CliError::unsupported_platform())
                }
            }
        }
    }

    fn record_window(
        &self,
        window: &WindowInfo,
        duration: u64,
        audio: AudioMode,
        path: &Path,
        format: ContainerFormat,
    ) -> Result<(), CliError> {
        match self {
            Backend::TestMode => test_mode::record_fixture_for_duration(duration, path, format),
            Backend::Macos => {
                #[cfg(target_os = "macos")]
                {
                    crate::macos::stream::record_window(window, duration, audio, path, format)
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let _ = (window, duration, audio, path, format);
                    Err(CliError::unsupported_platform())
                }
            }
            Backend::Linux => {
                #[cfg(target_os = "linux")]
                {
                    crate::linux::record_window(window, duration, audio, path, format)
                }
                #[cfg(not(target_os = "linux"))]
                {
                    let _ = (window, duration, audio, path, format);
                    Err(CliError::unsupported_platform())
                }
            }
        }
    }

    fn record_display(
        &self,
        display_id: u32,
        duration: u64,
        audio: AudioMode,
        path: &Path,
        format: ContainerFormat,
    ) -> Result<(), CliError> {
        match self {
            Backend::TestMode => test_mode::record_fixture_for_duration(duration, path, format),
            Backend::Macos => {
                #[cfg(target_os = "macos")]
                {
                    crate::macos::stream::record_display(display_id, duration, audio, path, format)
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let _ = (display_id, duration, audio, path, format);
                    Err(CliError::unsupported_platform())
                }
            }
            Backend::Linux => {
                #[cfg(target_os = "linux")]
                {
                    crate::linux::record_display(display_id, duration, audio, path, format)
                }
                #[cfg(not(target_os = "linux"))]
                {
                    let _ = (display_id, duration, audio, path, format);
                    Err(CliError::unsupported_platform())
                }
            }
        }
    }

    fn record_main_display(
        &self,
        duration: u64,
        audio: AudioMode,
        path: &Path,
        format: ContainerFormat,
    ) -> Result<(), CliError> {
        match self {
            Backend::TestMode => test_mode::record_fixture_for_duration(duration, path, format),
            Backend::Macos => {
                #[cfg(target_os = "macos")]
                {
                    crate::macos::stream::record_main_display(duration, audio, path, format)
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let _ = (duration, audio, path, format);
                    Err(CliError::unsupported_platform())
                }
            }
            Backend::Linux => {
                #[cfg(target_os = "linux")]
                {
                    crate::linux::record_main_display(duration, audio, path, format)
                }
                #[cfg(not(target_os = "linux"))]
                {
                    let _ = (duration, audio, path, format);
                    Err(CliError::unsupported_platform())
                }
            }
        }
    }

    fn record_portal(
        &self,
        duration: u64,
        path: &Path,
        format: ContainerFormat,
    ) -> Result<(), CliError> {
        match self {
            Backend::TestMode => test_mode::record_fixture_for_duration(duration, path, format),
            Backend::Macos => Err(CliError::usage(
                "--portal is only supported on Linux (Wayland)",
            )),
            Backend::Linux => {
                #[cfg(target_os = "linux")]
                {
                    crate::linux::record_portal(duration, path, format)
                }
                #[cfg(not(target_os = "linux"))]
                {
                    let _ = (duration, path, format);
                    Err(CliError::unsupported_platform())
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
struct RecordingMetadata {
    target: String,
    duration_ms: u64,
    audio_mode: String,
    format: Option<String>,
    output_path: Option<String>,
    output_bytes: Option<u64>,
    started_at: String,
    ended_at: String,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct RecordingDiagnosticsMetadata {
    schema_version: u32,
    contract_version: &'static str,
    source_output_path: String,
    source_output_bytes: u64,
    generated_at: String,
    contact_sheet_path: String,
    motion_intervals_path: String,
    motion_interval_count: usize,
    error: Option<String>,
}

fn run_record_mode(cli: &Cli, backend: &Backend, test_mode_enabled: bool) -> Result<(), CliError> {
    let metadata_out = resolve_metadata_out_path(cli.metadata_out.as_deref())?;
    let diagnostics_out = resolve_diagnostics_out_path(cli.diagnostics_out.as_deref())?;
    let started_at = metadata_timestamp(test_mode_enabled);
    let started = Instant::now();
    let target = recording_target(cli);
    let audio_mode = audio_mode_label(cli.audio).to_string();
    let mut resolved_format: Option<ContainerFormat> = None;
    let mut resolved_output_path: Option<PathBuf> = None;

    let record_result = (|| {
        validate_record_args(cli)?;

        let output_path = resolve_output_path(cli)?;
        let staged_output_path = staged_recording_output_path(&output_path)?;
        let container = resolve_container(&output_path, cli.format)?;
        if cli.audio == AudioMode::Both && container == ContainerFormat::Mp4 {
            return Err(CliError::usage("--audio both requires .mov"));
        }
        let duration = cli.duration.expect("duration validated");

        resolved_output_path = Some(output_path.clone());
        resolved_format = Some(container);

        let record_result = if cli.portal {
            backend.record_portal(duration, &staged_output_path, container)
        } else if matches!(backend, Backend::Linux) {
            ensure_linux_x11_selectors_allowed()?;
            if cli.display {
                backend.record_main_display(duration, cli.audio, &staged_output_path, container)
            } else if let Some(display_id) = cli.display_id {
                backend.record_display(
                    display_id,
                    duration,
                    cli.audio,
                    &staged_output_path,
                    container,
                )
            } else {
                let content = fetch_shareable_content(backend)?;
                let args = SelectionArgs {
                    window_id: cli.window_id,
                    app: cli.app.clone(),
                    window_name: cli.window_name.clone(),
                    active_window: cli.active_window,
                };
                let selected = select_window(&content.windows, &args)?;
                backend.record_window(
                    &selected,
                    duration,
                    cli.audio,
                    &staged_output_path,
                    container,
                )
            }
        } else if cli.display {
            backend.record_main_display(duration, cli.audio, &staged_output_path, container)
        } else if let Some(display_id) = cli.display_id {
            backend.record_display(
                display_id,
                duration,
                cli.audio,
                &staged_output_path,
                container,
            )
        } else {
            let content = fetch_shareable_content(backend)?;
            let args = SelectionArgs {
                window_id: cli.window_id,
                app: cli.app.clone(),
                window_name: cli.window_name.clone(),
                active_window: cli.active_window,
            };
            let selected = select_window(&content.windows, &args)?;
            backend.record_window(
                &selected,
                duration,
                cli.audio,
                &staged_output_path,
                container,
            )
        };

        if let Err(err) = record_result {
            cleanup_recording_temp_file(&staged_output_path);
            return Err(err);
        }

        if let Err(err) = publish_recording_output(&staged_output_path, &output_path) {
            cleanup_recording_temp_file(&staged_output_path);
            return Err(err);
        }

        println!("{}", output_path.display());
        Ok(())
    })();

    let duration_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let ended_at = metadata_timestamp(test_mode_enabled);
    let mut final_result = record_result;

    let diagnostics_error = if final_result.is_ok() {
        match (diagnostics_out.as_deref(), resolved_output_path.as_deref()) {
            (Some(diagnostics_path), Some(output_path)) => write_recording_diagnostics(
                diagnostics_path,
                output_path,
                duration_ms,
                &ended_at,
                test_mode_enabled,
            )
            .err(),
            _ => None,
        }
    } else {
        None
    };
    if let Some(err) = diagnostics_error {
        final_result = Err(err);
    }

    if let Some(metadata_path) = metadata_out.as_deref() {
        let output_bytes = resolved_output_path
            .as_deref()
            .and_then(|path| std::fs::metadata(path).ok())
            .map(|meta| meta.len());
        let metadata = RecordingMetadata {
            target,
            duration_ms,
            audio_mode,
            format: resolved_format.map(|format| format_label(format).to_string()),
            output_path: resolved_output_path
                .as_ref()
                .map(|path| path.display().to_string()),
            output_bytes,
            started_at,
            ended_at,
            error: final_result.as_ref().err().map(ToString::to_string),
        };

        match write_recording_metadata(metadata_path, &metadata) {
            Err(err) if final_result.is_ok() => return Err(err),
            _ => {}
        }
    }

    final_result
}

fn recording_target(cli: &Cli) -> String {
    if cli.portal {
        return "portal".to_string();
    }
    if cli.display {
        return "display".to_string();
    }
    if let Some(display_id) = cli.display_id {
        return format!("display-id:{display_id}");
    }
    if let Some(window_id) = cli.window_id {
        return format!("window-id:{window_id}");
    }
    if cli.active_window {
        return "active-window".to_string();
    }
    if let Some(app) = cli.app.as_ref() {
        if let Some(window_name) = cli.window_name.as_ref() {
            return format!("app:{app};window-name:{window_name}");
        }
        return format!("app:{app}");
    }
    "unknown".to_string()
}

fn audio_mode_label(mode: AudioMode) -> &'static str {
    match mode {
        AudioMode::Off => "off",
        AudioMode::System => "system",
        AudioMode::Mic => "mic",
        AudioMode::Both => "both",
    }
}

fn metadata_timestamp(test_mode_enabled: bool) -> String {
    if test_mode_enabled {
        return "2026-01-01T00:00:00.000Z".to_string();
    }
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn resolve_metadata_out_path(path: Option<&Path>) -> Result<Option<PathBuf>, CliError> {
    let Some(path) = path else {
        return Ok(None);
    };

    let mut resolved = path.to_path_buf();
    if !resolved.is_absolute() {
        let cwd = std::env::current_dir()
            .map_err(|err| CliError::runtime(format!("failed to resolve cwd: {err}")))?;
        resolved = cwd.join(resolved);
    }

    if let Some(parent) = resolved.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| CliError::runtime(format!("failed to create output dir: {err}")))?;
    }

    Ok(Some(resolved))
}

fn resolve_diagnostics_out_path(path: Option<&Path>) -> Result<Option<PathBuf>, CliError> {
    let Some(path) = path else {
        return Ok(None);
    };

    let mut resolved = path.to_path_buf();
    if !resolved.is_absolute() {
        let cwd = std::env::current_dir()
            .map_err(|err| CliError::runtime(format!("failed to resolve cwd: {err}")))?;
        resolved = cwd.join(resolved);
    }

    if let Some(parent) = resolved.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| CliError::runtime(format!("failed to create output dir: {err}")))?;
    }

    Ok(Some(resolved))
}

fn write_recording_diagnostics(
    diagnostics_path: &Path,
    source_output_path: &Path,
    duration_ms: u64,
    generated_at: &str,
    test_mode_enabled: bool,
) -> Result<(), CliError> {
    if test_mode_enabled && diagnostics_failure_forced() {
        return Err(CliError::runtime(
            "diagnostics generation failed in test mode (forced)",
        ));
    }

    let source_output_bytes = std::fs::metadata(source_output_path)
        .map_err(|err| {
            CliError::runtime(format!(
                "failed to read recording output metadata: {} ({err})",
                source_output_path.display()
            ))
        })?
        .len();

    let artifacts =
        prepare_diagnostics_artifacts(diagnostics_path, source_output_path, duration_ms)?;
    let metadata = RecordingDiagnosticsMetadata {
        schema_version: RECORDING_DIAGNOSTICS_SCHEMA_VERSION,
        contract_version: RECORDING_DIAGNOSTICS_CONTRACT_VERSION,
        source_output_path: source_output_path.display().to_string(),
        source_output_bytes,
        generated_at: generated_at.to_string(),
        contact_sheet_path: artifacts.contact_sheet_path.display().to_string(),
        motion_intervals_path: artifacts.motion_intervals_path.display().to_string(),
        motion_interval_count: artifacts.interval_count,
        error: None,
    };

    write_recording_diagnostics_manifest(diagnostics_path, &metadata)
}

fn prepare_diagnostics_artifacts(
    diagnostics_path: &Path,
    source_output_path: &Path,
    duration_ms: u64,
) -> Result<RecordingDiagnosticsArtifacts, CliError> {
    let output_stem = source_output_path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("recording");
    let artifacts_dir = diagnostics_path.with_extension(RECORDING_DIAGNOSTICS_ARTIFACT_DIR_SUFFIX);
    std::fs::create_dir_all(&artifacts_dir)
        .map_err(|err| CliError::runtime(format!("failed to create diagnostics dir: {err}")))?;

    let contact_sheet_path =
        artifacts_dir.join(format!("{output_stem}-{CONTACT_SHEET_ARTIFACT_SUFFIX}"));
    let motion_intervals_path =
        artifacts_dir.join(format!("{output_stem}-{MOTION_INTERVALS_ARTIFACT_SUFFIX}"));

    write_contact_sheet_artifact(&contact_sheet_path, source_output_path, duration_ms)?;
    let interval_count =
        write_motion_intervals_artifact(&motion_intervals_path, source_output_path, duration_ms)?;

    Ok(RecordingDiagnosticsArtifacts {
        contact_sheet_path,
        motion_intervals_path,
        interval_count,
    })
}

fn write_recording_diagnostics_manifest(
    path: &Path,
    diagnostics: &RecordingDiagnosticsMetadata,
) -> Result<(), CliError> {
    let text = format!(
        "{{\n  \"schema_version\": {},\n  \"contract_version\": \"{}\",\n  \"source_output_path\": \"{}\",\n  \"source_output_bytes\": {},\n  \"generated_at\": \"{}\",\n  \"artifacts\": {{\n    \"contact_sheet\": {{ \"path\": \"{}\", \"format\": \"svg\" }},\n    \"motion_intervals\": {{ \"path\": \"{}\", \"format\": \"json\", \"interval_count\": {} }}\n  }},\n  \"error\": {}\n}}\n",
        diagnostics.schema_version,
        escape_json(diagnostics.contract_version),
        escape_json(&diagnostics.source_output_path),
        diagnostics.source_output_bytes,
        escape_json(&diagnostics.generated_at),
        escape_json(&diagnostics.contact_sheet_path),
        escape_json(&diagnostics.motion_intervals_path),
        diagnostics.motion_interval_count,
        format_optional_string(diagnostics.error.as_deref()),
    );
    std::fs::write(path, text)
        .map_err(|err| CliError::runtime(format!("failed to write diagnostics: {err}")))
}

fn write_contact_sheet_artifact(
    path: &Path,
    source_output_path: &Path,
    duration_ms: u64,
) -> Result<(), CliError> {
    #[cfg(all(target_os = "macos", not(coverage)))]
    {
        crate::macos::writer::write_diagnostics_contact_sheet(path, source_output_path, duration_ms)
    }

    #[cfg(not(all(target_os = "macos", not(coverage))))]
    {
        let source_name = source_output_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("recording");
        let escaped_name = escape_svg(source_name);
        let view_duration = duration_ms.max(1);
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"960\" height=\"540\" viewBox=\"0 0 960 540\">\n  <rect x=\"0\" y=\"0\" width=\"960\" height=\"540\" fill=\"#0d1b2a\" />\n  <text x=\"48\" y=\"64\" fill=\"#e0e1dd\" font-size=\"28\" font-family=\"Menlo, monospace\">screen-record diagnostics</text>\n  <text x=\"48\" y=\"112\" fill=\"#e0e1dd\" font-size=\"18\" font-family=\"Menlo, monospace\">source={escaped_name} duration_ms={view_duration}</text>\n</svg>\n"
        );
        std::fs::write(path, svg)
            .map_err(|err| CliError::runtime(format!("failed to write contact sheet: {err}")))
    }
}

fn write_motion_intervals_artifact(
    path: &Path,
    source_output_path: &Path,
    duration_ms: u64,
) -> Result<usize, CliError> {
    #[cfg(all(target_os = "macos", not(coverage)))]
    {
        crate::macos::writer::write_diagnostics_motion_intervals(
            path,
            source_output_path,
            duration_ms,
        )
    }

    #[cfg(not(all(target_os = "macos", not(coverage))))]
    {
        let source_path = source_output_path.display().to_string();
        let escaped_source_path = escape_json(&source_path);
        let span = duration_ms.max(1);
        let text = format!(
            "{{\n  \"schema_version\": 1,\n  \"contract_version\": \"1.0\",\n  \"source_output_path\": \"{}\",\n  \"intervals\": [\n    {{ \"start_ms\": 0, \"end_ms\": {}, \"score\": 100 }}\n  ]\n}}\n",
            escaped_source_path, span
        );
        std::fs::write(path, text)
            .map_err(|err| CliError::runtime(format!("failed to write motion intervals: {err}")))?;
        Ok(1)
    }
}

fn diagnostics_failure_forced() -> bool {
    shared_env::env_truthy("AGENTS_SCREEN_RECORD_TEST_MODE_FAIL_DIAGNOSTICS")
}

fn write_recording_metadata(path: &Path, metadata: &RecordingMetadata) -> Result<(), CliError> {
    let text = format!(
        "{{\n  \"target\": \"{}\",\n  \"duration_ms\": {},\n  \"audio_mode\": \"{}\",\n  \"format\": {},\n  \"output_path\": {},\n  \"output_bytes\": {},\n  \"started_at\": \"{}\",\n  \"ended_at\": \"{}\",\n  \"error\": {}\n}}\n",
        escape_json(&metadata.target),
        metadata.duration_ms,
        escape_json(&metadata.audio_mode),
        format_optional_string(metadata.format.as_deref()),
        format_optional_string(metadata.output_path.as_deref()),
        format_optional_u64(metadata.output_bytes),
        escape_json(&metadata.started_at),
        escape_json(&metadata.ended_at),
        format_optional_string(metadata.error.as_deref()),
    );

    std::fs::write(path, text)
        .map_err(|err| CliError::runtime(format!("failed to write metadata: {err}")))
}

fn format_optional_string(value: Option<&str>) -> String {
    match value {
        Some(value) => format!("\"{}\"", escape_json(value)),
        None => "null".to_string(),
    }
}

fn format_optional_u64(value: Option<u64>) -> String {
    match value {
        Some(value) => value.to_string(),
        None => "null".to_string(),
    }
}

fn escape_json(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{08}' => escaped.push_str("\\b"),
            '\u{0C}' => escaped.push_str("\\f"),
            ch if ch <= '\u{1F}' => escaped.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(not(all(target_os = "macos", not(coverage))))]
fn escape_svg(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '"' => "&quot;".to_string(),
            '\'' => "&apos;".to_string(),
            _ => ch.to_string(),
        })
        .collect()
}

fn capture_if_changed_screenshot(
    cli: &Cli,
    backend: &Backend,
    window: &WindowInfo,
    format: ImageFormat,
    output_path: &Path,
) -> Result<(), CliError> {
    let threshold = cli.if_changed_threshold.unwrap_or(0);
    let baseline_path = resolve_if_changed_baseline_path(cli, output_path)?;
    let baseline_hash = baseline_path.as_deref().map(hash_file_u64).transpose()?;

    let staged_path = staged_if_changed_path(output_path)?;
    let capture_result = backend.screenshot_window(window, &staged_path, format);
    if let Err(err) = capture_result {
        let _ = std::fs::remove_file(&staged_path);
        return Err(err);
    }

    let current_hash = match hash_file_u64(&staged_path) {
        Ok(value) => value,
        Err(err) => {
            let _ = std::fs::remove_file(&staged_path);
            return Err(err);
        }
    };

    let changed_by_threshold = baseline_hash
        .map(|baseline| hamming_distance(baseline, current_hash) > threshold)
        .unwrap_or(true);
    let changed = changed_by_threshold || !output_path.exists();
    if changed {
        publish_if_changed_capture(&staged_path, output_path)?;
    } else {
        let _ = std::fs::remove_file(&staged_path);
    }
    Ok(())
}

fn resolve_if_changed_baseline_path(
    cli: &Cli,
    output_path: &Path,
) -> Result<Option<PathBuf>, CliError> {
    if let Some(path) = cli.if_changed_baseline.as_ref() {
        let baseline_path = if path.is_absolute() {
            path.clone()
        } else {
            let cwd = std::env::current_dir()
                .map_err(|err| CliError::runtime(format!("failed to resolve cwd: {err}")))?;
            cwd.join(path)
        };
        if !baseline_path.exists() {
            return Err(CliError::runtime(format!(
                "--if-changed-baseline path does not exist: {}",
                baseline_path.display()
            )));
        }
        return Ok(Some(baseline_path));
    }

    if output_path.exists() {
        return Ok(Some(output_path.to_path_buf()));
    }
    Ok(None)
}

fn staged_if_changed_path(output_path: &Path) -> Result<PathBuf, CliError> {
    let parent = output_path
        .parent()
        .ok_or_else(|| CliError::runtime("missing output parent dir"))?;
    let name = output_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("screenshot");
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    Ok(parent.join(format!(".{name}.ifchanged-{pid}-{nanos}")))
}

fn publish_if_changed_capture(staged_path: &Path, output_path: &Path) -> Result<(), CliError> {
    if output_path.exists() {
        std::fs::remove_file(output_path)
            .map_err(|err| CliError::runtime(format!("failed to replace output file: {err}")))?;
    }
    std::fs::rename(staged_path, output_path)
        .map_err(|err| CliError::runtime(format!("failed to write output: {err}")))
}

fn hash_file_u64(path: &Path) -> Result<u64, CliError> {
    let bytes = std::fs::read(path).map_err(|err| {
        CliError::runtime(format!(
            "failed to read screenshot for --if-changed hash: {} ({err})",
            path.display()
        ))
    })?;
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    Ok(hasher.finish())
}

fn hamming_distance(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

fn determine_mode(cli: &Cli) -> Result<Mode, CliError> {
    let mut modes = Vec::new();
    if cli.list_windows {
        modes.push(Mode::ListWindows);
    }
    if cli.list_displays {
        modes.push(Mode::ListDisplays);
    }
    if cli.list_apps {
        modes.push(Mode::ListApps);
    }
    if cli.preflight {
        modes.push(Mode::Preflight);
    }
    if cli.request_permission {
        modes.push(Mode::RequestPermission);
    }
    if cli.screenshot {
        modes.push(Mode::Screenshot);
    }

    if modes.len() > 1 {
        return Err(CliError::usage(
            "select exactly one mode: --list-windows, --list-displays, --list-apps, --preflight, --request-permission, or --screenshot",
        ));
    }

    Ok(modes.pop().unwrap_or(Mode::Record))
}

fn validate_screenshot_flag_usage(cli: &Cli, mode: Mode) -> Result<(), CliError> {
    if mode != Mode::Screenshot && (cli.image_format.is_some() || cli.dir.is_some()) {
        return Err(CliError::usage("screenshot flags require --screenshot"));
    }
    Ok(())
}

fn validate_portal_flag_usage(cli: &Cli, mode: Mode) -> Result<(), CliError> {
    if !cli.portal {
        return Ok(());
    }

    match mode {
        Mode::Record | Mode::Screenshot => {}
        _ => {
            return Err(CliError::usage(
                "--portal is only valid with recording or --screenshot",
            ));
        }
    }

    #[cfg(target_os = "linux")]
    {
        let _ = mode;
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = mode;
        Err(CliError::usage(
            "--portal is only supported on Linux (Wayland)",
        ))
    }
}

fn validate_metadata_flag_usage(cli: &Cli, mode: Mode) -> Result<(), CliError> {
    if mode != Mode::Record && cli.metadata_out.is_some() {
        return Err(CliError::usage(
            "--metadata-out is only valid with recording",
        ));
    }
    Ok(())
}

fn validate_diagnostics_flag_usage(cli: &Cli, mode: Mode) -> Result<(), CliError> {
    if mode != Mode::Record && cli.diagnostics_out.is_some() {
        return Err(CliError::usage(
            "--diagnostics-out is only valid with recording",
        ));
    }
    Ok(())
}

fn validate_if_changed_flag_usage(cli: &Cli, mode: Mode) -> Result<(), CliError> {
    if !cli.if_changed {
        if cli.if_changed_baseline.is_some() || cli.if_changed_threshold.is_some() {
            return Err(CliError::usage(
                "--if-changed-baseline/--if-changed-threshold requires --if-changed",
            ));
        }
        return Ok(());
    }

    if mode != Mode::Screenshot {
        return Err(CliError::usage(
            "--if-changed is only valid with --screenshot",
        ));
    }

    Ok(())
}

fn validate_record_args(cli: &Cli) -> Result<(), CliError> {
    if cli.image_format.is_some() || cli.dir.is_some() {
        return Err(CliError::usage("screenshot flags require --screenshot"));
    }

    let selection_count = cli.window_id.is_some() as u8
        + cli.active_window as u8
        + cli.app.is_some() as u8
        + cli.portal as u8
        + cli.display as u8
        + cli.display_id.is_some() as u8;

    if selection_count != 1 {
        return Err(CliError::usage(
            "recording requires exactly one selector: --portal, --window-id, --active-window, --app, --display, or --display-id",
        ));
    }

    if cli.window_name.is_some() && cli.app.is_none() {
        return Err(CliError::usage("--window-name requires --app"));
    }

    if (cli.display || cli.display_id.is_some()) && cli.window_name.is_some() {
        return Err(CliError::usage("--window-name is only valid with --app"));
    }

    if cli.portal && cli.audio != AudioMode::Off {
        return Err(CliError::usage(
            "--portal does not support audio; use --audio off",
        ));
    }

    if cli.duration.is_none() {
        return Err(CliError::usage("--duration is required for recording"));
    }

    if cli.path.is_none() {
        return Err(CliError::usage("--path is required for recording"));
    }

    Ok(())
}

fn validate_screenshot_args(cli: &Cli) -> Result<(), CliError> {
    if !cli.screenshot {
        return Err(CliError::usage("missing --screenshot"));
    }

    if cli.display || cli.display_id.is_some() {
        return Err(CliError::usage(
            "display selectors are not valid with --screenshot",
        ));
    }

    let selection_count = cli.portal as u8
        + cli.window_id.is_some() as u8
        + cli.active_window as u8
        + cli.app.is_some() as u8;

    if selection_count != 1 {
        return Err(CliError::usage(
            "screenshot requires exactly one selector: --portal, --window-id, --active-window, or --app",
        ));
    }

    if cli.window_name.is_some() && cli.app.is_none() {
        return Err(CliError::usage("--window-name requires --app"));
    }

    if cli.duration.is_some() {
        return Err(CliError::usage("--duration is not valid with --screenshot"));
    }

    if cli.audio != AudioMode::Off {
        return Err(CliError::usage("--audio is not valid with --screenshot"));
    }

    if cli.format.is_some() {
        return Err(CliError::usage("--format is not valid with --screenshot"));
    }

    if cli.path.is_some() && cli.dir.is_some() {
        return Err(CliError::usage("use either --path or --dir"));
    }

    Ok(())
}

fn ensure_no_recording_flags(cli: &Cli) -> Result<(), CliError> {
    if cli.portal
        || cli.window_id.is_some()
        || cli.app.is_some()
        || cli.window_name.is_some()
        || cli.active_window
        || cli.display
        || cli.display_id.is_some()
        || cli.duration.is_some()
        || cli.screenshot
        || cli.path.is_some()
        || cli.format.is_some()
        || cli.image_format.is_some()
        || cli.dir.is_some()
        || cli.metadata_out.is_some()
        || cli.diagnostics_out.is_some()
        || cli.audio != AudioMode::Off
    {
        return Err(CliError::usage(
            "capture flags are not valid with this mode",
        ));
    }
    Ok(())
}

fn resolve_output_path(cli: &Cli) -> Result<PathBuf, CliError> {
    let mut path = cli
        .path
        .clone()
        .ok_or_else(|| CliError::usage("--path is required for recording"))?;

    if !path.is_absolute() {
        let cwd = std::env::current_dir()
            .map_err(|err| CliError::runtime(format!("failed to resolve cwd: {err}")))?;
        path = cwd.join(path);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| CliError::runtime(format!("failed to create output dir: {err}")))?;
    }

    Ok(path)
}

fn staged_recording_output_path(output_path: &Path) -> Result<PathBuf, CliError> {
    let parent = output_path
        .parent()
        .ok_or_else(|| CliError::runtime("missing output parent dir"))?;
    let name = output_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("recording");
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    Ok(parent.join(format!(".{name}.recording-{pid}-{nanos}")))
}

fn cleanup_recording_temp_file(path: &Path) {
    let _ = std::fs::remove_file(path);
}

fn publish_recording_output(staged_path: &Path, output_path: &Path) -> Result<(), CliError> {
    if output_path.exists() {
        std::fs::remove_file(output_path)
            .map_err(|err| CliError::runtime(format!("failed to replace output file: {err}")))?;
    }

    std::fs::rename(staged_path, output_path)
        .map_err(|err| CliError::runtime(format!("failed to write output: {err}")))
}

fn resolve_portal_screenshot_output(
    cli: &Cli,
    test_mode_enabled: bool,
) -> Result<(PathBuf, ImageFormat), CliError> {
    let format = resolve_image_format(cli.path.as_deref(), cli.image_format)?;
    let cwd = std::env::current_dir()
        .map_err(|err| CliError::runtime(format!("failed to resolve cwd: {err}")))?;

    if let Some(path) = cli.path.as_ref() {
        let mut path = if path.is_absolute() {
            path.clone()
        } else {
            cwd.join(path)
        };

        if path.extension().is_none() {
            path.set_extension(image_ext(format));
        }

        if path.is_dir() {
            return Err(CliError::usage("--path must be a file path"));
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| CliError::runtime(format!("failed to create output dir: {err}")))?;
        }

        return Ok((path, format));
    }

    let dir = cli
        .dir
        .as_ref()
        .map(|dir| {
            if dir.is_absolute() {
                dir.clone()
            } else {
                cwd.join(dir)
            }
        })
        .unwrap_or_else(|| cwd.join("screenshots"));

    if dir.exists() && !dir.is_dir() {
        return Err(CliError::usage("--dir must be a directory"));
    }

    std::fs::create_dir_all(&dir)
        .map_err(|err| CliError::runtime(format!("failed to create output dir: {err}")))?;

    let timestamp = screenshot_timestamp(test_mode_enabled);
    let stem = format!("screenshot-{timestamp}-portal");
    let ext = image_ext(format);

    let mut candidate = dir.join(format!("{stem}.{ext}"));
    if !candidate.exists() {
        return Ok((candidate, format));
    }

    for idx in 2..=u32::MAX {
        candidate = dir.join(format!("{stem}-{idx}.{ext}"));
        if !candidate.exists() {
            return Ok((candidate, format));
        }
    }

    Err(CliError::runtime("failed to choose a unique output path"))
}

fn resolve_screenshot_output(
    cli: &Cli,
    window: &WindowInfo,
    test_mode_enabled: bool,
) -> Result<(PathBuf, ImageFormat), CliError> {
    let format = resolve_image_format(cli.path.as_deref(), cli.image_format)?;

    let cwd = std::env::current_dir()
        .map_err(|err| CliError::runtime(format!("failed to resolve cwd: {err}")))?;

    if let Some(path) = cli.path.as_ref() {
        let mut path = if path.is_absolute() {
            path.clone()
        } else {
            cwd.join(path)
        };

        if path.extension().is_none() {
            path.set_extension(image_ext(format));
        }

        if path.is_dir() {
            return Err(CliError::usage("--path must be a file path"));
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| CliError::runtime(format!("failed to create output dir: {err}")))?;
        }

        return Ok((path, format));
    }

    let dir = cli
        .dir
        .as_ref()
        .map(|dir| {
            if dir.is_absolute() {
                dir.clone()
            } else {
                cwd.join(dir)
            }
        })
        .unwrap_or_else(|| cwd.join("screenshots"));

    if dir.exists() && !dir.is_dir() {
        return Err(CliError::usage("--dir must be a directory"));
    }

    std::fs::create_dir_all(&dir)
        .map_err(|err| CliError::runtime(format!("failed to create output dir: {err}")))?;

    let timestamp = screenshot_timestamp(test_mode_enabled);
    let stem = default_screenshot_stem(&timestamp, window);
    let ext = image_ext(format);

    let mut candidate = dir.join(format!("{stem}.{ext}"));
    if !candidate.exists() {
        return Ok((candidate, format));
    }

    for idx in 2..=u32::MAX {
        candidate = dir.join(format!("{stem}-{idx}.{ext}"));
        if !candidate.exists() {
            return Ok((candidate, format));
        }
    }

    Err(CliError::runtime("failed to choose a unique output path"))
}

fn resolve_image_format(
    path: Option<&Path>,
    format: Option<ImageFormat>,
) -> Result<ImageFormat, CliError> {
    let ext = path
        .and_then(|path| path.extension())
        .map(|value| value.to_string_lossy().to_ascii_lowercase());

    let ext_format = match ext.as_deref() {
        Some("png") => Some(ImageFormat::Png),
        Some("jpg") | Some("jpeg") => Some(ImageFormat::Jpg),
        Some("webp") => Some(ImageFormat::Webp),
        Some(_) => None,
        None => None,
    };

    if let Some(format) = format {
        if let Some(ext) = ext.as_deref()
            && !matches!(ext, "png" | "jpg" | "jpeg" | "webp")
        {
            return Err(CliError::usage(
                "unsupported --path extension for screenshot (supported: .png, .jpg, .jpeg, .webp)",
            ));
        }

        if let Some(ext_format) = ext_format
            && ext_format != format
        {
            return Err(CliError::usage(format!(
                "--image-format {} conflicts with --path extension",
                image_label(format)
            )));
        }
        return Ok(format);
    }

    if ext.is_some() {
        return ext_format.ok_or_else(|| {
            CliError::usage(
                "unsupported --path extension for screenshot (supported: .png, .jpg, .jpeg, .webp)",
            )
        });
    }

    Ok(ImageFormat::Png)
}

fn image_label(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpg => "jpg",
        ImageFormat::Webp => "webp",
    }
}

fn image_ext(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpg => "jpg",
        ImageFormat::Webp => "webp",
    }
}

fn screenshot_timestamp(test_mode_enabled: bool) -> String {
    if test_mode_enabled {
        if let Some(value) = std::env::var_os("AGENTS_SCREEN_RECORD_TEST_TIMESTAMP") {
            let value = value.to_string_lossy().trim().to_string();
            if !value.is_empty() {
                return value;
            }
        }
        return "20260101-000000".to_string();
    }

    Local::now().format("%Y%m%d-%H%M%S").to_string()
}

fn default_screenshot_stem(timestamp: &str, window: &WindowInfo) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push("screenshot".to_string());
    parts.push(timestamp.to_string());
    parts.push(format!("win{}", window.id));

    if let Some(owner) = sanitize_filename_segment(&window.owner_name) {
        parts.push(owner);
    }
    if let Some(title) = sanitize_filename_segment(&window.title) {
        parts.push(title);
    }

    parts.join("-")
}

fn sanitize_filename_segment(value: &str) -> Option<String> {
    const MAX_CHARS: usize = 48;

    let mut out = String::new();
    let mut last_was_dash = false;
    let mut chars = 0usize;

    for ch in value.chars() {
        if chars >= MAX_CHARS {
            break;
        }

        let is_ok = ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.');
        if is_ok {
            out.push(ch);
            last_was_dash = false;
            chars += 1;
            continue;
        }

        if ch.is_whitespace() || ch.is_ascii_punctuation() || ch.is_control() {
            if !out.is_empty() && !last_was_dash {
                out.push('-');
                last_was_dash = true;
                chars += 1;
            }
            continue;
        }

        // Default: treat unsupported chars as separators, but keep the string UTF-8 safe.
        if !out.is_empty() && !last_was_dash {
            out.push('-');
            last_was_dash = true;
            chars += 1;
        }
    }

    let trimmed = out.trim_matches(&['-', '_', '.'][..]).to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn resolve_container(
    path: &Path,
    format: Option<ContainerFormat>,
) -> Result<ContainerFormat, CliError> {
    let ext = path
        .extension()
        .map(|value| value.to_string_lossy().to_ascii_lowercase());

    let ext_format = match ext.as_deref() {
        Some("mov") => Some(ContainerFormat::Mov),
        Some("mp4") => Some(ContainerFormat::Mp4),
        _ => None,
    };

    if let Some(format) = format {
        if let Some(ext_format) = ext_format
            && ext_format != format
        {
            return Err(CliError::usage(format!(
                "--format {} conflicts with --path extension",
                format_label(format)
            )));
        }
        return Ok(format);
    }

    Ok(ext_format.unwrap_or(ContainerFormat::Mov))
}

fn format_label(format: ContainerFormat) -> &'static str {
    match format {
        ContainerFormat::Mov => "mov",
        ContainerFormat::Mp4 => "mp4",
    }
}

fn fetch_shareable_content(backend: &Backend) -> Result<ShareableContent, CliError> {
    backend.shareable_content()
}

fn ensure_linux_x11_only_mode_allowed() -> Result<(), CliError> {
    #[cfg(target_os = "linux")]
    {
        match linux_session_kind() {
            LinuxSessionKind::X11 => Ok(()),
            LinuxSessionKind::WaylandOnly => Err(CliError::usage(
                "X11-only mode is unavailable on Wayland-only sessions. Use --portal for recording/screenshot, or log into \"Ubuntu on Xorg\".",
            )),
            LinuxSessionKind::NoDisplay => Err(CliError::runtime(
                "X11 display not detected (DISPLAY is unset).",
            )),
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        Ok(())
    }
}

fn ensure_linux_x11_selectors_allowed() -> Result<(), CliError> {
    #[cfg(target_os = "linux")]
    {
        match linux_session_kind() {
            LinuxSessionKind::X11 => Ok(()),
            LinuxSessionKind::WaylandOnly => Err(CliError::usage(
                "X11 selectors require X11 (DISPLAY is unset). Use --portal on Wayland-only sessions, or log into \"Ubuntu on Xorg\".",
            )),
            LinuxSessionKind::NoDisplay => Err(CliError::runtime(
                "X11 display not detected (DISPLAY is unset).",
            )),
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinuxSessionKind {
    X11,
    WaylandOnly,
    NoDisplay,
}

#[cfg(target_os = "linux")]
fn linux_session_kind() -> LinuxSessionKind {
    let display = std::env::var_os("DISPLAY")
        .map(|value| value.to_string_lossy().trim().to_string())
        .filter(|value| !value.is_empty());
    if display.is_some() {
        return LinuxSessionKind::X11;
    }

    let wayland = std::env::var_os("WAYLAND_DISPLAY")
        .map(|value| value.to_string_lossy().trim().to_string())
        .filter(|value| !value.is_empty());
    if wayland.is_some() {
        return LinuxSessionKind::WaylandOnly;
    }

    LinuxSessionKind::NoDisplay
}

fn normalize_app_list(apps: &mut Vec<AppInfo>) {
    apps.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.pid.cmp(&b.pid)));
    apps.dedup_by(|a, b| a.name == b.name && a.pid == b.pid && a.bundle_id == b.bundle_id);
}

fn format_app_tsv(app: &AppInfo) -> String {
    format!(
        "{}\t{}\t{}",
        normalize_tsv_field(&app.name),
        app.pid,
        normalize_tsv_field(&app.bundle_id)
    )
}

fn format_display_tsv(display: &DisplayInfo) -> String {
    format!("{}\t{}\t{}", display.id, display.width, display.height)
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn base_record_cli() -> Cli {
        Cli {
            screenshot: false,
            list_windows: false,
            list_displays: false,
            list_apps: false,
            preflight: false,
            request_permission: false,
            window_id: None,
            app: None,
            window_name: None,
            active_window: false,
            display: false,
            display_id: None,
            portal: false,
            duration: Some(1),
            audio: AudioMode::Off,
            path: Some(PathBuf::from("out.mp4")),
            metadata_out: None,
            diagnostics_out: None,
            format: None,
            image_format: None,
            dir: None,
            if_changed: false,
            if_changed_baseline: None,
            if_changed_threshold: None,
        }
    }

    fn base_screenshot_cli() -> Cli {
        Cli {
            screenshot: true,
            list_windows: false,
            list_displays: false,
            list_apps: false,
            preflight: false,
            request_permission: false,
            window_id: None,
            app: Some("Terminal".to_string()),
            window_name: None,
            active_window: false,
            display: false,
            display_id: None,
            portal: false,
            duration: None,
            audio: AudioMode::Off,
            path: None,
            metadata_out: None,
            diagnostics_out: None,
            format: None,
            image_format: None,
            dir: None,
            if_changed: false,
            if_changed_baseline: None,
            if_changed_threshold: None,
        }
    }

    fn sample_window(id: u32, owner_name: &str, title: &str) -> WindowInfo {
        WindowInfo {
            id,
            owner_name: owner_name.to_string(),
            title: title.to_string(),
            bounds: crate::types::Rect::default(),
            on_screen: true,
            active: true,
            owner_pid: 1,
            z_order: 0,
        }
    }

    #[test]
    fn portal_requires_audio_off() {
        let mut cli = base_record_cli();
        cli.portal = true;
        cli.audio = AudioMode::System;
        let err = validate_record_args(&cli).expect_err("usage error");
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("--portal does not support audio"));
    }

    #[test]
    fn portal_requires_record_or_screenshot_mode() {
        let mut cli = base_record_cli();
        cli.portal = true;
        let err = validate_portal_flag_usage(&cli, Mode::ListApps).expect_err("usage error");
        assert_eq!(err.exit_code(), 2);
        assert!(
            err.to_string()
                .contains("--portal is only valid with recording or --screenshot")
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn portal_is_rejected_on_non_linux_even_in_valid_mode() {
        let mut cli = base_record_cli();
        cli.portal = true;
        let err = validate_portal_flag_usage(&cli, Mode::Record).expect_err("usage error");
        assert_eq!(err.exit_code(), 2);
        assert!(
            err.to_string()
                .contains("--portal is only supported on Linux (Wayland)")
        );
    }

    #[test]
    fn record_window_name_requires_app() {
        let mut cli = base_record_cli();
        cli.window_id = Some(100);
        cli.path = Some(PathBuf::from("recording.mov"));
        cli.window_name = Some("Inbox".to_string());
        let err = validate_record_args(&cli).expect_err("usage error");
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("--window-name requires --app"));
    }

    #[test]
    fn screenshot_window_name_requires_app() {
        let mut cli = base_screenshot_cli();
        cli.app = None;
        cli.window_id = Some(100);
        cli.window_name = Some("Inbox".to_string());
        let err = validate_screenshot_args(&cli).expect_err("usage error");
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("--window-name requires --app"));
    }

    #[test]
    fn screenshot_rejects_recording_format_flag() {
        let mut cli = base_screenshot_cli();
        cli.format = Some(ContainerFormat::Mov);
        let err = validate_screenshot_args(&cli).expect_err("usage error");
        assert_eq!(err.exit_code(), 2);
        assert!(
            err.to_string()
                .contains("--format is not valid with --screenshot")
        );
    }

    #[test]
    fn diagnostics_out_requires_record_mode() {
        let mut cli = base_screenshot_cli();
        cli.diagnostics_out = Some(PathBuf::from("diag.json"));
        let err = validate_diagnostics_flag_usage(&cli, Mode::Screenshot).expect_err("usage");
        assert_eq!(err.exit_code(), 2);
        assert!(
            err.to_string()
                .contains("--diagnostics-out is only valid with recording")
        );
    }

    #[test]
    fn ensure_no_recording_flags_rejects_capture_inputs() {
        let mut cli = base_record_cli();
        cli.duration = None;
        cli.path = None;
        cli.app = Some("Terminal".to_string());
        let err = ensure_no_recording_flags(&cli).expect_err("usage error");
        assert_eq!(err.exit_code(), 2);
        assert!(
            err.to_string()
                .contains("capture flags are not valid with this mode")
        );
    }

    #[test]
    fn resolve_container_defaults_to_mov_for_unknown_extension() {
        let format = resolve_container(Path::new("capture.mkv"), None).expect("format");
        assert_eq!(format, ContainerFormat::Mov);
    }

    #[test]
    fn resolve_container_uses_path_extension_when_format_unspecified() {
        let format = resolve_container(Path::new("capture.mp4"), None).expect("format");
        assert_eq!(format, ContainerFormat::Mp4);
    }

    #[test]
    fn resolve_container_conflict_returns_usage_error() {
        let err = resolve_container(Path::new("capture.mov"), Some(ContainerFormat::Mp4))
            .expect_err("usage error");
        assert_eq!(err.exit_code(), 2);
        assert!(
            err.to_string()
                .contains("--format mp4 conflicts with --path extension")
        );
    }

    #[test]
    fn resolve_image_format_defaults_to_png() {
        let format = resolve_image_format(None, None).expect("format");
        assert_eq!(format, ImageFormat::Png);
    }

    #[test]
    fn resolve_image_format_reads_extension_when_flag_is_absent() {
        let format = resolve_image_format(Some(Path::new("shot.JPEG")), None).expect("format");
        assert_eq!(format, ImageFormat::Jpg);
    }

    #[test]
    fn resolve_image_format_conflict_returns_usage_error() {
        let err = resolve_image_format(Some(Path::new("shot.png")), Some(ImageFormat::Webp))
            .expect_err("usage error");
        assert_eq!(err.exit_code(), 2);
        assert!(
            err.to_string()
                .contains("--image-format webp conflicts with --path extension")
        );
    }

    #[test]
    fn resolve_image_format_rejects_unknown_extension() {
        let err = resolve_image_format(Some(Path::new("shot.tiff")), None).expect_err("usage");
        assert_eq!(err.exit_code(), 2);
        assert!(
            err.to_string()
                .contains("unsupported --path extension for screenshot")
        );
    }

    #[test]
    fn resolve_image_format_rejects_unknown_extension_even_with_flag() {
        let err = resolve_image_format(Some(Path::new("shot.bmp")), Some(ImageFormat::Png))
            .expect_err("usage");
        assert_eq!(err.exit_code(), 2);
        assert!(
            err.to_string()
                .contains("unsupported --path extension for screenshot")
        );
    }

    #[test]
    fn resolve_portal_screenshot_output_uses_portal_stem() {
        let dir = TempDir::new().expect("tempdir");
        let mut cli = base_screenshot_cli();
        cli.portal = true;
        cli.app = None;
        cli.dir = Some(dir.path().to_path_buf());

        let (path, format) =
            resolve_portal_screenshot_output(&cli, true).expect("portal screenshot output");
        assert_eq!(format, ImageFormat::Png);
        assert_eq!(
            path,
            dir.path().join("screenshot-20260101-000000-portal.png")
        );
    }

    #[test]
    fn resolve_portal_screenshot_output_adds_collision_suffix() {
        let dir = TempDir::new().expect("tempdir");
        let existing = dir.path().join("screenshot-20260101-000000-portal.png");
        std::fs::write(&existing, b"existing").expect("write existing");

        let mut cli = base_screenshot_cli();
        cli.portal = true;
        cli.app = None;
        cli.dir = Some(dir.path().to_path_buf());

        let (path, _) =
            resolve_portal_screenshot_output(&cli, true).expect("portal screenshot output");
        assert_eq!(
            path,
            dir.path().join("screenshot-20260101-000000-portal-2.png")
        );
    }

    #[test]
    fn resolve_portal_screenshot_output_rejects_file_dir() {
        let dir = TempDir::new().expect("tempdir");
        let not_a_dir = dir.path().join("not-a-dir");
        std::fs::write(&not_a_dir, b"file").expect("write file");

        let mut cli = base_screenshot_cli();
        cli.portal = true;
        cli.app = None;
        cli.dir = Some(not_a_dir);

        let err = resolve_portal_screenshot_output(&cli, true).expect_err("usage error");
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("--dir must be a directory"));
    }

    #[test]
    fn sanitize_filename_segment_normalizes_punctuation_and_control_chars() {
        let normalized = sanitize_filename_segment("Termi\tnal!!\nInbox").expect("segment");
        assert_eq!(normalized, "Termi-nal-Inbox");
        assert_eq!(sanitize_filename_segment(" \n\t!!!\r"), None);
    }

    #[test]
    fn sanitize_filename_segment_truncates_to_max_length() {
        let input = "a".repeat(80);
        let normalized = sanitize_filename_segment(&input).expect("segment");
        assert_eq!(normalized, "a".repeat(48));
        assert_eq!(normalized.len(), 48);
    }

    #[test]
    fn default_screenshot_stem_uses_sanitized_segments() {
        let window = sample_window(17, "Termi\tnal!!", "  \nInbox??\r");
        let stem = default_screenshot_stem("20260101-000000", &window);
        assert_eq!(stem, "screenshot-20260101-000000-win17-Termi-nal-Inbox");
    }

    #[test]
    fn format_app_tsv_normalizes_tabs_and_newlines() {
        let app = AppInfo {
            name: "Termi\t\nnal".to_string(),
            pid: 42,
            bundle_id: "com.example.\n\tapp".to_string(),
        };
        let line = format_app_tsv(&app);
        assert_eq!(line, "Termi  nal\t42\tcom.example.  app");
    }

    #[test]
    fn staged_recording_output_path_uses_hidden_temp_name() {
        let dir = TempDir::new().expect("tempdir");
        let output = dir.path().join("captures").join("clip.mov");
        std::fs::create_dir_all(output.parent().expect("parent")).expect("create dir");

        let staged = staged_recording_output_path(&output).expect("staged path");
        let parent = staged.parent().expect("parent");
        let name = staged
            .file_name()
            .and_then(|value| value.to_str())
            .expect("filename");

        assert_eq!(parent, output.parent().expect("output parent"));
        assert!(name.starts_with(".clip.mov.recording-"));
    }

    #[test]
    fn publish_recording_output_replaces_existing_target() {
        let dir = TempDir::new().expect("tempdir");
        let output = dir.path().join("clip.mov");
        let staged = dir.path().join(".clip.mov.recording-temp");

        std::fs::write(&output, b"old").expect("write old output");
        std::fs::write(&staged, b"new").expect("write staged output");

        publish_recording_output(&staged, &output).expect("publish output");

        assert!(!staged.exists(), "staged file should be moved away");
        assert_eq!(std::fs::read(&output).expect("read output"), b"new");
    }
}
