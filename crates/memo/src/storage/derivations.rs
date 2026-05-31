use std::collections::HashSet;

use rusqlite::{Transaction, params};
use serde::Serialize;
use serde_json::{Value, json};

use crate::errors::AppError;
use crate::preprocess::{self, ValidationStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncomingStatus {
    Accepted,
    Rejected,
    Conflict,
}

impl IncomingStatus {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "accepted" => Some(Self::Accepted),
            "rejected" => Some(Self::Rejected),
            "conflict" => Some(Self::Conflict),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ApplyInputItem {
    pub item_id: i64,
    pub status: IncomingStatus,
    pub derivation_hash: String,
    pub base_derivation_id: Option<i64>,
    pub summary: Option<String>,
    pub category: Option<String>,
    pub priority: Option<String>,
    pub due_at: Option<String>,
    pub normalized_text: Option<String>,
    pub confidence: Option<f64>,
    pub content_type: Option<String>,
    pub validation_status: Option<String>,
    pub validation_errors: Option<Vec<ApplyValidationError>>,
    pub payload_json: serde_json::Value,
    pub conflict_reason: Option<String>,
    pub tags: Vec<String>,
    pub agent_run_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ApplyValidationError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyItemError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyItemOutcome {
    pub item_id: i64,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derivation_version: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation_errors: Option<Vec<ApplyValidationError>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ApplyItemError>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ApplyChange {
    pub item_id: i64,
    pub field: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old: Option<Value>,
    pub new: Value,
}

impl ApplyItemOutcome {
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    pub fn validation_status(&self) -> Option<&str> {
        self.validation_status.as_deref()
    }

    pub fn validation_errors(&self) -> Option<&[ApplyValidationError]> {
        self.validation_errors.as_deref()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplySummary {
    pub dry_run: bool,
    pub processed: i64,
    pub accepted: i64,
    pub skipped: i64,
    pub failed: i64,
    pub items: Vec<ApplyItemOutcome>,
    pub changes: Vec<ApplyChange>,
}

#[derive(Debug, Clone, Default)]
struct ResolvedMetadata {
    content_type: Option<String>,
    validation_status: Option<String>,
    validation_errors: Option<Vec<ApplyValidationError>>,
}

impl ResolvedMetadata {
    fn from_item(item: &ApplyInputItem) -> Self {
        Self {
            content_type: item.content_type.clone(),
            validation_status: item.validation_status.clone(),
            validation_errors: item.validation_errors.clone(),
        }
    }
}

pub fn apply_items(
    tx: &Transaction<'_>,
    items: &[ApplyInputItem],
    dry_run: bool,
    default_agent_run_id: &str,
) -> Result<ApplySummary, AppError> {
    let mut accepted = 0_i64;
    let mut skipped = 0_i64;
    let mut failed = 0_i64;
    let mut outcomes = Vec::with_capacity(items.len());
    let mut changes = Vec::new();

    for item in items {
        let mut metadata = ResolvedMetadata::from_item(item);

        if !item_exists(tx, item.item_id)? {
            failed += 1;
            outcomes.push(build_outcome(
                item.item_id,
                "failed",
                None,
                &metadata,
                Some(ApplyItemError {
                    code: "invalid-apply-payload".to_string(),
                    message: "item_id does not exist".to_string(),
                    details: None,
                }),
            ));
            continue;
        }

        populate_missing_metadata(tx, item.item_id, &mut metadata)?;

        let active = current_active(tx, item.item_id)?;
        if let Some(base_derivation_id) = item.base_derivation_id
            && active.map(|row| row.0) != Some(base_derivation_id)
        {
            skipped += 1;
            outcomes.push(build_outcome(
                item.item_id,
                "skipped",
                None,
                &metadata,
                Some(ApplyItemError {
                    code: "apply-item-conflict".to_string(),
                    message: "incoming base derivation does not match active derivation"
                        .to_string(),
                    details: Some(serde_json::json!({
                        "incoming_base_derivation_id": base_derivation_id,
                        "active_derivation_id": active.map(|row| row.0),
                    })),
                }),
            ));
            continue;
        }

        if let Some(existing_version) =
            derivation_version_by_hash(tx, item.item_id, &item.derivation_hash)?
        {
            skipped += 1;
            outcomes.push(build_outcome(
                item.item_id,
                "skipped",
                Some(existing_version),
                &metadata,
                None,
            ));
            continue;
        }

        if item.status == IncomingStatus::Conflict && item.conflict_reason.is_none() {
            failed += 1;
            outcomes.push(build_outcome(
                item.item_id,
                "failed",
                None,
                &metadata,
                Some(ApplyItemError {
                    code: "invalid-apply-payload".to_string(),
                    message: "status=conflict requires conflict_reason".to_string(),
                    details: None,
                }),
            ));
            continue;
        }

        let next_version = next_derivation_version(tx, item.item_id)?;
        if dry_run {
            accepted += 1;
            changes.extend(collect_dry_run_changes(tx, item, &metadata)?);
            outcomes.push(build_outcome(
                item.item_id,
                "accepted",
                Some(next_version),
                &metadata,
                None,
            ));
            continue;
        }

        if item.status == IncomingStatus::Accepted {
            tx.execute(
                "update item_derivations
                 set is_active = 0
                 where item_id = ?1 and is_active = 1 and status = 'accepted'",
                params![item.item_id],
            )
            .map_err(AppError::db_write)?;
        }

        let payload_json = serde_json::to_string(&merge_payload_with_metadata(
            item.payload_json.clone(),
            &metadata,
        ))
        .map_err(|err| {
            AppError::invalid_apply_payload(
                format!(
                    "payload serialization failed for item {}: {err}",
                    item.item_id
                ),
                None,
            )
        })?;
        let status_str = match item.status {
            IncomingStatus::Accepted => "accepted",
            IncomingStatus::Rejected => "rejected",
            IncomingStatus::Conflict => "conflict",
        };
        let is_active = if item.status == IncomingStatus::Accepted {
            1
        } else {
            0
        };
        let agent_run_id = item
            .agent_run_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(default_agent_run_id);

        tx.execute(
            "insert into item_derivations(
                item_id,
                derivation_version,
                status,
                is_active,
                base_derivation_id,
                derivation_hash,
                agent_run_id,
                summary,
                category,
                priority,
                due_at,
                normalized_text,
                confidence,
                payload_json,
                conflict_reason
            ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                item.item_id,
                next_version,
                status_str,
                is_active,
                item.base_derivation_id,
                item.derivation_hash,
                agent_run_id,
                item.summary,
                item.category,
                item.priority,
                item.due_at,
                item.normalized_text,
                item.confidence,
                payload_json,
                item.conflict_reason,
            ],
        )
        .map_err(|err| {
            if let rusqlite::Error::SqliteFailure(native, _) = &err
                && native.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
            {
                return AppError::runtime("duplicate derivation hash for item")
                    .with_code("apply-item-conflict");
            }
            AppError::db_write(err)
        })?;
        let derivation_id = tx.last_insert_rowid();

        if item.status == IncomingStatus::Accepted {
            let tag_list = tags_with_metadata(&item.tags, &metadata);
            attach_tags(tx, derivation_id, &tag_list)?;
        }

        accepted += 1;
        outcomes.push(build_outcome(
            item.item_id,
            "accepted",
            Some(next_version),
            &metadata,
            None,
        ));
    }

    Ok(ApplySummary {
        dry_run,
        processed: items.len() as i64,
        accepted,
        skipped,
        failed,
        items: outcomes,
        changes,
    })
}

#[derive(Debug, Clone)]
struct ActiveDerivationSnapshot {
    status: String,
    summary: Option<String>,
    category: Option<String>,
    priority: Option<String>,
    due_at: Option<String>,
    normalized_text: Option<String>,
    confidence: Option<f64>,
    content_type: Option<String>,
    validation_status: Option<String>,
    tags: Vec<String>,
}

fn collect_dry_run_changes(
    tx: &Transaction<'_>,
    item: &ApplyInputItem,
    metadata: &ResolvedMetadata,
) -> Result<Vec<ApplyChange>, AppError> {
    let active = current_active_snapshot(tx, item.item_id)?;
    let active_ref = active.as_ref();
    let mut changes = Vec::new();
    let next_tags = if item.status == IncomingStatus::Accepted {
        tags_with_metadata(&item.tags, metadata)
    } else {
        Vec::new()
    };

    push_change(
        &mut changes,
        item.item_id,
        "status",
        active_ref.map(|snapshot| json!(snapshot.status)),
        json!(status_label(item.status)),
    );
    push_change(
        &mut changes,
        item.item_id,
        "summary",
        active_ref.map(|snapshot| option_string_value(snapshot.summary.as_deref())),
        option_string_value(item.summary.as_deref()),
    );
    push_change(
        &mut changes,
        item.item_id,
        "category",
        active_ref.map(|snapshot| option_string_value(snapshot.category.as_deref())),
        option_string_value(item.category.as_deref()),
    );
    push_change(
        &mut changes,
        item.item_id,
        "priority",
        active_ref.map(|snapshot| option_string_value(snapshot.priority.as_deref())),
        option_string_value(item.priority.as_deref()),
    );
    push_change(
        &mut changes,
        item.item_id,
        "due_at",
        active_ref.map(|snapshot| option_string_value(snapshot.due_at.as_deref())),
        option_string_value(item.due_at.as_deref()),
    );
    push_change(
        &mut changes,
        item.item_id,
        "normalized_text",
        active_ref.map(|snapshot| option_string_value(snapshot.normalized_text.as_deref())),
        option_string_value(item.normalized_text.as_deref()),
    );
    push_change(
        &mut changes,
        item.item_id,
        "confidence",
        active_ref.map(|snapshot| option_f64_value(snapshot.confidence)),
        option_f64_value(item.confidence),
    );
    push_change(
        &mut changes,
        item.item_id,
        "content_type",
        active_ref.map(|snapshot| option_string_value(snapshot.content_type.as_deref())),
        option_string_value(metadata.content_type.as_deref()),
    );
    push_change(
        &mut changes,
        item.item_id,
        "validation_status",
        active_ref.map(|snapshot| option_string_value(snapshot.validation_status.as_deref())),
        option_string_value(metadata.validation_status.as_deref()),
    );
    push_change(
        &mut changes,
        item.item_id,
        "tags",
        active_ref.map(|snapshot| json!(snapshot.tags)),
        json!(next_tags),
    );

    Ok(changes)
}

fn push_change(
    changes: &mut Vec<ApplyChange>,
    item_id: i64,
    field: &str,
    old: Option<Value>,
    new: Value,
) {
    if old.as_ref() == Some(&new) {
        return;
    }
    changes.push(ApplyChange {
        item_id,
        field: field.to_string(),
        old,
        new,
    });
}

fn option_string_value(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |value| json!(value))
}

fn option_f64_value(value: Option<f64>) -> Value {
    value.map_or(Value::Null, |value| json!(value))
}

fn status_label(status: IncomingStatus) -> &'static str {
    match status {
        IncomingStatus::Accepted => "accepted",
        IncomingStatus::Rejected => "rejected",
        IncomingStatus::Conflict => "conflict",
    }
}

fn build_outcome(
    item_id: i64,
    status: &str,
    derivation_version: Option<i64>,
    metadata: &ResolvedMetadata,
    error: Option<ApplyItemError>,
) -> ApplyItemOutcome {
    ApplyItemOutcome {
        item_id,
        status: status.to_string(),
        derivation_version,
        content_type: metadata.content_type.clone(),
        validation_status: metadata.validation_status.clone(),
        validation_errors: metadata.validation_errors.clone(),
        error,
    }
}

fn populate_missing_metadata(
    tx: &Transaction<'_>,
    item_id: i64,
    metadata: &mut ResolvedMetadata,
) -> Result<(), AppError> {
    let needs_content_type = metadata.content_type.is_none();
    let needs_status = metadata.validation_status.is_none();
    let needs_invalid_errors = matches!(metadata.validation_status.as_deref(), Some("invalid"))
        && metadata.validation_errors.is_none();

    if !needs_content_type && !needs_status && !needs_invalid_errors {
        return Ok(());
    }

    let raw_text: String = tx
        .query_row(
            "select raw_text from inbox_items where item_id = ?1",
            params![item_id],
            |row| row.get(0),
        )
        .map_err(AppError::db_query)?;
    let analyzed = preprocess::analyze(&raw_text);

    if needs_content_type {
        metadata.content_type = Some(analyzed.content_type.as_str().to_string());
    }
    if needs_status {
        metadata.validation_status = Some(analyzed.validation.status.as_str().to_string());
    }

    let invalid_from_analysis = matches!(analyzed.validation.status, ValidationStatus::Invalid);
    if (metadata.validation_errors.is_none() && invalid_from_analysis) || needs_invalid_errors {
        metadata.validation_errors = analyzed.validation.errors.map(|errors| {
            errors
                .into_iter()
                .map(|err| ApplyValidationError {
                    code: err.code,
                    message: err.message,
                    path: err.path,
                })
                .collect::<Vec<_>>()
        });
    }

    Ok(())
}

fn tags_with_metadata(base_tags: &[String], metadata: &ResolvedMetadata) -> Vec<String> {
    let mut tags = base_tags.to_vec();
    if let Some(content_type) = &metadata.content_type {
        tags.push(format!("fmt:{content_type}"));
    }
    if let Some(validation_status) = &metadata.validation_status {
        tags.push(format!("val:{validation_status}"));
    }
    tags
}

fn merge_payload_with_metadata(
    payload_json: serde_json::Value,
    metadata: &ResolvedMetadata,
) -> serde_json::Value {
    let mut map = match payload_json {
        serde_json::Value::Object(object) => object,
        other => {
            let mut object = serde_json::Map::new();
            object.insert("payload".to_string(), other);
            object
        }
    };

    if let Some(content_type) = &metadata.content_type {
        map.insert(
            "content_type".to_string(),
            serde_json::Value::String(content_type.clone()),
        );
    }
    if let Some(validation_status) = &metadata.validation_status {
        map.insert(
            "validation_status".to_string(),
            serde_json::Value::String(validation_status.clone()),
        );
    }
    if let Some(validation_errors) = &metadata.validation_errors
        && let Ok(encoded) = serde_json::to_value(validation_errors)
    {
        map.insert("validation_errors".to_string(), encoded);
    }

    serde_json::Value::Object(map)
}

fn item_exists(tx: &Transaction<'_>, item_id: i64) -> Result<bool, AppError> {
    let count: i64 = tx
        .query_row(
            "select count(*) from inbox_items where item_id = ?1",
            params![item_id],
            |row| row.get(0),
        )
        .map_err(AppError::db_query)?;
    Ok(count > 0)
}

fn current_active(tx: &Transaction<'_>, item_id: i64) -> Result<Option<(i64, i64)>, AppError> {
    tx.query_row(
        "select derivation_id, derivation_version
         from item_derivations
         where item_id = ?1 and is_active = 1 and status = 'accepted'
         order by derivation_version desc, derivation_id desc
         limit 1",
        params![item_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .map(Some)
    .or_else(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(AppError::db_query(other)),
    })
}

fn current_active_snapshot(
    tx: &Transaction<'_>,
    item_id: i64,
) -> Result<Option<ActiveDerivationSnapshot>, AppError> {
    let row = tx
        .query_row(
            "select derivation_id, status, summary, category, priority, due_at, normalized_text,
                    confidence, payload_json
             from item_derivations
             where item_id = ?1 and is_active = 1 and status = 'accepted'
             order by derivation_version desc, derivation_id desc
             limit 1",
            params![item_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<f64>>(7)?,
                    row.get::<_, String>(8)?,
                ))
            },
        )
        .map(Some)
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(AppError::db_query(other)),
        })?;

    let Some((
        derivation_id,
        status,
        summary,
        category,
        priority,
        due_at,
        normalized_text,
        confidence,
        payload_json,
    )) = row
    else {
        return Ok(None);
    };

    let payload: Value = serde_json::from_str(&payload_json).unwrap_or(Value::Null);
    let content_type = payload
        .get("content_type")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let validation_status = payload
        .get("validation_status")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let tags = active_tags(tx, derivation_id)?;

    Ok(Some(ActiveDerivationSnapshot {
        status,
        summary,
        category,
        priority,
        due_at,
        normalized_text,
        confidence,
        content_type,
        validation_status,
        tags,
    }))
}

