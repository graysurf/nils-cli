use std::fs;
use std::io::{self, IsTerminal, Read};

use serde::Serialize;

use crate::cli::{ApplyArgs, OutputFormat};
use crate::errors::AppError;
use crate::output::{emit_data_with_warnings, format_item_id, parse_item_id, text};
use crate::preprocess::{ContentType, ValidationStatus};
use crate::storage::Storage;
use crate::storage::derivations::{self, ApplyInputItem, IncomingStatus};

const DEFAULT_AGENT_RUN_ID: &str = "memo-cli";

#[derive(Debug, Serialize)]
struct JsonApplyResult<'a> {
    dry_run: bool,
    processed: i64,
    accepted: i64,
    skipped: i64,
    failed: i64,
    items: Vec<JsonApplyItem<'a>>,
    changes: &'a [derivations::ApplyChange],
}

#[derive(Debug, Serialize)]
struct JsonApplyItem<'a> {
    item_id: String,
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    derivation_version: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_type: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    validation_status: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    validation_errors: Option<&'a [derivations::ApplyValidationError]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a derivations::ApplyItemError>,
}

pub fn run(storage: &Storage, format: OutputFormat, args: &ApplyArgs) -> Result<(), AppError> {
    if args.input.is_some() == args.stdin {
        return Err(AppError::usage(
            "apply requires exactly one input source: --input <file> or --stdin",
        ));
    }

    let payload = if let Some(path) = &args.input {
        fs::read_to_string(path).map_err(|err| {
            AppError::runtime(format!(
                "failed to read apply payload from {}: {err}",
                path.display()
            ))
            .with_code("io-read-failed")
        })?
    } else {
        if io::stdin().is_terminal() {
            return Err(AppError::usage(
                "--stdin requires piped input; use --input <file> or pipe content into stdin",
            ));
        }
        let mut buffer = String::new();
        io::stdin().read_to_string(&mut buffer).map_err(|err| {
            AppError::runtime(format!("failed to read apply payload from stdin: {err}"))
                .with_code("io-read-failed")
        })?;
        buffer
    };

    let value: serde_json::Value = serde_json::from_str(&payload).map_err(|err| {
        AppError::invalid_apply_payload(format!("invalid apply payload JSON: {err}"), None)
    })?;
    let parsed = parse_apply_items(value)?;
    if parsed.items.is_empty() {
        return Err(AppError::invalid_apply_payload(
            "payload must include at least one item",
            Some("payload.items"),
        ));
    }

    let default_agent_run_id = parsed
        .agent_run_id
        .unwrap_or_else(|| DEFAULT_AGENT_RUN_ID.to_string());
    let summary = storage.with_transaction(|tx| {
        derivations::apply_items(tx, &parsed.items, args.dry_run, &default_agent_run_id)
    })?;

    if format.is_json() {
        let warnings: Vec<String> = summary
            .items
            .iter()
            .filter_map(|item| {
                item.error.as_ref().map(|error| {
                    format!(
                        "{} {}: {}",
                        format_item_id(item.item_id),
                        item.status,
                        error.message
                    )
                })
            })
            .collect();
        let result = JsonApplyResult {
            dry_run: summary.dry_run,
            processed: summary.processed,
            accepted: summary.accepted,
            skipped: summary.skipped,
            failed: summary.failed,
            changes: &summary.changes,
            items: summary
                .items
                .iter()
                .map(|item| JsonApplyItem {
                    item_id: format_item_id(item.item_id),
                    status: &item.status,
                    derivation_version: item.derivation_version,
                    content_type: item.content_type(),
                    validation_status: item.validation_status(),
                    validation_errors: item.validation_errors(),
                    error: item.error.as_ref(),
                })
                .collect(),
        };
        return emit_data_with_warnings("cli.memo-cli.apply.v1", result, warnings);
    }

    text::print_apply(&summary);
    Ok(())
}

#[derive(Debug)]
struct ParsedApplyPayload {
    agent_run_id: Option<String>,
    items: Vec<ApplyInputItem>,
}

