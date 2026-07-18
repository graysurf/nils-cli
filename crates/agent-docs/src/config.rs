use std::fs;
use std::path::{Path, PathBuf};

use toml::Value;

use crate::env::ResolvedRoots;
use crate::model::{
    CatalogOrigin, ConfigErrorLocation, ConfigLoadError, Context, DocumentEntry, LoadedCatalog,
    Product, Scope, ScopeCatalog, SkillPolicy, ValidationEntry, When,
};
use crate::path_classes::PathClassContract;
use crate::predicate::parse_when;

pub const CONFIG_FILE_NAME: &str = "AGENT_DOCS.toml";

const ALLOWED_DOCUMENT_FIELDS: [&str; 9] = [
    "context",
    "scope",
    "path",
    "product",
    "required",
    "when",
    "marker",
    "last-reviewed-within-days",
    "notes",
];

const ALLOWED_VALIDATION_FIELDS: [&str; 5] =
    ["context", "commands", "product", "marker", "description"];

const ALLOWED_SKILLS_FIELDS: [&str; 3] = ["enforce_name_prefix", "allowed_prefixes", "dir"];

pub fn config_path_for_root(root: &Path) -> PathBuf {
    root.join(CONFIG_FILE_NAME)
}

pub fn load_catalog_from_roots(roots: &ResolvedRoots) -> Result<LoadedCatalog, ConfigLoadError> {
    load_catalog(&roots.docs_home, &roots.project_path)
}

pub fn load_catalog(
    docs_home: &Path,
    project_path: &Path,
) -> Result<LoadedCatalog, ConfigLoadError> {
    let home = load_scope_catalog(Scope::Home, docs_home)?;

    // When the docs-home and project share the same catalog file (a repo that
    // is its own docs-home, e.g. the kit itself), load it once as the home
    // catalog and leave the project slot empty to avoid duplicate entries.
    if same_config_file(docs_home, project_path) {
        return Ok(LoadedCatalog {
            home,
            project: None,
        });
    }

    let project = load_scope_catalog(Scope::Project, project_path)?;
    Ok(LoadedCatalog { home, project })
}

pub fn load_scope_catalog(
    source_scope: Scope,
    root: &Path,
) -> Result<Option<ScopeCatalog>, ConfigLoadError> {
    let file_path = config_path_for_root(root);
    if !file_path.exists() {
        return Ok(None);
    }
    let origin = match source_scope {
        Scope::Home | Scope::Global => CatalogOrigin::Home,
        Scope::Project => CatalogOrigin::Repository,
    };
    load_catalog_file(source_scope, origin, root, &file_path).map(Some)
}

pub fn load_external_project_catalog(
    project_root: &Path,
    file_path: &Path,
) -> Result<ScopeCatalog, ConfigLoadError> {
    load_catalog_file(Scope::Project, CatalogOrigin::User, project_root, file_path)
}

fn load_catalog_file(
    source_scope: Scope,
    origin: CatalogOrigin,
    root: &Path,
    file_path: &Path,
) -> Result<ScopeCatalog, ConfigLoadError> {
    let raw = fs::read_to_string(file_path).map_err(|err| {
        ConfigLoadError::io(
            file_path.to_path_buf(),
            format!("failed to read {}: {err}", file_path.display()),
        )
    })?;
    load_scope_catalog_from_str(source_scope, origin, root, file_path, &raw)
}

pub(crate) fn load_scope_catalog_from_str(
    source_scope: Scope,
    origin: CatalogOrigin,
    root: &Path,
    file_path: &Path,
    raw: &str,
) -> Result<ScopeCatalog, ConfigLoadError> {
    let parsed = parse_toml(file_path, raw, origin)?;
    let documents = parse_documents(source_scope, origin, file_path, &parsed)?;
    let validations = parse_validations(file_path, &parsed)?;
    let skill_policy = parse_skill_policy(file_path, &parsed)?;
    let path_classes = parse_path_classes(source_scope, file_path, &parsed)?;

    Ok(ScopeCatalog {
        source_scope,
        root: root.to_path_buf(),
        file_path: file_path.to_path_buf(),
        documents,
        validations,
        skill_policy,
        path_classes,
    })
}

