use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use crate::error::HookError;
use crate::model::{
    CONFIG_VERSION, Capability, Config, FailurePosture, LoadedPolicy, OverrideClass,
    POLICY_VERSION, PolicyBundle, PolicyRule, Product, ProviderMode, RuleMode,
};
use crate::paths::Layout;

const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const MAX_POLICY_BYTES: u64 = 1024 * 1024;
const MAX_RULES: usize = 512;
const MAX_TEXT: usize = 16 * 1024;

pub fn load(layout: &Layout, policy_override: Option<&Path>) -> Result<LoadedPolicy, HookError> {
    let config_bytes = read_regular(&layout.config_path, MAX_CONFIG_BYTES, "config")?;
    let config_text = std::str::from_utf8(&config_bytes)
        .map_err(|_| HookError::data("config-invalid", "agent-hook config is not UTF-8"))?;
    let config: Config = parse_toml(config_text, "config-invalid")?;
    validate_config(&config)?;

    let policy_path = policy_override.unwrap_or(&config.policy.path);
    if !policy_path.is_absolute() {
        return Err(HookError::data(
            "policy-path-not-absolute",
            "selected policy path must be absolute",
        ));
    }
    let policy_bytes = read_regular(policy_path, MAX_POLICY_BYTES, "policy")?;
    let actual_policy_digest = digest(&policy_bytes);
    if !constant_time_eq(&actual_policy_digest, &config.policy.digest) {
        return Err(HookError::data(
            "policy-digest-mismatch",
            "selected policy bytes do not match the configured digest",
        ));
    }
    let policy_text = std::str::from_utf8(&policy_bytes)
        .map_err(|_| HookError::data("policy-invalid", "agent-hook policy is not UTF-8"))?;
    let bundle: PolicyBundle = parse_toml(policy_text, "policy-invalid")?;
    validate_policy(&bundle, &config)?;

    Ok(LoadedPolicy {
        config,
        bundle,
        config_digest: digest(&config_bytes),
        policy_digest: actual_policy_digest,
    })
}

pub fn read_regular(path: &Path, limit: u64, role: &str) -> Result<Vec<u8>, HookError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| {
            let code = if error.raw_os_error() == Some(libc::ELOOP) {
                format!("{role}-untrusted")
            } else {
                format!("{role}-unavailable")
            };
            HookError::data(code, format!("{role} file is unavailable: {error}"))
        })?;
    let metadata = file.metadata().map_err(|error| {
        HookError::data(
            format!("{role}-unavailable"),
            format!("{role} file metadata is unavailable: {error}"),
        )
    })?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o022 != 0
        || metadata.nlink() != 1
    {
        return Err(HookError::data(
            format!("{role}-untrusted"),
            format!("{role} file type, owner, link count, or write mode is untrusted"),
        ));
    }
    if metadata.len() > limit {
        return Err(HookError::data(
            format!("{role}-too-large"),
            format!("{role} file exceeds its byte limit"),
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            HookError::runtime(
                format!("{role}-read-failed"),
                format!("failed to read {role} file: {error}"),
            )
        })?;
    if bytes.len() as u64 > limit {
        return Err(HookError::data(
            format!("{role}-too-large"),
            format!("{role} file exceeds its byte limit"),
        ));
    }
    Ok(bytes)
}

pub fn digest(bytes: &[u8]) -> String {
    let hash = Sha256::digest(bytes);
    let mut result = String::with_capacity(71);
    result.push_str("sha256:");
    for byte in hash {
        use std::fmt::Write as _;
        let _ = write!(result, "{byte:02x}");
    }
    result
}

pub fn digest_serializable<T: Serialize>(value: &T) -> Result<String, HookError> {
    serde_json::to_vec(value)
        .map(|bytes| digest(&bytes))
        .map_err(|_| HookError::runtime("digest-failed", "value could not be digested"))
}

pub fn valid_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left
            .bytes()
            .zip(right.bytes())
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            })
            == 0
}

fn parse_toml<T: DeserializeOwned>(text: &str, code: &str) -> Result<T, HookError> {
    toml::from_str(text)
        .map_err(|error| HookError::data(code, format!("strict TOML rejected: {error}")))
}

fn validate_config(config: &Config) -> Result<(), HookError> {
    if config.schema_version != CONFIG_VERSION {
        return Err(HookError::data(
            "config-version-unsupported",
            "unsupported agent-hook config schema_version",
        ));
    }
    if !config.policy.path.is_absolute() {
        return Err(HookError::data(
            "policy-path-not-absolute",
            "config policy.path must be absolute",
        ));
    }
    if !valid_digest(&config.policy.digest) {
        return Err(HookError::data(
            "policy-digest-invalid",
            "config policy.digest must be lowercase sha256",
        ));
    }
    for provider in config.providers.keys() {
        if !matches!(provider.as_str(), "codex" | "claude" | "hermes") {
            return Err(HookError::data(
                "provider-unsupported",
                "config contains an unsupported provider",
            ));
        }
    }
    for rule_id in config.overrides.keys() {
        validate_id("override rule id", rule_id)?;
    }
    Ok(())
}