fn parse_apply_items(value: serde_json::Value) -> Result<ParsedApplyPayload, AppError> {
    match value {
        serde_json::Value::Array(items) => parse_apply_item_array(items, None),
        serde_json::Value::Object(mut object) => {
            let agent_run_id =
                optional_trimmed_string(object.remove("agent_run_id"), "payload.agent_run_id")?;
            let items = object.remove("items").ok_or_else(|| {
                AppError::invalid_apply_payload("payload.items is required", Some("payload.items"))
            })?;
            match items {
                serde_json::Value::Array(array) => parse_apply_item_array(array, agent_run_id),
                _ => Err(AppError::invalid_apply_payload(
                    "payload.items must be an array",
                    Some("payload.items"),
                )),
            }
        }
        _ => Err(AppError::invalid_apply_payload(
            "payload must be an object with items[] or a top-level array",
            Some("payload"),
        )),
    }
}

fn parse_apply_item_array(
    items: Vec<serde_json::Value>,
    default_agent_run_id: Option<String>,
) -> Result<ParsedApplyPayload, AppError> {
    let mut parsed = Vec::with_capacity(items.len());
    for (index, item_value) in items.into_iter().enumerate() {
        let path = format!("payload.items[{index}]");
        let object = item_value.as_object().ok_or_else(|| {
            AppError::invalid_apply_payload("item must be an object", Some(&path))
        })?;

        let item_id = parse_item_id_value(object.get("item_id"), &format!("{path}.item_id"))?;
        let status = parse_status(object.get("status"), &format!("{path}.status"))?;
        let derivation_hash = optional_trimmed_string(
            object.get("derivation_hash").cloned(),
            &format!("{path}.derivation_hash"),
        )?
        .unwrap_or_else(|| derive_hash(&item_value));
        if derivation_hash.is_empty() {
            return Err(AppError::invalid_apply_payload(
                "derivation_hash must be non-empty when provided",
                Some(&format!("{path}.derivation_hash")),
            ));
        }

        let base_derivation_id = optional_i64(
            object.get("base_derivation_id"),
            &format!("{path}.base_derivation_id"),
        )?;
        let summary =
            optional_trimmed_string(object.get("summary").cloned(), &format!("{path}.summary"))?;
        let category =
            optional_trimmed_string(object.get("category").cloned(), &format!("{path}.category"))?;
        let priority = optional_priority(object.get("priority"), &format!("{path}.priority"))?;
        let due_at =
            optional_trimmed_string(object.get("due_at").cloned(), &format!("{path}.due_at"))?;
        let normalized_text = optional_trimmed_string(
            object.get("normalized_text").cloned(),
            &format!("{path}.normalized_text"),
        )?;
        let confidence = optional_f64(object.get("confidence"), &format!("{path}.confidence"))?;
        let content_type =
            optional_content_type(object.get("content_type"), &format!("{path}.content_type"))?;
        let validation_status = optional_validation_status(
            object.get("validation_status"),
            &format!("{path}.validation_status"),
        )?;
        let validation_errors = optional_validation_errors(
            object.get("validation_errors"),
            &format!("{path}.validation_errors"),
        )?;
        let payload_json = object
            .get("payload")
            .cloned()
            .unwrap_or_else(|| item_value.clone());
        let conflict_reason = optional_trimmed_string(
            object.get("conflict_reason").cloned(),
            &format!("{path}.conflict_reason"),
        )?;
        let tags = parse_tags(object.get("tags"), &format!("{path}.tags"))?;
        let agent_run_id = optional_trimmed_string(
            object.get("agent_run_id").cloned(),
            &format!("{path}.agent_run_id"),
        )?;

        parsed.push(ApplyInputItem {
            item_id,
            status,
            derivation_hash,
            base_derivation_id,
            summary,
            category,
            priority,
            due_at,
            normalized_text,
            confidence,
            content_type,
            validation_status,
            validation_errors,
            payload_json,
            conflict_reason,
            tags,
            agent_run_id,
        });
    }

    Ok(ParsedApplyPayload {
        agent_run_id: default_agent_run_id,
        items: parsed,
    })
}

