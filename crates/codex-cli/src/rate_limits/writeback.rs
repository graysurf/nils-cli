use anyhow::{Context, Result};
use chrono::{TimeZone, Utc};
use nils_common::fs;
use serde_json::{Map, Value};
use std::path::Path;

use crate::json;
use crate::rate_limits::render;

pub fn write_weekly(target_file: &Path, usage_json: &Value) -> Result<()> {
    if !target_file.is_file() {
        anyhow::bail!("target file not found");
    }

    let usage = match render::parse_usage(usage_json) {
        Some(value) => value,
        None => return Ok(()),
    };
    let values = render::render_values(&usage);
    let weekly = render::weekly_values(&values);
    if weekly
        .weekly
        .as_ref()
        .is_some_and(|window| window.reset_epoch <= 0)
    {
        return Ok(());
    }
    let weekly_reset_epoch = weekly
        .weekly
        .as_ref()
        .map(|window| window.reset_epoch)
        .filter(|epoch| *epoch > 0);
    let non_weekly_reset_epoch = weekly
        .non_weekly
        .as_ref()
        .map(|window| window.reset_epoch)
        .filter(|epoch| *epoch > 0);
    let weekly_reset_iso = weekly_reset_epoch.and_then(|epoch| epoch_to_iso(epoch).ok());
    let non_weekly_reset_iso = non_weekly_reset_epoch.and_then(|epoch| epoch_to_iso(epoch).ok());

    let fetched_at_iso = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let mut root = json::read_json(target_file).unwrap_or_else(|_| Value::Object(Map::new()));
    let root_obj = root
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("root not object"))?;

    let mut codex_rate_limits = root_obj
        .get("codex_rate_limits")
        .and_then(|value| value.as_object())
        .cloned()
        .unwrap_or_else(Map::new);

    match (weekly_reset_epoch, weekly_reset_iso) {
        (Some(epoch), Some(iso)) => {
            codex_rate_limits.insert("weekly_reset_at".to_string(), Value::String(iso));
            codex_rate_limits.insert(
                "weekly_reset_at_epoch".to_string(),
                Value::Number(epoch.into()),
            );
            codex_rate_limits.insert(
                "weekly_fetched_at".to_string(),
                Value::String(fetched_at_iso),
            );
        }
        _ => {
            codex_rate_limits.remove("weekly_reset_at");
            codex_rate_limits.remove("weekly_reset_at_epoch");
            codex_rate_limits.remove("weekly_fetched_at");
        }
    }

    match (non_weekly_reset_epoch, non_weekly_reset_iso) {
        (Some(epoch), Some(iso)) => {
            codex_rate_limits.insert("non_weekly_reset_at".to_string(), Value::String(iso));
            codex_rate_limits.insert(
                "non_weekly_reset_at_epoch".to_string(),
                Value::Number(epoch.into()),
            );
        }
        _ => {
            codex_rate_limits.remove("non_weekly_reset_at");
            codex_rate_limits.remove("non_weekly_reset_at_epoch");
        }
    }

    root_obj.insert(
        "codex_rate_limits".to_string(),
        Value::Object(codex_rate_limits),
    );

    let out = serde_json::to_vec(&root).context("serialize writeback")?;
    fs::write_atomic(target_file, &out, fs::SECRET_FILE_MODE)?;

    Ok(())
}