fn validate_policy(bundle: &PolicyBundle, config: &Config) -> Result<(), HookError> {
    if bundle.schema_version != POLICY_VERSION {
        return Err(HookError::data(
            "policy-version-unsupported",
            "unsupported agent-hook policy schema_version",
        ));
    }
    validate_id("bundle id", &bundle.bundle_id)?;
    validate_version(&bundle.version)?;
    if bundle.rules.len() > MAX_RULES {
        return Err(HookError::data(
            "policy-too-many-rules",
            "policy rule count exceeds 512",
        ));
    }
    let mut ids = BTreeSet::new();
    for rule in &bundle.rules {
        validate_id("rule id", &rule.id)?;
        if !ids.insert(rule.id.clone()) {
            return Err(HookError::data(
                "policy-duplicate-rule-id",
                "policy rule IDs must be unique",
            ));
        }
        if rule.products.is_empty() || rule.events.is_empty() {
            return Err(HookError::data(
                "policy-rule-empty-selector",
                "policy rule products and events must be non-empty",
            ));
        }
        let products: BTreeSet<_> = rule.products.iter().collect();
        if products.len() != rule.products.len() {
            return Err(HookError::data(
                "policy-duplicate-product",
                "policy rule products must be unique",
            ));
        }
        let events: BTreeSet<_> = rule.events.iter().collect();
        if events.len() != rule.events.len() {
            return Err(HookError::data(
                "policy-duplicate-event",
                "policy rule events must be unique",
            ));
        }
        for event in &rule.events {
            validate_event(event)?;
            for product in &rule.products {
                if !supported_event(*product, event) {
                    return Err(HookError::data(
                        "policy-event-unsupported",
                        "policy rule selects an event unsupported by its product",
                    ));
                }
            }
        }
        if let Some(matcher) = rule.matcher.as_deref() {
            validate_matcher_expression(matcher)?;
            for event in &rule.events {
                for product in &rule.products {
                    if matcher_input_field(*product, event).is_none() {
                        return Err(HookError::data(
                            "policy-matcher-unsupported",
                            "policy matcher selects an event without native matcher support",
                        ));
                    }
                }
            }
        }
        validate_capability(&rule.capability)?;
        for product in &rule.products {
            for event in &rule.events {
                validate_capability_binding(*product, event, &rule.capability)?;
            }
        }
        if matches!(rule.override_class, OverrideClass::Locked)
            && !matches!(rule.failure_posture, FailurePosture::Closed)
        {
            return Err(HookError::data(
                "locked-rule-failure-posture",
                "locked rules must fail closed",
            ));
        }
    }
    for (rule_id, override_value) in &config.overrides {
        let rule = bundle
            .rules
            .iter()
            .find(|rule| &rule.id == rule_id)
            .ok_or_else(|| {
                HookError::data(
                    "override-rule-missing",
                    "config override references no policy rule",
                )
            })?;
        match rule.override_class {
            OverrideClass::Locked => {
                return Err(HookError::data(
                    "locked-rule-override",
                    "config cannot override a locked rule",
                ));
            }
            OverrideClass::DowngradeOnly
                if override_value.mode.authority() > rule.mode.authority() =>
            {
                return Err(HookError::data(
                    "rule-override-upgrade",
                    "downgrade-only overrides cannot increase authority",
                ));
            }
            OverrideClass::DowngradeOnly | OverrideClass::Free => {}
        }
    }
    Ok(())
}

fn validate_capability_binding(
    product: Product,
    event: &str,
    capability: &Capability,
) -> Result<(), HookError> {
    if !product.enforceable() {
        return Ok(());
    }
    let compatible = match capability {
        Capability::Allow { .. }
        | Capability::SessionActivity { .. }
        | Capability::RuntimeKitHandler { .. } => true,
        Capability::Warn { .. } | Capability::Context { .. } => supports_context(product, event),
        Capability::Block { .. } => supports_block(product, event),
        Capability::Transform { .. } => supports_transform(product, event),
        Capability::OwnerLiveness { .. } | Capability::SemanticConflict { .. } => {
            supports_context(product, event) && supports_block(product, event)
        }
    };
    if compatible {
        Ok(())
    } else {
        Err(HookError::data(
            "policy-capability-event-unsupported",
            "policy capability can produce an action unsupported by the selected provider event",
        ))
    }
}