fn parse_path_classes(
    _source_scope: Scope,
    file_path: &Path,
    parsed: &Value,
) -> Result<Option<PathClassContract>, ConfigLoadError> {
    let Some(root) = parsed.as_table() else {
        return Ok(None);
    };
    let Some(value) = root.get("path_classes") else {
        return Ok(None);
    };
    let table = value.as_table().ok_or_else(|| {
        ConfigLoadError::validation_root(
            file_path.to_path_buf(),
            "path_classes",
            "key `path_classes` must be a [path_classes] table",
        )
    })?;
    PathClassContract::from_toml(table)
        .map(Some)
        .map_err(|message| {
            ConfigLoadError::validation_root(file_path.to_path_buf(), "path_classes", message)
        })
}

fn same_config_file(docs_home: &Path, project_path: &Path) -> bool {
    let home_config = config_path_for_root(docs_home);
    let project_config = config_path_for_root(project_path);
    let home = fs::canonicalize(&home_config).unwrap_or(home_config);
    let project = fs::canonicalize(&project_config).unwrap_or(project_config);
    home == project
}

fn parse_toml(
    file_path: &Path,
    raw: &str,
    origin: CatalogOrigin,
) -> Result<Value, ConfigLoadError> {
    raw.parse::<toml::Table>()
        .map(Value::Table)
        .map_err(|err| parse_error(file_path, raw, &err, origin))
}

fn array_of_tables<'a>(
    parsed: &'a Value,
    file_path: &Path,
    key: &str,
) -> Result<Option<&'a Vec<Value>>, ConfigLoadError> {
    let Some(root_table) = parsed.as_table() else {
        return Err(ConfigLoadError::validation_root(
            file_path.to_path_buf(),
            key,
            "root TOML value must be a table",
        ));
    };
    let Some(value) = root_table.get(key) else {
        return Ok(None);
    };
    let Some(array) = value.as_array() else {
        return Err(ConfigLoadError::validation_root(
            file_path.to_path_buf(),
            key,
            format!("key `{key}` must be an array of [[{key}]] tables"),
        ));
    };
    Ok(Some(array))
}

fn parse_documents(
    source_scope: Scope,
    origin: CatalogOrigin,
    file_path: &Path,
    parsed: &Value,
) -> Result<Vec<DocumentEntry>, ConfigLoadError> {
    let Some(raw_documents) = array_of_tables(parsed, file_path, "document")? else {
        return Ok(Vec::new());
    };

    let mut documents = Vec::with_capacity(raw_documents.len());
    for (index, raw_document) in raw_documents.iter().enumerate() {
        let Some(table) = raw_document.as_table() else {
            return Err(ConfigLoadError::validation(
                file_path.to_path_buf(),
                "document",
                index,
                "document",
                "entry must be a TOML table declared with [[document]]",
            ));
        };

        validate_unknown_fields(
            file_path,
            "document",
            index,
            table,
            &ALLOWED_DOCUMENT_FIELDS,
        )?;
        let context = parse_context(file_path, "document", index, table)?;
        let scope = parse_scope(file_path, index, table)?;
        validate_scope_for_source(source_scope, origin, scope, file_path, index)?;
        let path = parse_path(file_path, index, table, origin)?;
        let products = parse_products(file_path, "document", index, table)?;
        let required = parse_bool(file_path, index, table, "required")?.unwrap_or(false);
        let (when, when_raw) = parse_when_field(file_path, index, table)?;
        let marker = parse_opt_string(file_path, "document", index, table, "marker")?;
        let freshness_days = parse_u64(file_path, index, table, "last-reviewed-within-days")?;
        let notes = parse_opt_string(file_path, "document", index, table, "notes")?;

        documents.push(DocumentEntry {
            context,
            scope,
            path,
            products,
            required,
            when,
            when_raw,
            marker,
            freshness_days,
            notes,
        });
    }

    Ok(documents)
}

