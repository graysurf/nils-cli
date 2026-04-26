use std::fs;
use std::path::{Path, PathBuf};

use toml::Value;

use crate::env::ResolvedRoots;
use crate::model::{
    ConfigDocumentEntry, ConfigErrorLocation, ConfigLoadError, ConfigScopeFile, Context,
    DocumentWhen, LoadedConfigs, Scope,
};

pub const CONFIG_FILE_NAME: &str = "AGENT_DOCS.toml";

const ALLOWED_DOCUMENT_FIELDS: [&str; 6] =
    ["context", "scope", "path", "required", "when", "notes"];

pub fn config_path_for_root(root: &Path) -> PathBuf {
    root.join(CONFIG_FILE_NAME)
}

pub fn load_configs_from_roots(roots: &ResolvedRoots) -> Result<LoadedConfigs, ConfigLoadError> {
    load_configs(&roots.docs_home, &roots.project_path)
}

pub fn load_configs(
    docs_home: &Path,
    project_path: &Path,
) -> Result<LoadedConfigs, ConfigLoadError> {
    let home = load_scope_config(Scope::Home, docs_home)?;
    let project = load_scope_config(Scope::Project, project_path)?;
    Ok(LoadedConfigs { home, project })
}

pub fn load_scope_config(
    source_scope: Scope,
    root: &Path,
) -> Result<Option<ConfigScopeFile>, ConfigLoadError> {
    let file_path = config_path_for_root(root);
    if !file_path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(&file_path).map_err(|err| {
        ConfigLoadError::io(
            file_path.clone(),
            format!("failed to read {}: {err}", CONFIG_FILE_NAME),
        )
    })?;
    let parsed = parse_toml(&file_path, &raw)?;
    let documents = parse_documents(&file_path, &parsed)?;

    Ok(Some(ConfigScopeFile {
        source_scope,
        root: root.to_path_buf(),
        file_path,
        documents,
    }))
}

fn parse_toml(file_path: &Path, raw: &str) -> Result<Value, ConfigLoadError> {
    raw.parse::<toml::Table>()
        .map(Value::Table)
        .map_err(|err| parse_error(file_path, raw, &err))
}

fn parse_documents(
    file_path: &Path,
    parsed: &Value,
) -> Result<Vec<ConfigDocumentEntry>, ConfigLoadError> {
    let Some(root_table) = parsed.as_table() else {
        return Err(ConfigLoadError::validation_root(
            file_path.to_path_buf(),
            "document",
            "root TOML value must be a table",
        ));
    };

    let Some(raw_documents) = root_table.get("document") else {
        return Ok(Vec::new());
    };
    let Some(raw_documents) = raw_documents.as_array() else {
        return Err(ConfigLoadError::validation_root(
            file_path.to_path_buf(),
            "document",
            "key `document` must be an array of [[document]] tables",
        ));
    };

    let mut documents = Vec::with_capacity(raw_documents.len());
    for (index, raw_document) in raw_documents.iter().enumerate() {
        let Some(table) = raw_document.as_table() else {
            return Err(ConfigLoadError::validation(
                file_path.to_path_buf(),
                index,
                "document",
                "entry must be a TOML table declared with [[document]]",
            ));
        };

        validate_unknown_fields(file_path, index, table)?;
        let context = parse_context(file_path, index, table)?;
        let scope = parse_scope(file_path, index, table)?;
        let path = parse_path(file_path, index, table)?;
        let required = parse_required(file_path, index, table)?;
        let when = parse_when(file_path, index, table)?;
        let notes = parse_notes(file_path, index, table)?;

        documents.push(ConfigDocumentEntry {
            context,
            scope,
            path,
            required,
            when,
            notes,
        });
    }

    Ok(documents)
}

fn validate_unknown_fields(
    file_path: &Path,
    index: usize,
    table: &toml::map::Map<String, Value>,
) -> Result<(), ConfigLoadError> {
    for key in table.keys() {
        if !ALLOWED_DOCUMENT_FIELDS.contains(&key.as_str()) {
            return Err(ConfigLoadError::validation(
                file_path.to_path_buf(),
                index,
                key,
                format!(
                    "unsupported field `{key}`; allowed fields: {}",
                    ALLOWED_DOCUMENT_FIELDS.join(", ")
                ),
            ));
        }
    }
    Ok(())
}

fn parse_context(
    file_path: &Path,
    index: usize,
    table: &toml::map::Map<String, Value>,
) -> Result<Context, ConfigLoadError> {
    let raw = required_string(file_path, index, table, "context")?;
    Context::from_config_value(raw).ok_or_else(|| {
        ConfigLoadError::validation(
            file_path.to_path_buf(),
            index,
            "context",
            format!(
                "unsupported context `{raw}`; allowed: {}",
                Context::supported_values().join(", ")
            ),
        )
    })
}