fn supports_context(product: Product, event: &str) -> bool {
    match product {
        Product::Codex => matches!(
            event,
            "SessionStart" | "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "SubagentStart"
        ),
        Product::Claude => matches!(
            event,
            "SessionStart"
                | "UserPromptSubmit"
                | "PreToolUse"
                | "PostToolUse"
                | "PostToolUseFailure"
                | "SubagentStart"
                | "SubagentStop"
                | "Stop"
        ),
        Product::Hermes => false,
    }
}

fn supports_block(product: Product, event: &str) -> bool {
    match product {
        Product::Codex => matches!(
            event,
            "SessionStart"
                | "UserPromptSubmit"
                | "PermissionRequest"
                | "PreToolUse"
                | "PostToolUse"
                | "PreCompact"
                | "PostCompact"
                | "SubagentStop"
                | "Stop"
        ),
        Product::Claude => matches!(
            event,
            "UserPromptSubmit"
                | "PermissionRequest"
                | "PreToolUse"
                | "PostToolUse"
                | "PostToolUseFailure"
                | "PreCompact"
                | "SubagentStop"
                | "Stop"
                | "Elicitation"
                | "ElicitationResult"
        ),
        Product::Hermes => false,
    }
}

fn supports_transform(product: Product, event: &str) -> bool {
    match product {
        Product::Codex => event == "PreToolUse",
        Product::Claude => matches!(event, "PreToolUse" | "PermissionRequest" | "PostToolUse"),
        Product::Hermes => false,
    }
}

fn validate_capability(capability: &Capability) -> Result<(), HookError> {
    match capability {
        Capability::Allow { reason_code }
        | Capability::SessionActivity { reason_code }
        | Capability::SemanticConflict { reason_code } => validate_id("reason code", reason_code),
        Capability::Warn {
            reason_code,
            message,
        }
        | Capability::Block {
            reason_code,
            message,
        } => {
            validate_id("reason code", reason_code)?;
            validate_bounded("message", message, 256)
        }
        Capability::Context { reason_code, text } => {
            validate_id("reason code", reason_code)?;
            validate_bounded("context", text, MAX_TEXT)
        }
        Capability::Transform {
            reason_code,
            replacement,
        } => {
            validate_id("reason code", reason_code)?;
            if !replacement.is_object()
                || serde_json::to_vec(replacement).map_or(true, |bytes| bytes.len() > MAX_TEXT)
            {
                return Err(HookError::data(
                    "replacement-invalid",
                    "transform replacement must be an object no larger than 16 KiB",
                ));
            }
            Ok(())
        }
        Capability::OwnerLiveness {
            reason_code,
            legacy_ttl_seconds,
        } => {
            validate_id("reason code", reason_code)?;
            if *legacy_ttl_seconds == 0 || *legacy_ttl_seconds > 900 {
                return Err(HookError::data(
                    concat!("leg", "acy-ttl-invalid"),
                    "owner-liveness compatibility TTL must be 1..=900",
                ));
            }
            Ok(())
        }
        Capability::RuntimeKitHandler { handler_id } => {
            if runtime_handler_filename(handler_id).is_none() {
                return Err(HookError::data(
                    "handler-id-unsupported",
                    "runtime-kit handler_id is not in the compiled v1 allowlist",
                ));
            }
            Ok(())
        }
    }
}

pub fn effective_mode_for_product(
    loaded: &LoadedPolicy,
    product: Product,
    rule: &PolicyRule,
) -> RuleMode {
    let provider_mode = if rule.override_class == OverrideClass::Locked {
        ProviderMode::Enforce
    } else {
        loaded
            .config
            .providers
            .get(product.as_str())
            .map_or(ProviderMode::Enforce, |provider| provider.mode)
    };
    let mode = loaded
        .config
        .overrides
        .get(&rule.id)
        .map_or(rule.mode, |override_value| override_value.mode);
    match provider_mode {
        ProviderMode::Enforce => mode,
        ProviderMode::Shadow => {
            if mode == RuleMode::Disabled {
                mode
            } else {
                RuleMode::Shadow
            }
        }
        ProviderMode::Disabled => RuleMode::Disabled,
    }
}

pub fn supported_event(product: Product, event: &str) -> bool {
    match product {
        Product::Codex => matches!(
            event,
            "SessionStart"
                | "UserPromptSubmit"
                | "PermissionRequest"
                | "PreToolUse"
                | "PostToolUse"
                | "PreCompact"
                | "PostCompact"
                | "SubagentStart"
                | "SubagentStop"
                | "Stop"
        ),
        Product::Claude => matches!(
            event,
            "SessionStart"
                | "UserPromptSubmit"
                | "PermissionRequest"
                | "PreToolUse"
                | "PostToolUse"
                | "PostToolUseFailure"
                | "PreCompact"
                | "SubagentStart"
                | "SubagentStop"
                | "Stop"
                | "StopFailure"
                | "Notification"
                | "Elicitation"
                | "ElicitationResult"
        ),
        Product::Hermes => matches!(
            event,
            "pre_llm_call" | "post_llm_call" | "pre_approval_request" | "post_approval_response"
        ),
    }
}