fn parse_validations(
    file_path: &Path,
    parsed: &Value,
) -> Result<Vec<ValidationEntry>, ConfigLoadError> {
    let Some(raw_validations) = array_of_tables(parsed, file_path, "validation")? else {
        return Ok(Vec::new());
    };

    let mut validations = Vec::with_capacity(raw_validations.len());
    for (index, raw_validation) in raw_validations.iter().enumerate() {
        let Some(table) = raw_validation.as_table() else {
            return Err(ConfigLoadError::validation(
                file_path.to_path_buf(),
                "validation",
                index,
                "validation",
                "entry must be a TOML table declared with [[validation]]",
            ));
        };

        validate_unknown_fields(
            file_path,
            "validation",
            index,
            table,
            &ALLOWED_VALIDATION_FIELDS,
        )?;
        let context = parse_context(file_path, "validation", index, table)?;
        let commands = parse_commands(file_path, index, table)?;
        let products = parse_products(file_path, "validation", index, table)?;
        let marker = parse_opt_string(file_path, "validation", index, table, "marker")?;
        let description = parse_opt_string(file_path, "validation", index, table, "description")?;

        validations.push(ValidationEntry {
            context,
            products,
            commands,
            marker,
            description,
        });
    }

    Ok(validations)
}

fn parse_products(
    file_path: &Path,
    section: &'static str,
    index: usize,
    table: &toml::map::Map<String, Value>,
) -> Result<Vec<Product>, ConfigLoadError> {
    let Some(value) = table.get("product") else {
        return Ok(Vec::new());
    };

    let mut products = match value {
        Value::String(raw) => vec![parse_product(file_path, section, index, raw)?],
        Value::Array(items) => {
            if items.is_empty() {
                return Err(ConfigLoadError::validation(
                    file_path.to_path_buf(),
                    section,
                    index,
                    "product",
                    "`product` must list at least one product when using array form",
                ));
            }
            let mut parsed = Vec::with_capacity(items.len());
            for item in items {
                let Some(raw) = item.as_str() else {
                    return Err(ConfigLoadError::validation(
                        file_path.to_path_buf(),
                        section,
                        index,
                        "product",
                        format!(
                            "invalid product entry: expected string, found {}",
                            value_type(item)
                        ),
                    ));
                };
                parsed.push(parse_product(file_path, section, index, raw)?);
            }
            parsed
        }
        other => {
            return Err(ConfigLoadError::validation(
                file_path.to_path_buf(),
                section,
                index,
                "product",
                format!(
                    "invalid type for `product`: expected string or array of strings, found {}",
                    value_type(other)
                ),
            ));
        }
    };
    products.sort();
    products.dedup();
    Ok(products)
}

fn parse_product(
    file_path: &Path,
    section: &'static str,
    index: usize,
    raw: &str,
) -> Result<Product, ConfigLoadError> {
    Product::from_config_value(raw.trim()).ok_or_else(|| {
        ConfigLoadError::validation(
            file_path.to_path_buf(),
            section,
            index,
            "product",
            format!(
                "unsupported product `{}`; allowed: {}",
                raw.trim(),
                Product::supported_values().join(", ")
            ),
        )
    })
}

