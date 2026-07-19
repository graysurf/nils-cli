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

const CODEX_BLOCK_START: &str = "# >>> agent-hook:provider-ingress:v1 >>>";
const CODEX_BLOCK_END: &str = "# <<< agent-hook:provider-ingress:v1 <<<";
const DISPATCH_TIMEOUT_SECONDS: i64 = 10;
const LOCK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HookGroup {
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SetupResult {
    pub schema_version: String,
    pub product: String,
    pub action: String,
    pub status: String,
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

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DoctorResult {
    pub schema_version: String,
    pub product: String,
    pub status: String,
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
    path: PathBuf,
    original: Option<Vec<u8>>,
    candidate: Option<Vec<u8>>,
    groups: Vec<HookGroup>,
    owned_before: usize,
    owned_after: usize,
    legacy_before: usize,
    unrelated_before: usize,
    drifted: bool,
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
            status: "unsupported".to_string(),
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
            "legacy or drifted provider state requires the exact reviewed plan digest",
        ));
    }
    if expected_plan_digest.is_some_and(|expected| expected != plan_digest) {
        return Err(HookError::data(
            "setup-plan-digest-mismatch",
            "provider setup plan changed after review",
        ));
    }
    let would_change = plan.original != plan.candidate;
    let mut changed = false;
    if !matches!(action, SetupAction::DryRun) && would_change {
        apply_plan(layout, &plan)?;
        changed = true;
    }
    let configured = if matches!(action, SetupAction::DryRun) {
        plan.owned_before == plan.groups.len() && !plan.drifted && plan.legacy_before == 0
    } else {
        plan.owned_after == plan.groups.len() && action != SetupAction::Remove
    };
    let would_configure = action != SetupAction::Remove && plan.owned_after == plan.groups.len();
    let status = classify_status(
        plan.owned_before,
        plan.groups.len(),
        plan.legacy_before,
        plan.drifted,
    );
    Ok(SetupResult {
        schema_version: "agent-hook.setup-result.v1".to_string(),
        product: product.as_str().to_string(),
        action: action.as_str().to_string(),
        status: status.to_string(),
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
            status: "unsupported".to_string(),
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
            plan.drifted,
        )
        .to_string(),
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
        Product::Codex => build_codex_plan(product, action, path, original, groups),
        Product::Claude => build_json_plan(product, action, path, original, groups, loaded),
        Product::Hermes => unreachable!("unsupported returned before plan"),
    }
}

fn build_codex_plan(
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
    raw.parse::<DocumentMut>().map_err(|_| {
        HookError::data("provider-config-invalid", "Codex config is not valid TOML")
    })?;
    let expected_block = render_codex_block(&groups, product);
    let (stripped, owned_before, drifted) = strip_codex_block(raw, &expected_block)?;
    let mut document = stripped.parse::<DocumentMut>().map_err(|_| {
        HookError::data("provider-config-invalid", "Codex config is not valid TOML")
    })?;
    let (legacy_before, unrelated_before) = inspect_toml_handlers(&document);
    remove_legacy_toml_handlers(&mut document);
    let mut rendered = document.to_string();
    if action != SetupAction::Remove {
        if !rendered.is_empty() && !rendered.ends_with('\n') {
            rendered.push('\n');
        }
        if !rendered.is_empty() && !rendered.ends_with("\n\n") {
            rendered.push('\n');
        }
        rendered.push_str(&expected_block);
    }
    let candidate = if rendered.is_empty() {
        None
    } else {
        Some(rendered.into_bytes())
    };
    Ok(Plan {
        product,
        path,
        original,
        candidate,
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
    })
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
        serde_json::from_slice::<Value>(bytes).map_err(|_| {
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
    let candidate = if root.as_object().is_some_and(Map::is_empty) {
        None
    } else {
        Some(serde_json::to_vec_pretty(&root).map_err(|_| {
            HookError::runtime(
                "provider-config-render-failed",
                "provider config render failed",
            )
        })?)
    };
    Ok(Plan {
        product,
        path,
        original,
        candidate,
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
    })
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

fn strip_codex_block(raw: &str, expected: &str) -> Result<(String, usize, bool), HookError> {
    let starts = raw.match_indices(CODEX_BLOCK_START).collect::<Vec<_>>();
    let ends = raw.match_indices(CODEX_BLOCK_END).collect::<Vec<_>>();
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
    let mut end = ends[0].0 + CODEX_BLOCK_END.len();
    if raw.as_bytes().get(end) == Some(&b'\n') {
        end += 1;
    }
    let block = &raw[begin..end];
    let drifted = block != expected;
    let owned = block.matches("command = \"agent-hook dispatch").count();
    let mut stripped = String::with_capacity(raw.len() - (end - begin));
    stripped.push_str(&raw[..begin]);
    stripped.push_str(&raw[end..]);
    Ok((stripped, owned, drifted))
}

fn inspect_toml_handlers(document: &DocumentMut) -> (usize, usize) {
    let mut legacy = 0;
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
                    legacy += 1;
                } else {
                    unrelated += 1;
                }
            }
        }
    }
    (legacy, unrelated)
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
    let mut legacy = 0;
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
                } else if legacy_json_handler(handler, loaded) {
                    legacy += 1;
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
    let drifted = !owned_groups.is_empty() && owned_groups != expected_set;
    Ok((owned, legacy, unrelated, drifted))
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
                !owned_json_handler(handler, product) && !legacy_json_handler(handler, loaded)
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

fn legacy_json_handler(handler: &Value, loaded: &LoadedPolicy) -> bool {
    handler.as_object().is_some_and(|object| object.len() == 3)
        && handler.get("type").and_then(Value::as_str) == Some("command")
        && handler.get("timeout").and_then(Value::as_i64) == Some(5)
        && handler
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| legacy_command_for_policy(command, loaded))
}