fn parse_scope(
    file_path: &Path,
    index: usize,
    table: &toml::map::Map<String, Value>,
) -> Result<Scope, ConfigLoadError> {
    let raw = required_string(file_path, index, table, "scope")?;
    Scope::from_config_value(raw).ok_or_else(|| {
        ConfigLoadError::validation(
            file_path.to_path_buf(),
            index,
            "scope",
            format!(
                "unsupported scope `{raw}`; allowed: {}",
                Scope::supported_values().join(", ")
            ),
        )
    })
}

fn parse_path(
    file_path: &Path,
    index: usize,
    table: &toml::map::Map<String, Value>,
) -> Result<PathBuf, ConfigLoadError> {
    let raw = required_string(file_path, index, table, "path")?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ConfigLoadError::validation(
            file_path.to_path_buf(),
            index,
            "path",
            "path cannot be empty",
        ));
    }
    Ok(PathBuf::from(trimmed))
}

fn parse_required(
    file_path: &Path,
    index: usize,
    table: &toml::map::Map<String, Value>,
) -> Result<bool, ConfigLoadError> {
    let Some(value) = table.get("required") else {
        return Ok(false);
    };
    let Some(required) = value.as_bool() else {
        return Err(ConfigLoadError::validation(
            file_path.to_path_buf(),
            index,
            "required",
            format!(
                "invalid type for `required`: expected boolean, found {}",
                value_type(value)
            ),
        ));
    };
    Ok(required)
}

fn parse_when(
    file_path: &Path,
    index: usize,
    table: &toml::map::Map<String, Value>,
) -> Result<DocumentWhen, ConfigLoadError> {
    let when_value = match table.get("when") {
        Some(value) => {
            let Some(value) = value.as_str() else {
                return Err(ConfigLoadError::validation(
                    file_path.to_path_buf(),
                    index,
                    "when",
                    format!(
                        "invalid type for `when`: expected string, found {}",
                        value_type(value)
                    ),
                ));
            };
            value
        }
        None => "always",
    };

    DocumentWhen::from_config_value(when_value).ok_or_else(|| {
        ConfigLoadError::validation(
            file_path.to_path_buf(),
            index,
            "when",
            format!(
                "unsupported when value `{when_value}`; allowed: {}",
                DocumentWhen::supported_values().join(", ")
            ),
        )
    })
}

fn parse_notes(
    file_path: &Path,
    index: usize,
    table: &toml::map::Map<String, Value>,
) -> Result<Option<String>, ConfigLoadError> {
    let Some(value) = table.get("notes") else {
        return Ok(None);
    };
    let Some(notes) = value.as_str() else {
        return Err(ConfigLoadError::validation(
            file_path.to_path_buf(),
            index,
            "notes",
            format!(
                "invalid type for `notes`: expected string, found {}",
                value_type(value)
            ),
        ));
    };
    Ok(Some(notes.to_string()))
}

fn required_string<'a>(
    file_path: &Path,
    index: usize,
    table: &'a toml::map::Map<String, Value>,
    field: &'static str,
) -> Result<&'a str, ConfigLoadError> {
    let Some(value) = table.get(field) else {
        return Err(ConfigLoadError::validation(
            file_path.to_path_buf(),
            index,
            field,
            format!("missing required field `{field}`"),
        ));
    };
    let Some(value) = value.as_str() else {
        return Err(ConfigLoadError::validation(
            file_path.to_path_buf(),
            index,
            field,
            format!(
                "invalid type for `{field}`: expected string, found {}",
                value_type(value)
            ),
        ));
    };
    Ok(value)
}

fn value_type(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "string",
        Value::Integer(_) => "integer",
        Value::Float(_) => "float",
        Value::Boolean(_) => "boolean",
        Value::Datetime(_) => "datetime",
        Value::Array(_) => "array",
        Value::Table(_) => "table",
    }
}

fn parse_error(file_path: &Path, raw: &str, err: &toml::de::Error) -> ConfigLoadError {
    let location = err
        .span()
        .map(|span| byte_offset_to_line_column(raw, span.start));

    ConfigLoadError::parse(
        file_path.to_path_buf(),
        format!("invalid TOML in {CONFIG_FILE_NAME}: {err}"),
        location,
    )
}

fn byte_offset_to_line_column(raw: &str, offset: usize) -> ConfigErrorLocation {
    let mut line = 1usize;
    let mut column = 1usize;
    let clamped = offset.min(raw.len());
    for (idx, ch) in raw.char_indices() {
        if idx >= clamped {
            break;
        }
        if ch == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    ConfigErrorLocation { line, column }
}