pub fn matcher_input_field(product: Product, event: &str) -> Option<&'static str> {
    match (product, event) {
        (Product::Codex | Product::Claude, "SessionStart") => Some("source"),
        (Product::Codex | Product::Claude, "PermissionRequest" | "PreToolUse" | "PostToolUse")
        | (Product::Claude, "PostToolUseFailure") => Some("tool_name"),
        (Product::Codex | Product::Claude, "PreCompact") | (Product::Codex, "PostCompact") => {
            Some("trigger")
        }
        (Product::Codex | Product::Claude, "SubagentStart" | "SubagentStop") => Some("agent_type"),
        (Product::Claude, "Notification") => Some("notification_type"),
        (Product::Claude, "Elicitation" | "ElicitationResult") => Some("mcp_server_name"),
        (Product::Claude, "StopFailure") => Some("error"),
        _ => None,
    }
}

pub fn validate_matcher_expression(expression: &str) -> Result<(), HookError> {
    if expression.is_empty() || expression.len() > 1024 {
        return Err(HookError::data(
            "matcher-expression-invalid",
            "matcher expression is empty or exceeds 1024 bytes",
        ));
    }
    let atoms = expression.split('|').collect::<Vec<_>>();
    if atoms.len() > 64
        || atoms.iter().any(|atom| {
            atom.is_empty()
                || atom.len() > 128
                || !atom.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':')
                })
        })
    {
        return Err(HookError::data(
            "matcher-expression-invalid",
            "matcher must be 1..=64 literal atoms separated only by |",
        ));
    }
    let unique = atoms.iter().copied().collect::<BTreeSet<_>>();
    if unique.len() != atoms.len() {
        return Err(HookError::data(
            "matcher-expression-duplicate",
            "matcher expression atoms must be unique",
        ));
    }
    Ok(())
}

pub fn matcher_expression_matches(expression: &str, candidate: &str) -> bool {
    expression.split('|').any(|atom| atom == candidate)
}

pub fn runtime_handler_filename(id: &str) -> Option<&'static str> {
    Some(match id {
        "agent-scope-lock-guard" => "agent-scope-lock-guard.py",
        "block-claude-coauthor-trailer" => "block-claude-coauthor-trailer.py",
        "block-direct-git-commit" => "block-direct-git-commit.py",
        "block-direct-git-worktree" => "block-direct-git-worktree.py",
        "block-direct-pr-create" => "block-direct-pr-create.py",
        "block-direct-python" => "block-direct-python.py",
        "block-project-memory-write" => "block-project-memory-write.py",
        "block-unsafe-default-delivery" => "block-unsafe-default-delivery.py",
        "checkout-lease-guard" => "checkout-lease-guard.py",
        "finish-line-record" => "finish-line-record.py",
        "forge-label-reminder" => "forge-label-reminder.py",
        "mcp-secret-scan" => "mcp-secret-scan.py",
        "memory-write-principle-reminder" => "memory-write-principle-reminder.py",
        "portable-paths-scan" => "portable-paths-scan.py",
        "pre-edit-intent-gate" => "pre-edit-intent-gate.py",
        "semantic-commit-body-gate" => "semantic-commit-body-gate.py",
        "session-start-healthcheck" => "session-start-healthcheck.sh",
        "skill-usage-reminder" => "skill-usage-reminder.py",
        "stop-finish-line-gate" => "stop-finish-line-gate.py",
        "stop-pre-pr-reminder" => "stop-pre-pr-reminder.sh",
        "user-prompt-agent-docs" => "user-prompt-agent-docs.sh",
        "user-prompt-agent-memory" => "user-prompt-agent-memory.sh",
        _ => return None,
    })
}

fn validate_id(label: &str, value: &str) -> Result<(), HookError> {
    if value.is_empty()
        || value.len() > 128
        || !value.is_ascii()
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        })
    {
        return Err(HookError::data(
            "identifier-invalid",
            format!("{label} is not a bounded stable identifier"),
        ));
    }
    Ok(())
}

fn validate_event(value: &str) -> Result<(), HookError> {
    validate_bounded("event", value, 128)?;
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(HookError::data(
            "event-invalid",
            "event contains unsupported characters",
        ));
    }
    Ok(())
}

fn validate_version(value: &str) -> Result<(), HookError> {
    validate_bounded("policy version", value, 64)
}

fn validate_bounded(label: &str, value: &str, max: usize) -> Result<(), HookError> {
    if value.is_empty() || value.len() > max || value.contains('\0') {
        return Err(HookError::data(
            "field-invalid",
            format!("{label} is empty or exceeds its bound"),
        ));
    }
    Ok(())
}