fn parse_skill_policy(
    file_path: &Path,
    parsed: &Value,
) -> Result<Option<SkillPolicy>, ConfigLoadError> {
    let Some(root_table) = parsed.as_table() else {
        return Ok(None);
    };
    let Some(value) = root_table.get("skills") else {
        return Ok(None);
    };
    let Some(table) = value.as_table() else {
        return Err(ConfigLoadError::validation_root(
            file_path.to_path_buf(),
            "skills",
            "key `skills` must be a [skills] table",
        ));
    };

    for key in table.keys() {
        if !ALLOWED_SKILLS_FIELDS.contains(&key.as_str()) {
            return Err(ConfigLoadError::validation_root(
                file_path.to_path_buf(),
                format!("skills.{key}"),
                format!(
                    "unsupported field `{key}`; allowed fields: {}",
                    ALLOWED_SKILLS_FIELDS.join(", ")
                ),
            ));
        }
    }

    let enforce_name_prefix = match table.get("enforce_name_prefix") {
        None => false,
        Some(value) => value.as_bool().ok_or_else(|| {
            ConfigLoadError::validation_root(
                file_path.to_path_buf(),
                "skills.enforce_name_prefix",
                format!(
                    "invalid type for `enforce_name_prefix`: expected boolean, found {}",
                    value_type(value)
                ),
            )
        })?,
    };

    let allowed_prefixes = match table.get("allowed_prefixes") {
        None => SkillPolicy::default_prefixes(),
        Some(value) => parse_allowed_prefixes(file_path, value)?,
    };

    let dir = match table.get("dir") {
        None => SkillPolicy::DEFAULT_DIR.to_string(),
        Some(value) => {
            let Some(text) = value.as_str() else {
                return Err(ConfigLoadError::validation_root(
                    file_path.to_path_buf(),
                    "skills.dir",
                    format!(
                        "invalid type for `dir`: expected string, found {}",
                        value_type(value)
                    ),
                ));
            };
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return Err(ConfigLoadError::validation_root(
                    file_path.to_path_buf(),
                    "skills.dir",
                    "`dir` cannot be empty",
                ));
            }
            trimmed.to_string()
        }
    };

    Ok(Some(SkillPolicy {
        enforce_name_prefix,
        allowed_prefixes,
        dir,
    }))
}

fn parse_allowed_prefixes(file_path: &Path, value: &Value) -> Result<Vec<String>, ConfigLoadError> {
    let Some(array) = value.as_array() else {
        return Err(ConfigLoadError::validation_root(
            file_path.to_path_buf(),
            "skills.allowed_prefixes",
            format!(
                "invalid type for `allowed_prefixes`: expected array of strings, found {}",
                value_type(value)
            ),
        ));
    };
    if array.is_empty() {
        return Err(ConfigLoadError::validation_root(
            file_path.to_path_buf(),
            "skills.allowed_prefixes",
            "`allowed_prefixes` must list at least one prefix",
        ));
    }
    let mut prefixes = Vec::with_capacity(array.len());
    for item in array {
        let Some(text) = item.as_str() else {
            return Err(ConfigLoadError::validation_root(
                file_path.to_path_buf(),
                "skills.allowed_prefixes",
                format!(
                    "invalid prefix entry: expected string, found {}",
                    value_type(item)
                ),
            ));
        };
        let prefix = text.trim();
        if prefix.is_empty() {
            return Err(ConfigLoadError::validation_root(
                file_path.to_path_buf(),
                "skills.allowed_prefixes",
                "prefix entries cannot be empty",
            ));
        }
        if !prefix
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(ConfigLoadError::validation_root(
                file_path.to_path_buf(),
                "skills.allowed_prefixes",
                format!("prefix `{prefix}` must be lowercase kebab-case ([a-z0-9-])"),
            ));
        }
        prefixes.push(prefix.to_string());
    }
    Ok(prefixes)
}

