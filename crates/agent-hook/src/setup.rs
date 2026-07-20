use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nils_common::fs::write_atomic;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use toml_edit::{DocumentMut, Item as TomlItem, Value as TomlValue};

use crate::contract::{digest, digest_serializable, runtime_handler_filename};
use crate::error::HookError;
use crate::model::{Capability, LoadedPolicy, Product, SetupAction};
use crate::paths::Layout;
use crate::strict_json;

const CODEX_BLOCK_START: &str = "# >>> agent-hook:provider-ingress:v1 >>>";
const CODEX_BLOCK_END: &str = "# <<< agent-hook:provider-ingress:v1 <<<";
const RUNTIME_KIT_BLOCK_START: &str = "# >>> agent-runtime-kit:hooks >>>";
const RUNTIME_KIT_BLOCK_END: &str = "# <<< agent-runtime-kit:hooks <<<";
const DISPATCH_TIMEOUT_SECONDS: i64 = 10;
const CODEX_NOTIFY_ARGV: [&str; 5] = ["agent-session", "activity", "notify", "--agent", "codex"];
const CODEX_NOTIFY_FORWARD_FLAG: &str = "--forward-notify-argv-json";
const MAX_CODEX_FORWARD_ARGS: usize = 64;
const MAX_CODEX_FORWARD_ARGV_BYTES: usize = 16 * 1024;
const MAX_PROVIDER_CONFIG_BYTES: usize = 1024 * 1024;
const LOCK_TIMEOUT: Duration = Duration::from_secs(2);