fn active_tags(tx: &Transaction<'_>, derivation_id: i64) -> Result<Vec<String>, AppError> {
    let mut stmt = tx
        .prepare(
            "select t.tag_name
             from item_tags it
             join tags t on t.tag_id = it.tag_id
             where it.derivation_id = ?1
             order by t.tag_name_norm",
        )
        .map_err(AppError::db_query)?;
    let rows = stmt
        .query_map(params![derivation_id], |row| row.get(0))
        .map_err(AppError::db_query)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(AppError::db_query)
}

fn derivation_version_by_hash(
    tx: &Transaction<'_>,
    item_id: i64,
    derivation_hash: &str,
) -> Result<Option<i64>, AppError> {
    tx.query_row(
        "select derivation_version
         from item_derivations
         where item_id = ?1 and derivation_hash = ?2
         limit 1",
        params![item_id, derivation_hash],
        |row| row.get(0),
    )
    .map(Some)
    .or_else(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(AppError::db_query(other)),
    })
}

fn next_derivation_version(tx: &Transaction<'_>, item_id: i64) -> Result<i64, AppError> {
    let next_version = tx
        .query_row(
            "select coalesce(max(derivation_version), 0) + 1
             from item_derivations
             where item_id = ?1",
            params![item_id],
            |row| row.get(0),
        )
        .map_err(AppError::db_query)?;
    Ok(next_version)
}

fn attach_tags(tx: &Transaction<'_>, derivation_id: i64, tags: &[String]) -> Result<(), AppError> {
    let mut seen = HashSet::new();
    for tag in tags {
        let trimmed = tag.trim();
        if trimmed.is_empty() {
            continue;
        }
        let normalized = trimmed.to_lowercase();
        if !seen.insert(normalized.clone()) {
            continue;
        }

        tx.execute(
            "insert into tags(tag_name, tag_name_norm)
             values(?1, ?2)
             on conflict(tag_name_norm) do update set tag_name = excluded.tag_name",
            params![trimmed, normalized],
        )
        .map_err(AppError::db_write)?;

        let tag_id: i64 = tx
            .query_row(
                "select tag_id from tags where tag_name_norm = ?1",
                params![normalized],
                |row| row.get(0),
            )
            .map_err(AppError::db_query)?;

        tx.execute(
            "insert or ignore into item_tags(derivation_id, tag_id) values (?1, ?2)",
            params![derivation_id, tag_id],
        )
        .map_err(AppError::db_write)?;
    }

    Ok(())
}