fn validate_scope_for_source(
    source_scope: Scope,
    origin: CatalogOrigin,
    document_scope: Scope,
    file_path: &Path,
    index: usize,
) -> Result<(), ConfigLoadError> {
    if origin == CatalogOrigin::User && document_scope != Scope::Project {
        return Err(ConfigLoadError::validation(
            file_path.to_path_buf(),
            "document",
            index,
            "scope",
            "private/user catalog documents require project scope",
        ));
    }
    if source_scope == Scope::Project && document_scope == Scope::Global {
        return Err(ConfigLoadError::validation(
            file_path.to_path_buf(),
            "document",
            index,
            "scope",
            "global scope is allowed only in the home catalog; use scope = \"project\" for project-local requirements",
        ));
    }
    Ok(())
}

fn validate_unknown_fields(
    file_path: &Path,
    section: &'static str,
    index: usize,
    table: &toml::map::Map<String, Value>,
    allowed: &[&str],
) -> Result<(), ConfigLoadError> {
    for key in table.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(ConfigLoadError::validation(
                file_path.to_path_buf(),
                section,
                index,
                key,
                format!(
                    "unsupported field `{key}`; allowed fields: {}",
                    allowed.join(", ")
                ),
            ));
        }
    }
    Ok(())
}

fn parse_context(
    file_path: &Path,
    section: &'static str,
    index: usize,
    table: &toml::map::Map<String, Value>,
) -> Result<Context, ConfigLoadError> {
    let raw = required_string(file_path, section, index, table, "context")?;
    Context::parse(raw).map_err(|message| {
        ConfigLoadError::validation(file_path.to_path_buf(), section, index, "context", message)
    })
}

fn parse_scope(
    file_path: &Path,
    index: usize,
    table: &toml::map::Map<String, Value>,
) -> Result<Scope, ConfigLoadError> {
    let raw = required_string(file_path, "document", index, table, "scope")?;
    Scope::from_config_value(raw).ok_or_else(|| {
        ConfigLoadError::validation(
            file_path.to_path_buf(),
            "document",
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
    origin: CatalogOrigin,
) -> Result<PathBuf, ConfigLoadError> {
    let raw = required_string(file_path, "document", index, table, "path")?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ConfigLoadError::validation(
            file_path.to_path_buf(),
            "document",
            index,
            "path",
            "path cannot be empty",
        ));
    }
    let path = PathBuf::from(trimmed);
    if origin == CatalogOrigin::User
        && (path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            }))
    {
        return Err(ConfigLoadError::validation(
            file_path.to_path_buf(),
            "document",
            index,
            "path",
            "private catalog document paths must be relative and must not contain `..`",
        ));
    }
    Ok(path)
}

fn parse_when_field(
    file_path: &Path,
    index: usize,
    table: &toml::map::Map<String, Value>,
) -> Result<(When, String), ConfigLoadError> {
    let raw = match table.get("when") {
        Some(value) => {
            let Some(value) = value.as_str() else {
                return Err(ConfigLoadError::validation(
                    file_path.to_path_buf(),
                    "document",
                    index,
                    "when",
                    format!(
                        "invalid type for `when`: expected string, found {}",
                        value_type(value)
                    ),
                ));
            };
            value.to_string()
        }
        None => "always".to_string(),
    };

    let when = parse_when(&raw).map_err(|message| {
        ConfigLoadError::validation(file_path.to_path_buf(), "document", index, "when", message)
    })?;
    Ok((when, raw))
}

fn parse_commands(
    file_path: &Path,
    index: usize,
    table: &toml::map::Map<String, Value>,
) -> Result<Vec<String>, ConfigLoadError> {
    let Some(value) = table.get("commands") else {
        return Err(ConfigLoadError::validation(
            file_path.to_path_buf(),
            "validation",
            index,
            "commands",
            "missing required field `commands`",
        ));
    };
    let Some(array) = value.as_array() else {
        return Err(ConfigLoadError::validation(
            file_path.to_path_buf(),
            "validation",
            index,
            "commands",
            format!(
                "invalid type for `commands`: expected array of strings, found {}",
                value_type(value)
            ),
        ));
    };
    if array.is_empty() {
        return Err(ConfigLoadError::validation(
            file_path.to_path_buf(),
            "validation",
            index,
            "commands",
            "`commands` must list at least one command",
        ));
    }
    let mut commands = Vec::with_capacity(array.len());
    for item in array {
        let Some(command) = item.as_str() else {
            return Err(ConfigLoadError::validation(
                file_path.to_path_buf(),
                "validation",
                index,
                "commands",
                format!(
                    "invalid command entry: expected string, found {}",
                    value_type(item)
                ),
            ));
        };
        let command = command.trim();
        if command.is_empty() {
            return Err(ConfigLoadError::validation(
                file_path.to_path_buf(),
                "validation",
                index,
                "commands",
                "command entries cannot be empty",
            ));
        }
        commands.push(command.to_string());
    }
    Ok(commands)
}