type JsonMigration = (Option<Vec<u8>>, usize, usize, usize, bool);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct HookGroup {
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderStatus {
    Missing,
    CompatibilityOnly,
    Dual,
    Drifted,
    Converged,
    Unsupported,
    Unrelated,
}

impl std::fmt::Display for ProviderStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Missing => "missing",
            Self::CompatibilityOnly => "compatibility-only",
            Self::Dual => "dual",
            Self::Drifted => "drifted",
            Self::Converged => "converged",
            Self::Unsupported => "unsupported",
            Self::Unrelated => "unrelated",
        };
        formatter.write_str(value)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SetupResult {
    pub schema_version: String,
    pub product: String,
    pub action: String,
    pub status: ProviderStatus,
    pub changed: bool,
    pub would_change: bool,
    pub configured: bool,
    pub would_configure: bool,
    pub apply_allowed: bool,
    pub plan_digest: String,
    pub config_digest: String,
    pub policy_digest: String,
    pub owned_events: Vec<String>,
    pub owned_groups: Vec<HookGroup>,
    pub owned_count: usize,
    pub legacy_residue_count: usize,
    pub unrelated_count: usize,
    pub compatibility_owner: String,
    pub trust: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DoctorResult {
    pub schema_version: String,
    pub product: String,
    pub status: ProviderStatus,
    pub supported: bool,
    pub owned_count: usize,
    pub expected_owned_count: usize,
    pub legacy_residue_count: usize,
    pub unrelated_count: usize,
    pub config_digest: String,
    pub policy_digest: String,
    pub recovery: String,
}

struct Plan {
    product: Product,
    files: Vec<PlannedFile>,
    primary_path: PathBuf,
    groups: Vec<HookGroup>,
    owned_before: usize,
    owned_after: usize,
    legacy_before: usize,
    unrelated_before: usize,
    drifted: bool,
    auxiliary_configured_before: bool,
    auxiliary_configured_after: bool,
}

struct PlannedFile {
    path: PathBuf,
    original: Option<Vec<u8>>,
    candidate: Option<Vec<u8>>,
    original_mode: Option<u32>,
}

pub fn run(
    layout: &Layout,
    loaded: &LoadedPolicy,
    product: Product,
    action: SetupAction,
    expected_plan_digest: Option<&str>,
) -> Result<SetupResult, HookError> {
    if !product.enforceable() {
        let groups = policy_groups(loaded, product);
        return Ok(SetupResult {
            schema_version: "agent-hook.setup-result.v1".to_string(),
            product: product.as_str().to_string(),
            action: action.as_str().to_string(),
            status: ProviderStatus::Unsupported,
            changed: false,
            would_change: false,
            configured: false,
            would_configure: false,
            apply_allowed: false,
            plan_digest: digest(b"unsupported"),
            config_digest: loaded.config_digest.clone(),
            policy_digest: loaded.policy_digest.clone(),
            owned_events: distinct_events(&groups),
            owned_count: 0,
            owned_groups: groups,
            legacy_residue_count: 0,
            unrelated_count: 0,
            compatibility_owner: "agent-hook".to_string(),
            trust: "provider has no compatible native hook runner; no files changed".to_string(),
        });
    }
    let plan = build_plan(loaded, product, action)?;
    let plan_digest = plan_digest(&plan)?;
    let requires_review = plan.legacy_before > 0 || plan.drifted;
    let apply_allowed =
        !requires_review || expected_plan_digest.is_some_and(|expected| expected == plan_digest);
    if !matches!(action, SetupAction::DryRun) && requires_review && expected_plan_digest.is_none() {
        return Err(HookError::data(
            "setup-plan-digest-required",
            "compatibility or drifted provider state requires the exact reviewed plan digest",
        ));
    }
    if expected_plan_digest.is_some_and(|expected| expected != plan_digest) {
        return Err(HookError::data(
            "setup-plan-digest-mismatch",
            "provider setup plan changed after review",
        ));
    }
    let would_change = plan
        .files
        .iter()
        .any(|file| file.original != file.candidate);
    let mut changed = false;
    if !matches!(action, SetupAction::DryRun) && would_change {
        apply_plan(layout, &plan)?;
        changed = true;
    }
    let configured = if matches!(action, SetupAction::DryRun) {
        plan.owned_before == plan.groups.len()
            && !plan.drifted
            && plan.legacy_before == 0
            && plan.auxiliary_configured_before
    } else {
        plan.owned_after == plan.groups.len()
            && action != SetupAction::Remove
            && plan.auxiliary_configured_after
    };
    let would_configure = action != SetupAction::Remove
        && plan.owned_after == plan.groups.len()
        && plan.auxiliary_configured_after;
    let status = classify_status(
        plan.owned_before,
        plan.groups.len(),
        plan.legacy_before,
        plan.unrelated_before,
        plan.drifted,
    );
    Ok(SetupResult {
        schema_version: "agent-hook.setup-result.v1".to_string(),
        product: product.as_str().to_string(),
        action: action.as_str().to_string(),
        status,
        changed,
        would_change,
        configured,
        would_configure,
        apply_allowed,
        plan_digest,
        config_digest: loaded.config_digest.clone(),
        policy_digest: loaded.policy_digest.clone(),
        owned_events: distinct_events(&plan.groups),
        owned_groups: plan.groups,
        owned_count: if matches!(action, SetupAction::DryRun) {
            plan.owned_before
        } else {
            plan.owned_after
        },
        legacy_residue_count: if changed { 0 } else { plan.legacy_before },
        unrelated_count: plan.unrelated_before,
        compatibility_owner: "agent-hook".to_string(),
        trust: "review the content-free plan digest and owned event/matcher groups before apply"
            .to_string(),
    })
}

pub fn doctor(loaded: &LoadedPolicy, product: Product) -> Result<DoctorResult, HookError> {
    if !product.enforceable() {
        return Ok(DoctorResult {
            schema_version: "agent-hook.doctor.v1".to_string(),
            product: product.as_str().to_string(),
            status: ProviderStatus::Unsupported,
            supported: false,
            owned_count: 0,
            expected_owned_count: policy_groups(loaded, product).len(),
            legacy_residue_count: 0,
            unrelated_count: 0,
            config_digest: loaded.config_digest.clone(),
            policy_digest: loaded.policy_digest.clone(),
            recovery: "available-for-shared-policy-only".to_string(),
        });
    }
    let plan = build_plan(loaded, product, SetupAction::DryRun)?;
    Ok(DoctorResult {
        schema_version: "agent-hook.doctor.v1".to_string(),
        product: product.as_str().to_string(),
        status: classify_status(
            plan.owned_before,
            plan.groups.len(),
            plan.legacy_before,
            plan.unrelated_before,
            plan.drifted || (plan.owned_before > 0 && !plan.auxiliary_configured_before),
        ),
        supported: true,
        owned_count: plan.owned_before,
        expected_owned_count: plan.groups.len(),
        legacy_residue_count: plan.legacy_before,
        unrelated_count: plan.unrelated_before,
        config_digest: loaded.config_digest.clone(),
        policy_digest: loaded.policy_digest.clone(),
        recovery: "challenge-authorize-consume".to_string(),
    })
}

pub fn policy_groups(loaded: &LoadedPolicy, product: Product) -> Vec<HookGroup> {
    let mut seen = BTreeSet::new();
    let mut groups = Vec::new();
    for rule in &loaded.bundle.rules {
        if !rule.products.contains(&product) {
            continue;
        }
        for event in &rule.events {
            let group = HookGroup {
                event: event.clone(),
                matcher: rule.matcher.clone(),
            };
            if seen.insert((group.event.clone(), group.matcher.clone())) {
                groups.push(group);
            }
        }
    }
    groups
}

fn build_plan(
    loaded: &LoadedPolicy,
    product: Product,
    action: SetupAction,
) -> Result<Plan, HookError> {
    let groups = policy_groups(loaded, product);
    let path = provider_path(product)?;
    let original = read_optional_config(&path)?;
    match product {
        Product::Codex => build_codex_plan(loaded, product, action, path, original, groups),
        Product::Claude => build_json_plan(product, action, path, original, groups, loaded),
        Product::Hermes => unreachable!("unsupported returned before plan"),
    }
}

fn build_codex_plan(
    loaded: &LoadedPolicy,
    product: Product,
    action: SetupAction,
    path: PathBuf,
    original: Option<Vec<u8>>,
    groups: Vec<HookGroup>,
) -> Result<Plan, HookError> {
    let raw = original
        .as_deref()
        .map(std::str::from_utf8)
        .transpose()
        .map_err(|_| HookError::data("provider-config-invalid", "Codex config is not UTF-8"))?
        .unwrap_or_default();
    let (boundary_repaired_raw, trust_boundary_repaired) = repair_codex_trust_boundary(raw)?;
    boundary_repaired_raw.parse::<DocumentMut>().map_err(|_| {
        HookError::data("provider-config-invalid", "Codex config is not valid TOML")
    })?;
    let expected_block = render_codex_block(&groups, product);
    let (stripped, owned_before, drifted) =
        strip_codex_block(&boundary_repaired_raw, &expected_block)?;
    let mut document = stripped.parse::<DocumentMut>().map_err(|_| {
        HookError::data("provider-config-invalid", "Codex config is not valid TOML")
    })?;
    let (legacy_before, unrelated_before) = inspect_toml_handlers(&document);
    let notify = plan_codex_notification(&mut document, action, &stripped)?;
    remove_legacy_toml_handlers(&mut document);
    let mut rendered = document.to_string();
    if action != SetupAction::Remove {
        if !rendered.is_empty() && !rendered.ends_with('\n') {
            rendered.push('\n');
        }
        rendered.push_str(&expected_block);
    }
    let mut candidate = if rendered.is_empty() {
        None
    } else {
        Some(rendered.into_bytes())
    };
    if action == SetupAction::Remove
        && owned_before == 0
        && legacy_before == 0
        && !notify.changed
        && !trust_boundary_repaired
    {
        candidate = original.clone();
    }
    validate_candidate_size(candidate.as_deref())?;
    let original_mode = file_mode(&path)?;
    let hooks_path = path.with_file_name("hooks.json");
    let hooks_original = read_optional_config(&hooks_path)?;
    let (hooks_candidate, hooks_owned, hooks_legacy, hooks_unrelated, hooks_drifted) =
        build_codex_json_migration(hooks_original.as_deref(), product, &groups, loaded)?;
    let hooks_mode = file_mode(&hooks_path)?;
    Ok(Plan {
        product,
        primary_path: path.clone(),
        files: vec![
            PlannedFile {
                path,
                original,
                candidate,
                original_mode,
            },
            PlannedFile {
                path: hooks_path,
                original: hooks_original,
                candidate: hooks_candidate,
                original_mode: hooks_mode,
            },
        ],
        groups: groups.clone(),
        owned_before,
        owned_after: if action == SetupAction::Remove {
            0
        } else {
            groups.len()
        },
        legacy_before: legacy_before + hooks_owned + hooks_legacy,
        unrelated_before: unrelated_before + hooks_unrelated,
        drifted: drifted || hooks_drifted || notify.requires_review || trust_boundary_repaired,
        auxiliary_configured_before: notify.configured_before,
        auxiliary_configured_after: notify.configured_after,
    })
}

struct CodexNotificationPlan {
    changed: bool,
    configured_before: bool,
    configured_after: bool,
    requires_review: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CodexNotifyMode {
    Absent,
    Owned,
    Composed(Vec<String>),
    Foreign(Vec<String>),
    Invalid,
}

fn plan_codex_notification(
    document: &mut DocumentMut,
    action: SetupAction,
    original_raw: &str,
) -> Result<CodexNotificationPlan, HookError> {
    let mode = codex_notify_mode(document);
    let configured_before = matches!(mode, CodexNotifyMode::Owned | CodexNotifyMode::Composed(_));
    let mut requires_review = false;
    if action == SetupAction::Remove {
        match mode {
            CodexNotifyMode::Owned => {
                document.remove("notify");
            }
            CodexNotifyMode::Composed(forwarded) => set_codex_notify(document, &forwarded),
            CodexNotifyMode::Absent | CodexNotifyMode::Foreign(_) | CodexNotifyMode::Invalid => {}
        }
    } else {
        match mode {
            CodexNotifyMode::Absent => {
                set_codex_notify(document, &CODEX_NOTIFY_ARGV.map(str::to_string))
            }
            CodexNotifyMode::Owned | CodexNotifyMode::Composed(_) => {}
            CodexNotifyMode::Foreign(forwarded) if codex_forward_argv_is_safe(&forwarded) => {
                let encoded = serde_json::to_string(&forwarded).map_err(|_| {
                    HookError::data(
                        "provider-notification-config-invalid",
                        "Codex user notify argv could not be encoded",
                    )
                })?;
                if encoded.len() > MAX_CODEX_FORWARD_ARGV_BYTES {
                    return Err(HookError::data(
                        "provider-notification-config-conflict",
                        "Codex user notify argv exceeds the safe composition limit",
                    ));
                }
                let mut composed = CODEX_NOTIFY_ARGV.map(str::to_string).to_vec();
                composed.push(CODEX_NOTIFY_FORWARD_FLAG.to_string());
                composed.push(encoded);
                set_codex_notify(document, &composed);
                let mut restored = document.clone();
                set_codex_notify(&mut restored, &forwarded);
                if restored.to_string().as_bytes() != original_raw.as_bytes() {
                    return Err(HookError::data(
                        "provider-notification-config-nonreversible",
                        "Codex user notify argv cannot be composed with byte-exact removal",
                    ));
                }
                requires_review = true;
            }
            CodexNotifyMode::Foreign(_) | CodexNotifyMode::Invalid => {
                return Err(HookError::data(
                    "provider-notification-config-conflict",
                    "Codex notify configuration cannot be composed safely",
                ));
            }
        }
    }
    let configured_after = matches!(
        codex_notify_mode(document),
        CodexNotifyMode::Owned | CodexNotifyMode::Composed(_)
    );
    let changed = document.to_string().as_bytes() != original_raw.as_bytes();
    Ok(CodexNotificationPlan {
        changed,
        configured_before,
        configured_after,
        requires_review,
    })
}

fn codex_notify_mode(document: &DocumentMut) -> CodexNotifyMode {
    let Some(item) = document.get("notify") else {
        return CodexNotifyMode::Absent;
    };
    let Some(array) = item.as_array() else {
        return CodexNotifyMode::Invalid;
    };
    let Some(argv) = array
        .iter()
        .map(|value| value.as_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()
    else {
        return CodexNotifyMode::Invalid;
    };
    if argv_matches(&argv, &CODEX_NOTIFY_ARGV) {
        return CodexNotifyMode::Owned;
    }
    if argv.len() == CODEX_NOTIFY_ARGV.len() + 2
        && argv_matches(&argv[..CODEX_NOTIFY_ARGV.len()], &CODEX_NOTIFY_ARGV)
        && argv[CODEX_NOTIFY_ARGV.len()] == CODEX_NOTIFY_FORWARD_FLAG
        && let Ok(forwarded) =
            serde_json::from_str::<Vec<String>>(&argv[CODEX_NOTIFY_ARGV.len() + 1])
        && codex_forward_argv_is_safe(&forwarded)
    {
        return CodexNotifyMode::Composed(forwarded);
    }
    CodexNotifyMode::Foreign(argv)
}

fn argv_matches(argv: &[String], expected: &[&str]) -> bool {
    argv.len() == expected.len()
        && argv
            .iter()
            .zip(expected)
            .all(|(value, expected)| value == expected)
}

fn codex_forward_argv_is_safe(argv: &[String]) -> bool {
    !argv.is_empty()
        && argv.len() <= MAX_CODEX_FORWARD_ARGS
        && !argv[0].trim().is_empty()
        && argv.iter().map(String::len).sum::<usize>() <= MAX_CODEX_FORWARD_ARGV_BYTES
        && !argv.iter().any(|value| value == CODEX_NOTIFY_FORWARD_FLAG)
        && (argv.len() < CODEX_NOTIFY_ARGV.len()
            || !argv_matches(&argv[..CODEX_NOTIFY_ARGV.len()], &CODEX_NOTIFY_ARGV))
}

fn set_codex_notify(document: &mut DocumentMut, argv: &[String]) {
    let mut array = toml_edit::Array::new();
    array.extend(argv.iter().map(String::as_str));
    document["notify"] = toml_edit::value(array);
}

fn build_codex_json_migration(
    original: Option<&[u8]>,
    product: Product,
    groups: &[HookGroup],
    loaded: &LoadedPolicy,
) -> Result<JsonMigration, HookError> {
    let Some(bytes) = original else {
        return Ok((None, 0, 0, 0, false));
    };
    let mut root = strict_json::from_slice(bytes).map_err(|_| {
        HookError::data(
            "provider-config-invalid",
            "Codex hooks.json is not valid JSON",
        )
    })?;
    if !root.is_object() {
        return Err(HookError::data(
            "provider-config-invalid",
            "Codex hooks.json root must be an object",
        ));
    }
    let (owned, compatibility, unrelated, drifted) =
        inspect_json_handlers(&root, product, groups, loaded)?;
    if owned == 0 && compatibility == 0 {
        return Ok((
            Some(bytes.to_vec()),
            owned,
            compatibility,
            unrelated,
            drifted,
        ));
    }
    remove_owned_and_legacy_json(&mut root, product, loaded)?;
    let candidate = if root.as_object().is_some_and(Map::is_empty) {
        None
    } else {
        Some(serde_json::to_vec_pretty(&root).map_err(|_| {
            HookError::runtime(
                "provider-config-render-failed",
                "Codex hooks.json render failed",
            )
        })?)
    };
    validate_candidate_size(candidate.as_deref())?;
    Ok((candidate, owned, compatibility, unrelated, drifted))
}

fn build_json_plan(
    product: Product,
    action: SetupAction,
    path: PathBuf,
    original: Option<Vec<u8>>,
    groups: Vec<HookGroup>,
    loaded: &LoadedPolicy,
) -> Result<Plan, HookError> {
    let mut root = if let Some(bytes) = original.as_deref() {
        strict_json::from_slice(bytes).map_err(|_| {
            HookError::data(
                "provider-config-invalid",
                "Claude settings are not valid JSON",
            )
        })?
    } else {
        Value::Object(Map::new())
    };
    if !root.is_object() {
        return Err(HookError::data(
            "provider-config-invalid",
            "provider config root must be an object",
        ));
    }
    let (owned_before, legacy_before, unrelated_before, drifted) =
        inspect_json_handlers(&root, product, &groups, loaded)?;
    remove_owned_and_legacy_json(&mut root, product, loaded)?;
    if action != SetupAction::Remove {
        for group in &groups {
            append_json_group(&mut root, product, group)?;
        }
    }
    let mut candidate = if root.as_object().is_some_and(Map::is_empty) {
        None
    } else {
        Some(serde_json::to_vec_pretty(&root).map_err(|_| {
            HookError::runtime(
                "provider-config-render-failed",
                "provider config render failed",
            )
        })?)
    };
    if action == SetupAction::Remove && owned_before == 0 && legacy_before == 0 {
        candidate = original.clone();
    }
    validate_candidate_size(candidate.as_deref())?;
    let original_mode = file_mode(&path)?;
    Ok(Plan {
        product,
        primary_path: path.clone(),
        files: vec![PlannedFile {
            path,
            original,
            candidate,
            original_mode,
        }],
        groups: groups.clone(),
        owned_before,
        owned_after: if action == SetupAction::Remove {
            0
        } else {
            groups.len()
        },
        legacy_before,
        unrelated_before,
        drifted,
        auxiliary_configured_before: true,
        auxiliary_configured_after: true,
    })
}

fn validate_candidate_size(candidate: Option<&[u8]>) -> Result<(), HookError> {
    if candidate.is_some_and(|bytes| bytes.len() > MAX_PROVIDER_CONFIG_BYTES) {
        return Err(HookError::data(
            "provider-config-candidate-too-large",
            "generated provider config exceeds the 1 MiB read limit",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TomlMultilineString {
    None,
    Basic,
    Literal,
}

fn toml_multiline_value_line_starts(raw: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut state = TomlMultilineString::None;
    let mut offset = 0_usize;
    for line in raw.split_inclusive('\n') {
        if state != TomlMultilineString::None {
            starts.push(offset);
        }
        let bytes = line.as_bytes();
        let mut index = 0_usize;
        while index < bytes.len() {
            match state {
                TomlMultilineString::None => match bytes[index] {
                    b'#' => break,
                    b'"' if bytes[index..].starts_with(b"\"\"\"") => {
                        state = TomlMultilineString::Basic;
                        index += 3;
                    }
                    b'\'' if bytes[index..].starts_with(b"'''") => {
                        state = TomlMultilineString::Literal;
                        index += 3;
                    }
                    b'"' => {
                        index += 1;
                        while index < bytes.len() {
                            match bytes[index] {
                                b'\\' => index = (index + 2).min(bytes.len()),
                                b'"' => {
                                    index += 1;
                                    break;
                                }
                                _ => index += 1,
                            }
                        }
                    }
                    b'\'' => {
                        index += 1;
                        while index < bytes.len() && bytes[index] != b'\'' {
                            index += 1;
                        }
                        index = (index + 1).min(bytes.len());
                    }
                    _ => index += 1,
                },
                TomlMultilineString::Basic => {
                    if bytes[index..].starts_with(b"\"\"\"") {
                        state = TomlMultilineString::None;
                        index += 3;
                    } else if bytes[index] == b'\\' {
                        index = (index + 2).min(bytes.len());
                    } else {
                        index += 1;
                    }
                }
                TomlMultilineString::Literal => {
                    if bytes[index..].starts_with(b"'''") {
                        state = TomlMultilineString::None;
                        index += 3;
                    } else {
                        index += 1;
                    }
                }
            }
        }
        offset += line.len();
    }
    starts
}

fn toml_marker_line_ranges(
    raw: &str,
    marker: &str,
    multiline_value_lines: &[usize],
) -> Vec<(usize, usize)> {
    let mut offset = 0_usize;
    raw.split_inclusive('\n')
        .filter_map(|line| {
            let start = offset;
            offset += line.len();
            let without_newline = line.strip_suffix('\n').unwrap_or(line);
            let content = without_newline
                .strip_suffix('\r')
                .unwrap_or(without_newline);
            (content == marker && multiline_value_lines.binary_search(&start).is_err())
                .then_some((start, offset))
        })
        .collect()
}

fn collect_explicit_toml_table_paths(
    table: &toml_edit::Table,
    path: &mut Vec<String>,
    paths: &mut Vec<Vec<String>>,
) {
    for (key, item) in table.iter() {
        path.push(key.to_string());
        if let Some(child) = item.as_table() {
            if !child.is_implicit() {
                paths.push(path.clone());
            }
            collect_explicit_toml_table_paths(child, path, paths);
        } else if let Some(children) = item.as_array_of_tables() {
            for child in children.iter() {
                paths.push(path.clone());
                collect_explicit_toml_table_paths(child, path, paths);
            }
        }
        path.pop();
    }
}

fn toml_table_header_path(content: &str) -> Option<Vec<String>> {
    if !content.trim_start().starts_with('[') {
        return None;
    }
    let document = format!("{content}\n").parse::<DocumentMut>().ok()?;
    let mut paths = Vec::new();
    collect_explicit_toml_table_paths(document.as_table(), &mut Vec::new(), &mut paths);
    (paths.len() == 1).then(|| paths.remove(0))
}

fn is_codex_trust_table_path(path: &[String]) -> bool {
    path.first().is_some_and(|key| key == "projects")
        || (path.first().is_some_and(|key| key == "hooks")
            && path.get(1).is_some_and(|key| key == "state"))
}

fn is_byte_preserving_codex_trust_table_header(content: &str) -> bool {
    let content = content.trim_start();
    content == "[projects]"
        || content.starts_with("[projects.")
        || content == "[hooks.state]"
        || content.starts_with("[hooks.state.")
}

#[derive(Clone, Copy)]
struct RuntimeKitMarkerRange {
    start_end: usize,
    end_begin: usize,
    end_end: usize,
}

fn runtime_kit_marker_range(
    raw: &str,
    multiline_value_lines: &[usize],
) -> Result<Option<RuntimeKitMarkerRange>, HookError> {
    let starts = toml_marker_line_ranges(raw, RUNTIME_KIT_BLOCK_START, multiline_value_lines);
    let ends = toml_marker_line_ranges(raw, RUNTIME_KIT_BLOCK_END, multiline_value_lines);
    if starts.is_empty() && ends.is_empty() {
        return Ok(None);
    }
    if starts.len() != 1 || ends.len() != 1 {
        return Err(HookError::data(
            "provider-config-invalid",
            "Codex config has an ambiguous agent-runtime-kit hook marker layout",
        ));
    }
    let (start_begin, start_end) = starts[0];
    let (end_begin, end_end) = ends[0];
    if end_begin < start_begin {
        return Err(HookError::data(
            "provider-config-invalid",
            "Codex config has a reversed agent-runtime-kit hook marker block",
        ));
    }
    Ok(Some(RuntimeKitMarkerRange {
        start_end,
        end_begin,
        end_end,
    }))
}

fn repair_codex_trust_boundary(raw: &str) -> Result<(String, bool), HookError> {
    let multiline_value_lines = toml_multiline_value_line_starts(raw);
    let Some(RuntimeKitMarkerRange {
        start_end,
        end_begin,
        end_end,
    }) = runtime_kit_marker_range(raw, &multiline_value_lines)?
    else {
        return Ok((raw.to_string(), false));
    };

    let mut offset = start_end;
    let mut blank_run_start = None;
    let mut trust_boundary = None;
    for line in raw[start_end..end_begin].split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        if multiline_value_lines.binary_search(&line_start).is_ok() {
            blank_run_start = None;
            continue;
        }
        let without_newline = line.strip_suffix('\n').unwrap_or(line);
        let content = without_newline
            .strip_suffix('\r')
            .unwrap_or(without_newline);
        if content.trim().is_empty() {
            blank_run_start.get_or_insert(line_start);
        } else if let Some(table_path) = toml_table_header_path(content) {
            if is_codex_trust_table_path(&table_path) {
                if !is_byte_preserving_codex_trust_table_header(content) {
                    return Err(HookError::data(
                        "provider-config-invalid",
                        "Codex trust table syntax inside the agent-runtime-kit hook marker block cannot be migrated byte-for-byte",
                    ));
                }
                trust_boundary.get_or_insert(blank_run_start.unwrap_or(line_start));
            } else if trust_boundary.is_some() {
                return Err(HookError::data(
                    "provider-config-invalid",
                    "Codex trust tables are followed by non-trust TOML inside the agent-runtime-kit hook marker block",
                ));
            }
            blank_run_start = None;
        } else {
            blank_run_start = None;
        }
    }
    let Some(trust_boundary) = trust_boundary else {
        return Ok((raw.to_string(), false));
    };

    let close_line = &raw[end_begin..end_end];
    let moved_suffix = &raw[trust_boundary..end_begin];
    let (moved_suffix, separator) = if close_line.ends_with('\n') {
        (moved_suffix, "")
    } else if let Some(suffix) = moved_suffix.strip_suffix("\r\n") {
        (suffix, "\r\n")
    } else if let Some(suffix) = moved_suffix.strip_suffix('\n') {
        (suffix, "\n")
    } else {
        return Err(HookError::data(
            "provider-config-invalid",
            "Codex trust tables and the agent-runtime-kit closing marker do not have a reusable line separator",
        ));
    };
    let mut repaired = String::with_capacity(raw.len());
    repaired.push_str(&raw[..trust_boundary]);
    repaired.push_str(close_line);
    repaired.push_str(separator);
    repaired.push_str(moved_suffix);
    repaired.push_str(&raw[end_end..]);
    Ok((repaired, true))
}

fn render_codex_block(groups: &[HookGroup], product: Product) -> String {
    let command = TomlValue::from(dispatch_command(product));
    let mut block = format!("{CODEX_BLOCK_START}\n");
    for group in groups {
        block.push_str(&format!("[[hooks.{}]]\n", group.event));
        if let Some(matcher) = group.matcher.as_deref() {
            block.push_str(&format!("matcher = {}\n", TomlValue::from(matcher)));
        }
        block.push_str(&format!(
            "\n[[hooks.{event}.hooks]]\ntype = \"command\"\ncommand = {command}\ntimeout = {DISPATCH_TIMEOUT_SECONDS}\n\n",
            event = group.event,
        ));
    }
    block.push_str(CODEX_BLOCK_END);
    block.push('\n');
    block
}

fn managed_marker_owner(content: &str, prefix: &str, suffix: &str) -> Option<String> {
    content
        .strip_prefix(prefix)
        .and_then(|value| value.strip_suffix(suffix))
        .filter(|owner| !owner.is_empty())
        .map(str::to_string)
}

enum ForeignManagedMarker {
    Start { owner: String, begin: usize },
    End { owner: String, end: usize },
}

fn codex_owned_block_overlaps_foreign_manager(
    raw: &str,
    owned_begin: usize,
    owned_end: usize,
    multiline_value_lines: &[usize],
) -> Result<bool, HookError> {
    let owned_owner = "agent-hook:provider-ingress:v1";
    let mut markers = Vec::new();
    let mut offset = 0_usize;
    for line in raw.split_inclusive('\n') {
        let line_begin = offset;
        offset += line.len();
        if multiline_value_lines.binary_search(&line_begin).is_ok() {
            continue;
        }
        let without_newline = line.strip_suffix('\n').unwrap_or(line);
        let content = without_newline
            .strip_suffix('\r')
            .unwrap_or(without_newline);
        if let Some(owner) = managed_marker_owner(content, "# >>> ", " >>>") {
            if owner != owned_owner {
                markers.push(ForeignManagedMarker::Start {
                    owner,
                    begin: line_begin,
                });
            }
            continue;
        }
        let Some(owner) = managed_marker_owner(content, "# <<< ", " <<<") else {
            continue;
        };
        if owner == owned_owner {
            continue;
        }
        markers.push(ForeignManagedMarker::End { owner, end: offset });
    }

    let mut open = Vec::<(String, usize)>::new();
    let mut ranges = Vec::new();
    let mut malformed = false;
    for marker in markers {
        match marker {
            ForeignManagedMarker::Start { owner, begin } => {
                if open.iter().any(|(candidate, _)| candidate == &owner) {
                    malformed = true;
                }
                open.push((owner, begin));
            }
            ForeignManagedMarker::End { owner, end } => {
                let Some(index) = open.iter().rposition(|(candidate, _)| candidate == &owner)
                else {
                    malformed = true;
                    continue;
                };
                if index + 1 != open.len() {
                    malformed = true;
                }
                let (_, begin) = open.remove(index);
                ranges.push(begin..end);
            }
        }
    }
    malformed |= !open.is_empty();
    if malformed {
        return Err(HookError::data(
            "provider-config-invalid",
            "Codex config has an ambiguous foreign managed marker layout",
        ));
    }

    let mut owned_is_inside_foreign = false;
    for foreign in ranges {
        if foreign.start >= owned_end || owned_begin >= foreign.end {
            continue;
        }
        if foreign.start <= owned_begin && owned_end <= foreign.end {
            owned_is_inside_foreign = true;
            continue;
        }
        return Err(HookError::data(
            "provider-config-invalid",
            "Codex agent-hook ingress contains or crosses foreign managed bytes that cannot be regenerated safely",
        ));
    }
    Ok(owned_is_inside_foreign)
}

fn strip_codex_block(raw: &str, expected: &str) -> Result<(String, usize, bool), HookError> {
    let multiline_value_lines = toml_multiline_value_line_starts(raw);
    let starts = toml_marker_line_ranges(raw, CODEX_BLOCK_START, &multiline_value_lines);
    let ends = toml_marker_line_ranges(raw, CODEX_BLOCK_END, &multiline_value_lines);
    if starts.is_empty() && ends.is_empty() {
        return Ok((raw.to_string(), 0, false));
    }
    if starts.len() != 1 || ends.len() != 1 || starts[0].0 >= ends[0].0 {
        return Err(HookError::data(
            "provider-owned-marker-invalid",
            "agent-hook Codex ownership markers are incomplete or duplicated",
        ));
    }
    let begin = starts[0].0;
    let end = ends[0].1;
    let block = &raw[begin..end];
    let overlaps_foreign =
        codex_owned_block_overlaps_foreign_manager(raw, begin, end, &multiline_value_lines)?;
    let drifted = block != expected || overlaps_foreign;
    let owned = block.matches("command = \"agent-hook dispatch").count();
    let mut stripped = String::with_capacity(raw.len() - (end - begin));
    stripped.push_str(&raw[..begin]);
    stripped.push_str(&raw[end..]);
    Ok((stripped, owned, drifted))
}

fn inspect_toml_handlers(document: &DocumentMut) -> (usize, usize) {
    let mut compatibility = 0;
    let mut unrelated = 0;
    let Some(hooks) = document.get("hooks").and_then(TomlItem::as_table) else {
        return (0, 0);
    };
    for (_, item) in hooks.iter() {
        let Some(groups) = item.as_array_of_tables() else {
            continue;
        };
        for group in groups {
            let Some(handlers) = group.get("hooks").and_then(TomlItem::as_array_of_tables) else {
                continue;
            };
            for handler in handlers {
                if legacy_toml_handler(handler) {
                    compatibility += 1;
                } else {
                    unrelated += 1;
                }
            }
        }
    }
    (compatibility, unrelated)
}

fn remove_legacy_toml_handlers(document: &mut DocumentMut) {
    let Some(hooks) = document.get_mut("hooks").and_then(TomlItem::as_table_mut) else {
        return;
    };
    let events = hooks
        .iter()
        .map(|(event, _)| event.to_string())
        .collect::<Vec<_>>();
    for event in events {
        let mut remove_event = false;
        if let Some(groups) = hooks
            .get_mut(&event)
            .and_then(TomlItem::as_array_of_tables_mut)
        {
            for group in groups.iter_mut() {
                if let Some(handlers) = group
                    .get_mut("hooks")
                    .and_then(TomlItem::as_array_of_tables_mut)
                {
                    handlers.retain(|handler| !legacy_toml_handler(handler));
                }
            }
            groups.retain(|group| {
                toml_group_has_user_metadata(group)
                    || group
                        .get("hooks")
                        .and_then(TomlItem::as_array_of_tables)
                        .is_none_or(|handlers| !handlers.is_empty())
            });
            remove_event = groups.is_empty();
        }
        if remove_event {
            hooks.remove(&event);
        }
    }
    if hooks.is_empty() {
        document.remove("hooks");
    }
}

fn inspect_json_handlers(
    root: &Value,
    product: Product,
    expected: &[HookGroup],
    loaded: &LoadedPolicy,
) -> Result<(usize, usize, usize, bool), HookError> {
    let Some(hooks) = root.get("hooks") else {
        return Ok((0, 0, 0, false));
    };
    let hooks = hooks.as_object().ok_or_else(|| {
        HookError::data(
            "provider-config-invalid",
            "provider hooks must be an object",
        )
    })?;
    let mut owned = 0;
    let mut compatibility = 0;
    let mut unrelated = 0;
    let mut owned_groups = BTreeSet::new();
    for (event, groups) in hooks {
        let groups = groups.as_array().ok_or_else(|| {
            HookError::data(
                "provider-config-invalid",
                "provider event hooks must be an array",
            )
        })?;
        for group in groups {
            let matcher = group
                .get("matcher")
                .and_then(Value::as_str)
                .map(str::to_string);
            let handlers = group
                .get("hooks")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    HookError::data("provider-config-invalid", "provider hook group is invalid")
                })?;
            for handler in handlers {
                if owned_json_handler(handler, product) {
                    owned += 1;
                    owned_groups.insert((event.clone(), matcher.clone()));
                } else if legacy_json_handler(handler, product, loaded) {
                    compatibility += 1;
                } else {
                    unrelated += 1;
                }
            }
        }
    }
    let expected_set = expected
        .iter()
        .map(|group| (group.event.clone(), group.matcher.clone()))
        .collect::<BTreeSet<_>>();
    let drifted = owned > 0 && (owned != expected.len() || owned_groups != expected_set);
    Ok((owned, compatibility, unrelated, drifted))
}

fn remove_owned_and_legacy_json(
    root: &mut Value,
    product: Product,
    loaded: &LoadedPolicy,
) -> Result<(), HookError> {
    let Some(hooks) = root.get_mut("hooks") else {
        return Ok(());
    };
    let hooks = hooks.as_object_mut().ok_or_else(|| {
        HookError::data(
            "provider-config-invalid",
            "provider hooks must be an object",
        )
    })?;
    let events = hooks.keys().cloned().collect::<Vec<_>>();
    for event in events {
        let groups = hooks
            .get_mut(&event)
            .and_then(Value::as_array_mut)
            .ok_or_else(|| {
                HookError::data(
                    "provider-config-invalid",
                    "provider event hooks must be an array",
                )
            })?;
        for group in groups.iter_mut() {
            let handlers = group
                .get_mut("hooks")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| {
                    HookError::data("provider-config-invalid", "provider hook group is invalid")
                })?;
            handlers.retain(|handler| {
                !owned_json_handler(handler, product)
                    && !legacy_json_handler(handler, product, loaded)
            });
        }
        groups.retain(|group| {
            json_group_has_user_metadata(group)
                || group
                    .get("hooks")
                    .and_then(Value::as_array)
                    .is_some_and(|handlers| !handlers.is_empty())
        });
        if groups.is_empty() {
            hooks.remove(&event);
        }
    }
    if hooks.is_empty() {
        root.as_object_mut()
            .expect("validated object")
            .remove("hooks");
    }
    Ok(())
}

fn append_json_group(
    root: &mut Value,
    product: Product,
    group: &HookGroup,
) -> Result<(), HookError> {
    let root = root.as_object_mut().expect("validated object");
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks.as_object_mut().ok_or_else(|| {
        HookError::data(
            "provider-config-invalid",
            "provider hooks must be an object",
        )
    })?;
    let groups = hooks
        .entry(group.event.clone())
        .or_insert_with(|| json!([]));
    let groups = groups.as_array_mut().ok_or_else(|| {
        HookError::data(
            "provider-config-invalid",
            "provider event hooks must be an array",
        )
    })?;
    let mut object = Map::new();
    if let Some(matcher) = group.matcher.as_deref() {
        object.insert("matcher".to_string(), Value::String(matcher.to_string()));
    }
    object.insert(
        "hooks".to_string(),
        json!([{
            "type": "command",
            "command": dispatch_command(product),
            "timeout": DISPATCH_TIMEOUT_SECONDS,
        }]),
    );
    groups.push(Value::Object(object));
    Ok(())
}

fn owned_json_handler(handler: &Value, product: Product) -> bool {
    handler.as_object().is_some_and(|object| object.len() == 3)
        && handler.get("type").and_then(Value::as_str) == Some("command")
        && handler.get("command").and_then(Value::as_str)
            == Some(dispatch_command(product).as_str())
        && handler.get("timeout").and_then(Value::as_i64) == Some(DISPATCH_TIMEOUT_SECONDS)
}

fn legacy_json_handler(handler: &Value, product: Product, loaded: &LoadedPolicy) -> bool {
    handler.as_object().is_some_and(|object| object.len() == 3)
        && handler.get("type").and_then(Value::as_str) == Some("command")
        && handler.get("timeout").and_then(Value::as_i64) == Some(5)
        && handler
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| legacy_command_for_policy(command, product, loaded))
}

fn legacy_toml_handler(handler: &toml_edit::Table) -> bool {
    handler.len() == 3
        && handler.get("type").and_then(TomlItem::as_str) == Some("command")
        && handler.get("timeout").and_then(TomlItem::as_integer) == Some(5)
        && handler
            .get("command")
            .and_then(TomlItem::as_str)
            .is_some_and(|command| legacy_command(command, Product::Codex))
}

fn json_group_has_user_metadata(group: &Value) -> bool {
    group.as_object().is_some_and(|object| {
        object
            .keys()
            .any(|key| !matches!(key.as_str(), "matcher" | "hooks"))
    })
}

fn toml_group_has_user_metadata(group: &toml_edit::Table) -> bool {
    group
        .iter()
        .any(|(key, _)| !matches!(key, "matcher" | "hooks"))
}

fn legacy_command(command: &str, product: Product) -> bool {
    command == format!("agent-session activity hook --agent {}", product.as_str())
        || [
            "agent-scope-lock-guard",
            "block-claude-coauthor-trailer",
            "block-direct-git-commit",
            "block-direct-git-worktree",
            "block-direct-pr-create",
            "block-direct-python",
            "block-project-memory-write",
            "block-unsafe-default-delivery",
            "checkout-lease-guard",
            "finish-line-record",
            "forge-label-reminder",
            "mcp-secret-scan",
            "memory-write-principle-reminder",
            "portable-paths-scan",
            "pre-edit-intent-gate",
            "semantic-commit-body-gate",
            "session-start-healthcheck",
            "skill-usage-reminder",
            "stop-finish-line-gate",
            "stop-pre-pr-reminder",
            "user-prompt-agent-docs",
            "user-prompt-agent-memory",
        ]
        .iter()
        .any(|id| {
            runtime_handler_filename(id)
                .is_some_and(|filename| exact_runtime_handler_command(command, product, filename))
        })
}

fn legacy_command_for_policy(command: &str, product: Product, loaded: &LoadedPolicy) -> bool {
    if command == format!("agent-session activity hook --agent {}", product.as_str()) {
        return true;
    }
    loaded.bundle.rules.iter().any(|rule| {
        matches!(
            &rule.capability,
            Capability::RuntimeKitHandler { handler_id }
                if runtime_handler_filename(handler_id)
                    .is_some_and(|filename| exact_runtime_handler_command(command, product, filename))
        )
    })
}

fn exact_runtime_handler_command(command: &str, product: Product, filename: &str) -> bool {
    match product {
        Product::Codex => {
            command
                == format!(
                    "AGENT_RUNTIME_PRODUCT=codex \"${{CODEX_HOME:-$HOME/.codex}}/hooks/{filename}\""
                )
        }
        Product::Claude => {
            command == format!("$HOME/.claude/hooks/{filename}")
                || command
                    == format!("AGENT_RUNTIME_PRODUCT=claude \"$HOME/.claude/hooks/{filename}\"")
        }
        Product::Hermes => false,
    }
}

fn provider_path(product: Product) -> Result<PathBuf, HookError> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| HookError::runtime("home-unavailable", "HOME is required for setup"))?;
    Ok(match product {
        Product::Codex => std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(".codex"))
            .join("config.toml"),
        Product::Claude => home.join(".claude/settings.json"),
        Product::Hermes => home.join(".hermes/config.yaml"),
    })
}