fn parse_item_id_value(value: Option<&serde_json::Value>, path: &str) -> Result<i64, AppError> {
    let value =
        value.ok_or_else(|| AppError::invalid_apply_payload("item_id is required", Some(path)))?;

    match value {
        serde_json::Value::Number(number) => {
            number.as_i64().filter(|id| *id > 0).ok_or_else(|| {
                AppError::invalid_apply_payload("item_id must be a positive integer", Some(path))
            })
        }
        serde_json::Value::String(raw) => parse_item_id(raw).ok_or_else(|| {
            AppError::invalid_apply_payload(
                "item_id must be a positive integer or itm_* identifier",
                Some(path),
            )
        }),
        _ => Err(AppError::invalid_apply_payload(
            "item_id must be a positive integer or itm_* identifier",
            Some(path),
        )),
    }
}

fn parse_status(value: Option<&serde_json::Value>, path: &str) -> Result<IncomingStatus, AppError> {
    let Some(value) = value else {
        return Ok(IncomingStatus::Accepted);
    };

    let raw = value
        .as_str()
        .ok_or_else(|| AppError::invalid_apply_payload("status must be a string", Some(path)))?;
    let Some(status) = IncomingStatus::parse(raw) else {
        return Err(AppError::invalid_apply_payload(
            "status must be accepted in v1",
            Some(path),
        ));
    };
    if status != IncomingStatus::Accepted {
        return Err(AppError::invalid_apply_payload(
            "status must be accepted in v1",
            Some(path),
        ));
    }
    Ok(status)
}

fn optional_trimmed_string(
    value: Option<serde_json::Value>,
    path: &str,
) -> Result<Option<String>, AppError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let raw = value
        .as_str()
        .ok_or_else(|| AppError::invalid_apply_payload("value must be a string", Some(path)))?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(Some(trimmed.to_string()))
}

fn optional_priority(
    value: Option<&serde_json::Value>,
    path: &str,
) -> Result<Option<String>, AppError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let raw = value
        .as_str()
        .ok_or_else(|| AppError::invalid_apply_payload("priority must be a string", Some(path)))?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    match trimmed {
        "low" | "medium" | "high" | "urgent" => Ok(Some(trimmed.to_string())),
        _ => Err(AppError::invalid_apply_payload(
            "priority must be low|medium|high|urgent",
            Some(path),
        )),
    }
}

fn optional_i64(value: Option<&serde_json::Value>, path: &str) -> Result<Option<i64>, AppError> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        serde_json::Value::Number(number) => number
            .as_i64()
            .filter(|value| *value > 0)
            .map(Some)
            .ok_or_else(|| {
                AppError::invalid_apply_payload("value must be a positive integer", Some(path))
            }),
        serde_json::Value::Null => Ok(None),
        _ => Err(AppError::invalid_apply_payload(
            "value must be a positive integer",
            Some(path),
        )),
    }
}

fn optional_f64(value: Option<&serde_json::Value>, path: &str) -> Result<Option<f64>, AppError> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        serde_json::Value::Number(number) => {
            let confidence = number.as_f64().ok_or_else(|| {
                AppError::invalid_apply_payload("confidence must be a number", Some(path))
            })?;
            if !(0.0..=1.0).contains(&confidence) {
                return Err(AppError::invalid_apply_payload(
                    "confidence must be between 0.0 and 1.0",
                    Some(path),
                ));
            }
            Ok(Some(confidence))
        }
        serde_json::Value::Null => Ok(None),
        _ => Err(AppError::invalid_apply_payload(
            "confidence must be a number",
            Some(path),
        )),
    }
}

fn optional_content_type(
    value: Option<&serde_json::Value>,
    path: &str,
) -> Result<Option<String>, AppError> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        serde_json::Value::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            if let Some(content_type) = ContentType::parse(trimmed) {
                return Ok(Some(content_type.as_str().to_string()));
            }
            Err(AppError::invalid_apply_payload(
                "content_type must be url|json|yaml|xml|markdown|text|unknown",
                Some(path),
            ))
        }
        serde_json::Value::Null => Ok(None),
        _ => Err(AppError::invalid_apply_payload(
            "content_type must be a string",
            Some(path),
        )),
    }
}