fn parse_bool(
    file_path: &Path,
    index: usize,
    table: &toml::map::Map<String, Value>,
    field: &'static str,
) -> Result<Option<bool>, ConfigLoadError> {
    let Some(value) = table.get(field) else {
        return Ok(None);
    };
    let Some(parsed) = value.as_bool() else {
        return Err(ConfigLoadError::validation(
            file_path.to_path_buf(),
            "document",
            index,
            field,
            format!(
                "invalid type for `{field}`: expected boolean, found {}",
                value_type(value)
            ),
        ));
    };
    Ok(Some(parsed))
}

fn parse_u64(
    file_path: &Path,
    index: usize,
    table: &toml::map::Map<String, Value>,
    field: &'static str,
) -> Result<Option<u64>, ConfigLoadError> {
    let Some(value) = table.get(field) else {
        return Ok(None);
    };
    let Some(parsed) = value.as_integer() else {
        return Err(ConfigLoadError::validation(
            file_path.to_path_buf(),
            "document",
            index,
            field,
            format!(
                "invalid type for `{field}`: expected positive integer, found {}",
                value_type(value)
            ),
        ));
    };
    if parsed <= 0 {
        return Err(ConfigLoadError::validation(
            file_path.to_path_buf(),
            "document",
            index,
            field,
            format!("`{field}` must be a positive integer (days)"),
        ));
    }
    Ok(Some(parsed as u64))
}

fn parse_opt_string(
    file_path: &Path,
    section: &'static str,
    index: usize,
    table: &toml::map::Map<String, Value>,
    field: &'static str,
) -> Result<Option<String>, ConfigLoadError> {
    let Some(value) = table.get(field) else {
        return Ok(None);
    };
    let Some(text) = value.as_str() else {
        return Err(ConfigLoadError::validation(
            file_path.to_path_buf(),
            section,
            index,
            field,
            format!(
                "invalid type for `{field}`: expected string, found {}",
                value_type(value)
            ),
        ));
    };
    Ok(Some(text.to_string()))
}

fn required_string<'a>(
    file_path: &Path,
    section: &'static str,
    index: usize,
    table: &'a toml::map::Map<String, Value>,
    field: &'static str,
) -> Result<&'a str, ConfigLoadError> {
    let Some(value) = table.get(field) else {
        return Err(ConfigLoadError::validation(
            file_path.to_path_buf(),
            section,
            index,
            field,
            format!("missing required field `{field}`"),
        ));
    };
    let Some(value) = value.as_str() else {
        return Err(ConfigLoadError::validation(
            file_path.to_path_buf(),
            section,
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

fn parse_error(
    file_path: &Path,
    raw: &str,
    err: &toml::de::Error,
    origin: CatalogOrigin,
) -> ConfigLoadError {
    let location = err
        .span()
        .map(|span| byte_offset_to_line_column(raw, span.start));
    let message = if origin == CatalogOrigin::User {
        "invalid private catalog TOML".to_string()
    } else {
        format!("invalid TOML in {CONFIG_FILE_NAME}: {err}")
    };

    ConfigLoadError::parse(file_path.to_path_buf(), message, location)
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