fn read_optional_config(path: &Path) -> Result<Option<Vec<u8>>, HookError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(HookError::runtime(
                "provider-config-unavailable",
                "provider config metadata is unavailable",
            ));
        }
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o022 != 0
        || metadata.len() > 1024 * 1024
    {
        return Err(HookError::data(
            "provider-config-untrusted",
            "provider config type, owner, mode, or size is untrusted",
        ));
    }
    fs::read(path).map(Some).map_err(|_| {
        HookError::runtime("provider-config-read-failed", "provider config read failed")
    })
}

fn file_mode(path: &Path) -> Result<Option<u32>, HookError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata.permissions().mode() & 0o777)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(HookError::runtime(
            "provider-config-unavailable",
            "provider config metadata is unavailable",
        )),
    }
}

fn apply_plan(layout: &Layout, plan: &Plan) -> Result<(), HookError> {
    let _state_lock = setup_lock(layout)?;
    let _provider_lock = provider_lock(&plan.primary_path)?;
    for file in &plan.files {
        let current = read_optional_config(&file.path)?;
        if current != file.original {
            return Err(HookError::unavailable(
                "provider-config-drift",
                "provider config changed after preview",
            ));
        }
    }
    apply_transaction(&plan.files, apply_candidate)
}

fn apply_transaction(
    files: &[PlannedFile],
    mut apply: impl FnMut(&PlannedFile) -> Result<(), HookError>,
) -> Result<(), HookError> {
    for (index, file) in files.iter().enumerate() {
        if file.original == file.candidate {
            continue;
        }
        if let Err(error) = apply(file) {
            if rollback_files(&files[..=index]).is_err() {
                return Err(HookError::runtime(
                    "provider-config-rollback-failed",
                    "provider config transaction failed and exact rollback was not possible",
                ));
            }
            return Err(error);
        }
    }
    Ok(())
}