fn optional_validation_status(
    value: Option<&serde_json::Value>,
    path: &str,
) -> Result<Option<String>, AppError> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        serde_json::Value::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            if let Some(status) = ValidationStatus::parse(trimmed) {
                return Ok(Some(status.as_str().to_string()));
            }
            Err(AppError::invalid_apply_payload(
                "validation_status must be valid|invalid|unknown|skipped",
                Some(path),
            ))
        }
        serde_json::Value::Null => Ok(None),
        _ => Err(AppError::invalid_apply_payload(
            "validation_status must be a string",
            Some(path),
        )),
    }
}

fn optional_validation_errors(
    value: Option<&serde_json::Value>,
    path: &str,
) -> Result<Option<Vec<derivations::ApplyValidationError>>, AppError> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        serde_json::Value::Array(values) => {
            let mut errors = Vec::with_capacity(values.len());
            for (index, entry) in values.iter().enumerate() {
                let entry_path = format!("{path}[{index}]");
                let object = entry.as_object().ok_or_else(|| {
                    AppError::invalid_apply_payload(
                        "validation_errors must be an array of objects",
                        Some(&entry_path),
                    )
                })?;
                let code = required_trimmed_string(
                    object.get("code"),
                    &format!("{entry_path}.code"),
                    "validation_errors[].code",
                )?;
                let message = required_trimmed_string(
                    object.get("message"),
                    &format!("{entry_path}.message"),
                    "validation_errors[].message",
                )?;
                let field_path = optional_trimmed_string(
                    object.get("path").cloned(),
                    &format!("{entry_path}.path"),
                )?;
                errors.push(derivations::ApplyValidationError {
                    code,
                    message,
                    path: field_path,
                });
            }
            Ok(Some(errors))
        }
        serde_json::Value::Null => Ok(None),
        _ => Err(AppError::invalid_apply_payload(
            "validation_errors must be an array of objects",
            Some(path),
        )),
    }
}

fn required_trimmed_string(
    value: Option<&serde_json::Value>,
    path: &str,
    field_name: &str,
) -> Result<String, AppError> {
    let value = value.ok_or_else(|| {
        AppError::invalid_apply_payload(format!("{field_name} is required"), Some(path))
    })?;
    let raw = value.as_str().ok_or_else(|| {
        AppError::invalid_apply_payload(format!("{field_name} must be a string"), Some(path))
    })?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::invalid_apply_payload(
            format!("{field_name} must be a non-empty string"),
            Some(path),
        ));
    }
    Ok(trimmed.to_string())
}

fn parse_tags(value: Option<&serde_json::Value>, path: &str) -> Result<Vec<String>, AppError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };

    match value {
        serde_json::Value::Array(values) => {
            let mut tags = Vec::new();
            for (index, tag) in values.iter().enumerate() {
                let raw = tag.as_str().ok_or_else(|| {
                    AppError::invalid_apply_payload(
                        "tags must be an array of strings",
                        Some(&format!("{path}[{index}]")),
                    )
                })?;
                let trimmed = raw.trim();
                if !trimmed.is_empty() {
                    tags.push(trimmed.to_string());
                }
            }
            Ok(tags)
        }
        serde_json::Value::Null => Ok(Vec::new()),
        _ => Err(AppError::invalid_apply_payload(
            "tags must be an array of strings",
            Some(path),
        )),
    }
}