fn epoch_to_iso(epoch: i64) -> Result<String> {
    if epoch <= 0 {
        anyhow::bail!("invalid epoch");
    }
    Ok(Utc
        .timestamp_opt(epoch, 0)
        .single()
        .context("epoch")?
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::{epoch_to_iso, write_weekly};
    use serde_json::{Value, json};
    use std::fs;
    use std::path::Path;

    fn write_json(path: &Path, value: &Value) {
        let bytes = serde_json::to_vec(value).expect("serialize");
        fs::write(path, bytes).expect("write json");
    }

    fn read_json(path: &Path) -> Value {
        let bytes = fs::read(path).expect("read json");
        serde_json::from_slice(&bytes).expect("parse json")
    }

    fn usage_with_weekly_secondary() -> Value {
        json!({
            "rate_limit": {
                "primary_window": {
                    "limit_window_seconds": 18000,
                    "used_percent": 6.0,
                    "reset_at": 1700003600
                },
                "secondary_window": {
                    "limit_window_seconds": 604800,
                    "used_percent": 12.0,
                    "reset_at": 1700600000
                }
            }
        })
    }

    #[test]
    fn write_weekly_uses_primary_window_when_primary_is_weekly() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let target = dir.path().join("alpha.json");
        write_json(&target, &json!({ "tokens": { "access_token": "tok" } }));

        let usage = json!({
            "rate_limit": {
                "primary_window": {
                    "limit_window_seconds": 604800,
                    "used_percent": 12.0,
                    "reset_at": 1700700000
                },
                "secondary_window": {
                    "limit_window_seconds": 18000,
                    "used_percent": 6.0,
                    "reset_at": 1700003600
                }
            }
        });

        write_weekly(&target, &usage).expect("write weekly");
        let written = read_json(&target);
        let limits = &written["codex_rate_limits"];

        assert_eq!(limits["weekly_reset_at_epoch"].as_i64(), Some(1700700000));
        assert_eq!(
            limits["weekly_reset_at"].as_str(),
            Some(epoch_to_iso(1700700000).expect("weekly iso").as_str())
        );
        assert_eq!(
            limits["non_weekly_reset_at_epoch"].as_i64(),
            Some(1700003600)
        );
        assert_eq!(
            limits["non_weekly_reset_at"].as_str(),
            Some(epoch_to_iso(1700003600).expect("non-weekly iso").as_str())
        );
        assert!(limits["weekly_fetched_at"].as_str().is_some());
    }

    #[test]
    fn write_weekly_preserves_existing_codex_rate_limits_fields() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let target = dir.path().join("alpha.json");
        write_json(
            &target,
            &json!({
                "tokens": { "access_token": "tok" },
                "codex_rate_limits": {
                    "source": "existing-metadata",
                    "weekly_reset_at_epoch": 111
                }
            }),
        );

        let usage = json!({
            "rate_limit": {
                "primary_window": {
                    "limit_window_seconds": 18000,
                    "used_percent": 6.0,
                    "reset_at": 1700003600
                },
                "secondary_window": {
                    "limit_window_seconds": 604800,
                    "used_percent": 12.0,
                    "reset_at": 1700600000
                }
            }
        });

        write_weekly(&target, &usage).expect("write weekly");
        let written = read_json(&target);
        let limits = &written["codex_rate_limits"];

        assert_eq!(limits["source"].as_str(), Some("existing-metadata"));
        assert_eq!(limits["weekly_reset_at_epoch"].as_i64(), Some(1700600000));
        assert_eq!(
            limits["non_weekly_reset_at_epoch"].as_i64(),
            Some(1700003600)
        );
    }

    #[test]
    fn write_weekly_skips_write_when_weekly_epoch_is_non_positive() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let target = dir.path().join("alpha.json");
        write_json(
            &target,
            &json!({
                "tokens": { "access_token": "tok" },
                "codex_rate_limits": {
                    "weekly_reset_at_epoch": 111,
                    "weekly_reset_at": "existing"
                }
            }),
        );
        let before = read_json(&target);

        let usage = json!({
            "rate_limit": {
                "primary_window": {
                    "limit_window_seconds": 18000,
                    "used_percent": 6.0,
                    "reset_at": 1700003600
                },
                "secondary_window": {
                    "limit_window_seconds": 604800,
                    "used_percent": 12.0,
                    "reset_at": 0
                }
            }
        });

        write_weekly(&target, &usage).expect("write weekly");
        let after = read_json(&target);
        assert_eq!(after, before);
    }

    #[test]
    fn write_weekly_clears_non_weekly_fields_when_non_weekly_epoch_is_non_positive() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let target = dir.path().join("alpha.json");
        write_json(
            &target,
            &json!({
                "tokens": { "access_token": "tok" },
                "codex_rate_limits": {
                    "source": "existing-metadata",
                    "non_weekly_reset_at_epoch": 1700003600,
                    "non_weekly_reset_at": "2023-11-14T23:13:20Z"
                }
            }),
        );

        let usage = json!({
            "rate_limit": {
                "primary_window": {
                    "limit_window_seconds": 18000,
                    "used_percent": 6.0,
                    "reset_at": 0
                },
                "secondary_window": {
                    "limit_window_seconds": 604800,
                    "used_percent": 12.0,
                    "reset_at": 1700600000
                }
            }
        });

        write_weekly(&target, &usage).expect("write weekly");
        let written = read_json(&target);
        let limits = written["codex_rate_limits"]
            .as_object()
            .expect("limits object");

        assert_eq!(
            limits.get("source").and_then(Value::as_str),
            Some("existing-metadata")
        );
        assert_eq!(
            limits.get("weekly_reset_at_epoch").and_then(Value::as_i64),
            Some(1700600000)
        );
        assert!(
            limits
                .get("weekly_reset_at")
                .and_then(Value::as_str)
                .is_some()
        );
        assert!(
            limits
                .get("weekly_fetched_at")
                .and_then(Value::as_str)
                .is_some()
        );
        assert!(!limits.contains_key("non_weekly_reset_at"));
        assert!(!limits.contains_key("non_weekly_reset_at_epoch"));
    }

    #[test]
    fn write_weekly_fails_when_target_file_is_missing() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let target = dir.path().join("missing.json");
        let usage = json!({
            "rate_limit": {
                "primary_window": {
                    "limit_window_seconds": 18000,
                    "used_percent": 6.0,
                    "reset_at": 1700003600
                },
                "secondary_window": {
                    "limit_window_seconds": 604800,
                    "used_percent": 12.0,
                    "reset_at": 1700600000
                }
            }
        });

        let err = write_weekly(&target, &usage).expect_err("missing target must fail");
        assert!(err.to_string().contains("target file not found"));
    }

    #[test]
    fn write_weekly_noops_when_usage_payload_is_unparseable() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let target = dir.path().join("alpha.json");
        write_json(
            &target,
            &json!({
                "tokens": { "access_token": "tok" },
                "codex_rate_limits": {
                    "weekly_reset_at_epoch": 111,
                    "weekly_reset_at": "existing"
                }
            }),
        );
        let before = read_json(&target);

        write_weekly(&target, &json!({ "unexpected": "shape" })).expect("write weekly");
        let after = read_json(&target);

        assert_eq!(after, before);
    }

    #[test]
    fn write_weekly_recovers_from_malformed_existing_json() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let target = dir.path().join("alpha.json");
        fs::write(&target, b"{ malformed").expect("write malformed json");

        write_weekly(&target, &usage_with_weekly_secondary()).expect("write weekly");
        let written = read_json(&target);
        let limits = written["codex_rate_limits"]
            .as_object()
            .expect("limits object");

        assert_eq!(
            limits.get("weekly_reset_at_epoch").and_then(Value::as_i64),
            Some(1700600000)
        );
        assert_eq!(
            limits
                .get("non_weekly_reset_at_epoch")
                .and_then(Value::as_i64),
            Some(1700003600)
        );
        assert!(
            limits
                .get("weekly_fetched_at")
                .and_then(Value::as_str)
                .is_some()
        );
    }

    #[test]
    fn write_weekly_fails_when_existing_json_root_is_not_object() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let target = dir.path().join("alpha.json");
        write_json(&target, &json!(["not", "an", "object"]));

        let err = write_weekly(&target, &usage_with_weekly_secondary())
            .expect_err("non-object root should fail");

        assert!(err.to_string().contains("root not object"));
    }

    #[test]
    fn write_weekly_replaces_non_object_codex_rate_limits_value() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let target = dir.path().join("alpha.json");
        write_json(
            &target,
            &json!({
                "tokens": { "access_token": "tok" },
                "codex_rate_limits": "existing-string"
            }),
        );

        write_weekly(&target, &usage_with_weekly_secondary()).expect("write weekly");
        let written = read_json(&target);

        assert_eq!(written["tokens"]["access_token"].as_str(), Some("tok"));
        assert!(written["codex_rate_limits"].is_object());
        assert_eq!(
            written["codex_rate_limits"]["weekly_reset_at_epoch"].as_i64(),
            Some(1700600000)
        );
    }
}