fn apply_candidate(file: &PlannedFile) -> Result<(), HookError> {
    write_file_state(&file.path, file.candidate.as_deref(), file.original_mode)
}

fn rollback_files(files: &[PlannedFile]) -> Result<(), HookError> {
    for file in files.iter().rev() {
        write_file_state(&file.path, file.original.as_deref(), file.original_mode)?;
    }
    Ok(())
}

fn write_file_state(
    path: &Path,
    contents: Option<&[u8]>,
    original_mode: Option<u32>,
) -> Result<(), HookError> {
    if let Some(contents) = contents {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|_| {
                HookError::runtime(
                    "provider-config-dir-failed",
                    "provider config directory create failed",
                )
            })?;
        }
        write_atomic(path, contents, original_mode.unwrap_or(0o600)).map_err(|_| {
            HookError::runtime(
                "provider-config-write-failed",
                "provider config atomic write failed",
            )
        })?;
    } else if path.exists() {
        fs::remove_file(path).map_err(|_| {
            HookError::runtime(
                "provider-config-remove-failed",
                "provider config remove failed",
            )
        })?;
    }
    Ok(())
}

fn plan_digest(plan: &Plan) -> Result<String, HookError> {
    let files = plan
        .files
        .iter()
        .map(|file| {
            json!({
                "path_role": if file.path == plan.primary_path { "provider-primary" } else { "provider-compatibility" },
                "before": file.original.as_deref().map(digest),
                "after": file.candidate.as_deref().map(digest),
            })
        })
        .collect::<Vec<_>>();
    digest_serializable(&json!({
        "schema_version": "agent-hook.setup-plan.v2",
        "product": plan.product,
        "files": files,
        "groups": plan.groups,
        "legacy_count": plan.legacy_before,
        "drifted": plan.drifted,
    }))
}