fn legacy_toml_handler(handler: &toml_edit::Table) -> bool {
    handler.len() == 3
        && handler.get("type").and_then(TomlItem::as_str) == Some("command")
        && handler.get("timeout").and_then(TomlItem::as_integer) == Some(5)
        && handler
            .get("command")
            .and_then(TomlItem::as_str)
            .is_some_and(legacy_command)
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

fn legacy_command(command: &str) -> bool {
    command.contains("agent-session activity hook")
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
        .any(|id| command.contains(runtime_handler_filename(id).expect("compiled handler")))
}

fn legacy_command_for_policy(command: &str, loaded: &LoadedPolicy) -> bool {
    if command.contains("agent-session activity hook") {
        return true;
    }
    loaded.bundle.rules.iter().any(|rule| {
        matches!(
            &rule.capability,
            Capability::RuntimeKitHandler { handler_id }
                if runtime_handler_filename(handler_id).is_some_and(|name| command.contains(name))
        )
    })
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

fn apply_plan(layout: &Layout, plan: &Plan) -> Result<(), HookError> {
    let _lock = setup_lock(layout)?;
    let current = read_optional_config(&plan.path)?;
    if current != plan.original {
        return Err(HookError::unavailable(
            "provider-config-drift",
            "provider config changed after preview",
        ));
    }
    if let Some(candidate) = plan.candidate.as_deref() {
        if let Some(parent) = plan.path.parent() {
            fs::create_dir_all(parent).map_err(|_| {
                HookError::runtime(
                    "provider-config-dir-failed",
                    "provider config directory create failed",
                )
            })?;
        }
        let mode = plan
            .path
            .metadata()
            .map(|metadata| metadata.permissions().mode() & 0o777)
            .unwrap_or(0o600);
        write_atomic(&plan.path, candidate, mode).map_err(|_| {
            HookError::runtime(
                "provider-config-write-failed",
                "provider config atomic write failed",
            )
        })?;
    } else if plan.path.exists() {
        fs::remove_file(&plan.path).map_err(|_| {
            HookError::runtime(
                "provider-config-remove-failed",
                "provider config remove failed",
            )
        })?;
    }
    Ok(())
}

fn plan_digest(plan: &Plan) -> Result<String, HookError> {
    digest_serializable(&json!({
        "schema_version": "agent-hook.setup-plan.v1",
        "product": plan.product,
        "before": plan.original.as_deref().map(digest),
        "after": plan.candidate.as_deref().map(digest),
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

fn classify_status(owned: usize, expected: usize, legacy: usize, drifted: bool) -> &'static str {
    if drifted {
        "drifted"
    } else if owned > 0 && legacy > 0 {
        "dual"
    } else if legacy > 0 {
        "legacy"
    } else if owned == 0 {
        "missing"
    } else if owned == expected {
        "converged"
    } else {
        "drifted"
    }
}

struct SetupLock(File);

impl Drop for SetupLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn setup_lock(layout: &Layout) -> Result<SetupLock, HookError> {
    fs::create_dir_all(&layout.state_root).map_err(|_| {
        HookError::runtime(
            "setup-state-dir-failed",
            "setup state directory create failed",
        )
    })?;
    fs::set_permissions(&layout.state_root, fs::Permissions::from_mode(0o700)).map_err(|_| {
        HookError::runtime(
            "setup-state-mode-failed",
            "setup state directory mode failed",
        )
    })?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_rejects_a_concurrent_provider_change_without_overwriting_it() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let path = temp.path().join("provider.json");
        fs::write(&path, b"reviewed").expect("reviewed config");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("private mode");
        let layout = Layout {
            config_path: temp.path().join("config.toml"),
            state_root: temp.path().join("state"),
        };
        let plan = Plan {
            product: Product::Claude,
            path: path.clone(),
            original: Some(b"reviewed".to_vec()),
            candidate: Some(b"candidate".to_vec()),
            groups: Vec::new(),
            owned_before: 0,
            owned_after: 0,
            legacy_before: 0,
            unrelated_before: 0,
            drifted: false,
        };
        fs::write(&path, b"concurrent").expect("concurrent config");

        let error = apply_plan(&layout, &plan).expect_err("concurrent change");

        assert_eq!(error.code, "provider-config-drift");
        assert_eq!(fs::read(&path).expect("preserved config"), b"concurrent");
    }
}