fn derive_hash(value: &serde_json::Value) -> String {
    let canonical = serde_json::to_string(value).unwrap_or_default();
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in canonical.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("h{hash:016x}")
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn error_path(err: &AppError) -> Option<&str> {
        err.details()
            .and_then(|details| details.get("path"))
            .and_then(Value::as_str)
    }

    #[test]
    fn parse_apply_items_accepts_object_payload_and_trims_optional_values() {
        let payload = json!({
            "agent_run_id": " agent-run-1 ",
            "items": [{
                "item_id": "itm_00000042",
                "status": "accepted",
                "summary": "  buy milk  ",
                "category": "  errands ",
                "priority": "high",
                "due_at": " 2026-03-10T09:00:00Z ",
                "normalized_text": "  buy milk tomorrow ",
                "confidence": 0.82,
                "content_type": " json ",
                "validation_status": " invalid ",
                "validation_errors": [{
                    "code": "parse-error",
                    "message": " trailing comma",
                    "path": " $.items[0] "
                }],
                "tags": [" home ", "", "urgent"],
                "payload": { "model": "test" }
            }]
        });

        let parsed = parse_apply_items(payload).expect("payload should parse");
        assert_eq!(parsed.agent_run_id.as_deref(), Some("agent-run-1"));
        assert_eq!(parsed.items.len(), 1);

        let item = &parsed.items[0];
        assert_eq!(item.item_id, 42);
        assert_eq!(item.status, IncomingStatus::Accepted);
        assert_eq!(item.summary.as_deref(), Some("buy milk"));
        assert_eq!(item.category.as_deref(), Some("errands"));
        assert_eq!(item.priority.as_deref(), Some("high"));
        assert_eq!(item.due_at.as_deref(), Some("2026-03-10T09:00:00Z"));
        assert_eq!(item.normalized_text.as_deref(), Some("buy milk tomorrow"));
        assert_eq!(item.confidence, Some(0.82));
        assert_eq!(item.content_type.as_deref(), Some("json"));
        assert_eq!(item.validation_status.as_deref(), Some("invalid"));
        assert_eq!(
            item.validation_errors,
            Some(vec![derivations::ApplyValidationError {
                code: "parse-error".to_string(),
                message: "trailing comma".to_string(),
                path: Some("$.items[0]".to_string()),
            }])
        );
        assert_eq!(item.tags, vec!["home", "urgent"]);
        assert_eq!(item.payload_json, json!({ "model": "test" }));
        assert_eq!(item.agent_run_id, None);
        assert!(item.derivation_hash.starts_with('h'));
        assert_eq!(item.derivation_hash.len(), 17);
    }

    #[test]
    fn parse_apply_items_accepts_top_level_array_with_null_optionals() {
        let payload = json!([{
            "item_id": 7,
            "status": "accepted",
            "tags": null,
            "confidence": null,
            "base_derivation_id": null
        }]);

        let parsed = parse_apply_items(payload).expect("array payload should parse");
        assert_eq!(parsed.agent_run_id, None);
        assert_eq!(parsed.items.len(), 1);

        let item = &parsed.items[0];
        assert_eq!(item.item_id, 7);
        assert_eq!(item.tags, Vec::<String>::new());
        assert_eq!(item.confidence, None);
        assert_eq!(item.base_derivation_id, None);
        assert!(item.derivation_hash.starts_with('h'));
    }

    #[test]
    fn parse_apply_items_rejects_missing_or_invalid_items_container() {
        let missing_items = parse_apply_items(json!({"agent_run_id":"x"})).expect_err("must fail");
        assert_eq!(missing_items.code(), "invalid-apply-payload");
        assert_eq!(error_path(&missing_items), Some("payload.items"));
        assert!(
            missing_items
                .message()
                .contains("payload.items is required"),
            "unexpected message: {}",
            missing_items.message()
        );

        let non_array_items = parse_apply_items(json!({"items": {}})).expect_err("must fail");
        assert_eq!(non_array_items.code(), "invalid-apply-payload");
        assert_eq!(error_path(&non_array_items), Some("payload.items"));
        assert!(
            non_array_items
                .message()
                .contains("payload.items must be an array"),
            "unexpected message: {}",
            non_array_items.message()
        );
    }

    #[test]
    fn parse_apply_items_rejects_invalid_item_shapes_and_types() {
        let non_object_item = parse_apply_items(json!({"items": [1]})).expect_err("must fail");
        assert_eq!(non_object_item.code(), "invalid-apply-payload");
        assert_eq!(error_path(&non_object_item), Some("payload.items[0]"));
        assert!(
            non_object_item.message().contains("item must be an object"),
            "unexpected message: {}",
            non_object_item.message()
        );

        let invalid_item_id =
            parse_apply_items(json!({"items": [{"item_id": true}]})).expect_err("must fail");
        assert_eq!(invalid_item_id.code(), "invalid-apply-payload");
        assert_eq!(
            error_path(&invalid_item_id),
            Some("payload.items[0].item_id")
        );
        assert!(
            invalid_item_id
                .message()
                .contains("item_id must be a positive integer"),
            "unexpected message: {}",
            invalid_item_id.message()
        );
    }

    #[test]
    fn parse_apply_items_rejects_invalid_status_priority_confidence_and_tags() {
        let invalid_status = parse_apply_items(json!({
            "items": [{"item_id": 1, "status": "rejected"}]
        }))
        .expect_err("must fail");
        assert_eq!(error_path(&invalid_status), Some("payload.items[0].status"));

        let invalid_priority = parse_apply_items(json!({
            "items": [{"item_id": 1, "priority": "p1"}]
        }))
        .expect_err("must fail");
        assert_eq!(
            error_path(&invalid_priority),
            Some("payload.items[0].priority")
        );

        let invalid_confidence = parse_apply_items(json!({
            "items": [{"item_id": 1, "confidence": 1.1}]
        }))
        .expect_err("must fail");
        assert_eq!(
            error_path(&invalid_confidence),
            Some("payload.items[0].confidence")
        );

        let invalid_tags = parse_apply_items(json!({
            "items": [{"item_id": 1, "tags": [123]}]
        }))
        .expect_err("must fail");
        assert_eq!(error_path(&invalid_tags), Some("payload.items[0].tags[0]"));

        let invalid_content_type = parse_apply_items(json!({
            "items": [{"item_id": 1, "content_type": "pdf"}]
        }))
        .expect_err("must fail");
        assert_eq!(
            error_path(&invalid_content_type),
            Some("payload.items[0].content_type")
        );

        let invalid_validation_status = parse_apply_items(json!({
            "items": [{"item_id": 1, "validation_status": "failed"}]
        }))
        .expect_err("must fail");
        assert_eq!(
            error_path(&invalid_validation_status),
            Some("payload.items[0].validation_status")
        );

        let invalid_validation_errors = parse_apply_items(json!({
            "items": [{
                "item_id": 1,
                "validation_errors": [{
                    "code": "",
                    "message": "broken"
                }]
            }]
        }))
        .expect_err("must fail");
        assert_eq!(
            error_path(&invalid_validation_errors),
            Some("payload.items[0].validation_errors[0].code")
        );
    }

    #[test]
    fn parse_apply_items_rejects_invalid_base_derivation_id_and_non_string_fields() {
        let invalid_base = parse_apply_items(json!({
            "items": [{"item_id": 1, "base_derivation_id": -3}]
        }))
        .expect_err("must fail");
        assert_eq!(
            error_path(&invalid_base),
            Some("payload.items[0].base_derivation_id")
        );
        assert!(
            invalid_base.message().contains("positive integer"),
            "unexpected message: {}",
            invalid_base.message()
        );

        let invalid_category = parse_apply_items(json!({
            "items": [{"item_id": 1, "category": 123}]
        }))
        .expect_err("must fail");
        assert_eq!(
            error_path(&invalid_category),
            Some("payload.items[0].category")
        );
        assert!(
            invalid_category
                .message()
                .contains("value must be a string"),
            "unexpected message: {}",
            invalid_category.message()
        );
    }

    #[test]
    fn derive_hash_is_stable_for_same_payload() {
        let value = json!({
            "item_id": 11,
            "summary": "same payload",
            "tags": ["a", "b"]
        });

        let first = derive_hash(&value);
        let second = derive_hash(&value);
        assert_eq!(first, second);
        assert!(first.starts_with('h'));
        assert_eq!(first.len(), 17);
    }
}