fn dispatch_command(product: Product) -> String {
    format!("agent-hook dispatch --product {}", product.as_str())
}

fn distinct_events(groups: &[HookGroup]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    groups
        .iter()
        .filter_map(|group| {
            seen.insert(group.event.clone())
                .then_some(group.event.clone())
        })
        .collect()
}

fn classify_status(
    owned: usize,
    expected: usize,
    compatibility: usize,
    unrelated: usize,
    drifted: bool,
) -> ProviderStatus {
    if drifted {
        ProviderStatus::Drifted
    } else if owned > 0 && compatibility > 0 {
        ProviderStatus::Dual
    } else if compatibility > 0 {
        ProviderStatus::CompatibilityOnly
    } else if owned == 0 {
        if unrelated > 0 {
            ProviderStatus::Unrelated
        } else {
            ProviderStatus::Missing
        }
    } else if owned == expected {
        ProviderStatus::Converged
    } else {
        ProviderStatus::Drifted
    }
}

#[derive(Debug)]
struct SetupLock(File);

impl Drop for SetupLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn setup_lock(layout: &Layout) -> Result<SetupLock, HookError> {
    crate::paths::ensure_private_state_dir(&layout.state_root, "setup-state-dir")?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(layout.state_root.join("setup.lock"))
        .map_err(|_| HookError::runtime("setup-lock-unavailable", "setup lock unavailable"))?;
    let started = Instant::now();
    loop {
        let status = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if status == 0 {
            return Ok(SetupLock(file));
        }
        if started.elapsed() >= LOCK_TIMEOUT {
            return Err(HookError::unavailable(
                "setup-lock-timeout",
                "provider setup is busy",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn provider_lock(primary_path: &Path) -> Result<SetupLock, HookError> {
    let parent = primary_path.parent().ok_or_else(|| {
        HookError::runtime(
            "setup-lock-unavailable",
            "provider setup lock parent is unavailable",
        )
    })?;
    fs::create_dir_all(parent).map_err(|_| {
        HookError::runtime(
            "setup-lock-unavailable",
            "provider setup lock directory is unavailable",
        )
    })?;
    acquire_lock(parent.join(".agent-hook-setup.lock"))
}

fn acquire_lock(path: PathBuf) -> Result<SetupLock, HookError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| HookError::runtime("setup-lock-unavailable", "setup lock unavailable"))?;
    let started = Instant::now();
    loop {
        let status = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if status == 0 {
            return Ok(SetupLock(file));
        }
        if started.elapsed() >= LOCK_TIMEOUT {
            return Err(HookError::unavailable(
                "setup-lock-timeout",
                "provider setup is busy",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn runtime_kit_trust_boundary_reuses_eof_newlines_byte_exactly() {
        for newline in ["\n", "\r\n"] {
            let prefix = format!("{RUNTIME_KIT_BLOCK_START}{newline}[[hooks.PreToolUse]]{newline}");
            let trust = format!(
                "[projects.\"/foreign/project\"]{newline}trust_level = \"trusted\"{newline}"
            );
            let trust_at_eof = trust.strip_suffix(newline).expect("trust newline");
            let raw = format!("{prefix}{newline}{trust}{RUNTIME_KIT_BLOCK_END}");
            let expected =
                format!("{prefix}{RUNTIME_KIT_BLOCK_END}{newline}{newline}{trust_at_eof}");
            assert_eq!(
                repair_codex_trust_boundary(&raw).expect("byte-exact EOF repair"),
                (expected, true)
            );

            let no_separator = format!("{prefix}{trust}{RUNTIME_KIT_BLOCK_END}");
            assert_eq!(
                repair_codex_trust_boundary(&no_separator)
                    .expect("EOF repair reuses the trailing trust separator"),
                (
                    format!("{prefix}{RUNTIME_KIT_BLOCK_END}{newline}{trust_at_eof}"),
                    true
                )
            );
        }
    }

    #[test]
    fn runtime_kit_marker_lines_inside_multiline_values_are_not_boundaries() {
        let raw = format!(
            "prompt = \"\"\"\n{RUNTIME_KIT_BLOCK_START}\n[projects.\\\"/foreign/project\\\"]\n{RUNTIME_KIT_BLOCK_END}\n\"\"\"\n"
        );

        assert_eq!(
            repair_codex_trust_boundary(&raw).expect("multiline marker text is unrelated"),
            (raw, false)
        );
    }

    #[test]
    fn apply_rejects_a_concurrent_provider_change_without_overwriting_it() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let root = fs::canonicalize(temp.path()).expect("canonical tempdir");
        let path = root.join("provider.json");
        fs::write(&path, b"reviewed").expect("reviewed config");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("private mode");
        let layout = Layout {
            config_path: root.join("config.toml"),
            state_root: root.join("state"),
        };
        let plan = Plan {
            product: Product::Claude,
            primary_path: path.clone(),
            files: vec![PlannedFile {
                path: path.clone(),
                original: Some(b"reviewed".to_vec()),
                candidate: Some(b"candidate".to_vec()),
                original_mode: Some(0o600),
            }],
            groups: Vec::new(),
            owned_before: 0,
            owned_after: 0,
            legacy_before: 0,
            unrelated_before: 0,
            drifted: false,
            auxiliary_configured_before: true,
            auxiliary_configured_after: true,
        };
        fs::write(&path, b"concurrent").expect("concurrent config");

        let error = apply_plan(&layout, &plan).expect_err("concurrent change");

        assert_eq!(error.code, "provider-config-drift");
        assert_eq!(fs::read(&path).expect("preserved config"), b"concurrent");
    }

    #[test]
    fn transaction_rolls_back_every_file_when_second_replace_reports_failure() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let first = temp.path().join("config.toml");
        let second = temp.path().join("hooks.json");
        fs::write(&first, b"first-before").expect("first before");
        fs::write(&second, b"second-before").expect("second before");
        let files = vec![
            PlannedFile {
                path: first.clone(),
                original: Some(b"first-before".to_vec()),
                candidate: Some(b"first-after".to_vec()),
                original_mode: Some(0o600),
            },
            PlannedFile {
                path: second.clone(),
                original: Some(b"second-before".to_vec()),
                candidate: Some(b"second-after".to_vec()),
                original_mode: Some(0o600),
            },
        ];
        let mut writes = 0;

        let error = apply_transaction(&files, |file| {
            writes += 1;
            fs::write(&file.path, file.candidate.as_deref().expect("candidate"))
                .expect("simulated replace");
            if writes == 2 {
                Err(HookError::runtime(
                    "provider-config-write-failed",
                    "simulated post-replace failure",
                ))
            } else {
                Ok(())
            }
        })
        .expect_err("second write failure");

        assert_eq!(error.code, "provider-config-write-failed");
        assert_eq!(fs::read(&first).expect("first restored"), b"first-before");
        assert_eq!(
            fs::read(&second).expect("second restored"),
            b"second-before"
        );
    }

    #[test]
    fn provider_lock_identity_is_independent_of_state_root() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let provider = temp.path().join("config.toml");
        let first = provider_lock(&provider).expect("first provider lock");

        let error = provider_lock(&provider).expect_err("same provider lock must serialize");

        assert_eq!(error.code, "setup-lock-timeout");
        drop(first);
        provider_lock(&provider).expect("provider lock released");
    }
}
