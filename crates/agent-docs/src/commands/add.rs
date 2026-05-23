use std::fs;
use std::path::{Path, PathBuf};

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value, value};

use crate::config::{CONFIG_FILE_NAME, config_path_for_root, load_scope_config};
use crate::env::ResolvedRoots;
use crate::model::{
    AddDocumentAction, AddDocumentReport, ConfigDocumentEntry, ConfigLoadError, Context,
    DocumentWhen, Scope,
};

const DOCUMENT_KEY: &str = "document";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddDocumentRequest {
    pub target: Scope,
    pub context: Context,
    pub scope: Scope,
    pub path: PathBuf,
    pub required: bool,
    pub when: DocumentWhen,
    pub notes: Option<String>,
}

impl AddDocumentRequest {
    fn into_config_entry(self, config_path: &Path) -> Result<ConfigDocumentEntry, ConfigLoadError> {
        let path = normalize_requested_path(&self.path, config_path)?;
        Ok(ConfigDocumentEntry {
            context: self.context,
            scope: self.scope,
            path,
            required: self.required,
            when: self.when,
            notes: self.notes,
        })
    }
}

pub fn upsert_document(
    roots: &ResolvedRoots,
    request: AddDocumentRequest,
) -> Result<AddDocumentReport, ConfigLoadError> {
    let target_root = match request.target {
        Scope::Home => roots.docs_home.as_path(),
        Scope::Project => roots.project_path.as_path(),
    };
    upsert_document_at_root(target_root, request)
}

pub fn upsert_document_at_root(
    target_root: &Path,
    request: AddDocumentRequest,
) -> Result<AddDocumentReport, ConfigLoadError> {
    let target = request.target;
    let config_path = config_path_for_root(target_root);
    let entry = request.into_config_entry(&config_path)?;

    // Keep existing behavior: malformed existing config fails with a validation/parse error.
    if config_path.exists() {
        let _ = load_scope_config(target, target_root)?;
    }

    let (mut document, created_config) = load_document(&config_path)?;

    let (action, document_count) = {
        let documents = documents_array_mut(&mut document, &config_path)?;
        let action = upsert_documents(documents, &entry, &config_path)?;
        (action, documents.len())
    };

    write_document(&config_path, &document)?;

    Ok(AddDocumentReport {
        target,
        target_root: target_root.to_path_buf(),
        config_path,
        created_config,
        action,
        entry,
        document_count,
    })
}

fn load_document(config_path: &Path) -> Result<(DocumentMut, bool), ConfigLoadError> {
    if !config_path.exists() {
        return Ok((DocumentMut::new(), true));
    }

    let raw = fs::read_to_string(config_path).map_err(|err| {
        ConfigLoadError::io(
            config_path.to_path_buf(),
            format!("failed to read {}: {err}", CONFIG_FILE_NAME),
        )
    })?;

    let document = raw.parse::<DocumentMut>().map_err(|err| {
        ConfigLoadError::parse(
            config_path.to_path_buf(),
            format!("invalid TOML in {}: {err}", CONFIG_FILE_NAME),
            None,
        )
    })?;

    Ok((document, false))
}

fn documents_array_mut<'a>(
    document: &'a mut DocumentMut,
    config_path: &Path,
) -> Result<&'a mut ArrayOfTables, ConfigLoadError> {
    if document.get(DOCUMENT_KEY).is_none() {
        document[DOCUMENT_KEY] = Item::ArrayOfTables(ArrayOfTables::new());
    }

    document
        .get_mut(DOCUMENT_KEY)
        .and_then(Item::as_array_of_tables_mut)
        .ok_or_else(|| {
            ConfigLoadError::validation_root(
                config_path.to_path_buf(),
                DOCUMENT_KEY,
                "key `document` must be an array of [[document]] tables",
            )
        })
}

fn upsert_documents(
    documents: &mut ArrayOfTables,
    incoming: &ConfigDocumentEntry,
    config_path: &Path,
) -> Result<AddDocumentAction, ConfigLoadError> {
    let incoming_path = path_to_utf8(&incoming.path, config_path)?;

    let mut matching_indices = Vec::new();
    for (index, table) in documents.iter().enumerate() {
        if table_matches(table, incoming, &incoming_path) {
            matching_indices.push(index);
        }
    }

    if matching_indices.is_empty() {
        let mut table = Table::new();
        apply_entry_to_table(&mut table, incoming, &incoming_path);
        documents.push(table);
        return Ok(AddDocumentAction::Inserted);
    }

    let mut replace_index = *matching_indices.last().expect("matching index exists");

    for index in matching_indices.into_iter().rev() {
        if index != replace_index {
            documents.remove(index);
            if index < replace_index {
                replace_index -= 1;
            }
        }
    }

    let table = documents
        .get_mut(replace_index)
        .expect("replace index should remain valid");
    apply_entry_to_table(table, incoming, &incoming_path);
    Ok(AddDocumentAction::Updated)
}

fn table_matches(table: &Table, incoming: &ConfigDocumentEntry, incoming_path: &str) -> bool {
    let context = table.get("context").and_then(Item::as_str);
    let scope = table.get("scope").and_then(Item::as_str);
    let path = table.get("path").and_then(Item::as_str).map(str::trim);

    context == Some(incoming.context.as_str())
        && scope == Some(incoming.scope.as_str())
        && path == Some(incoming_path)
}

fn apply_entry_to_table(table: &mut Table, incoming: &ConfigDocumentEntry, incoming_path: &str) {
    set_string_field(table, "context", incoming.context.as_str());
    set_string_field(table, "scope", incoming.scope.as_str());
    set_string_field(table, "path", incoming_path);
    set_bool_field(table, "required", incoming.required);
    set_string_field(table, "when", incoming.when.as_str());

    if let Some(notes) = incoming.notes.as_deref() {
        set_string_field(table, "notes", notes);
    } else {
        table.remove("notes");
    }
}

fn set_string_field(table: &mut Table, key: &str, field_value: &str) {
    if preserve_existing_value_decor(table, key, Value::from(field_value)) {
        return;
    }
    table[key] = value(field_value);
}

fn set_bool_field(table: &mut Table, key: &str, field_value: bool) {
    if preserve_existing_value_decor(table, key, Value::from(field_value)) {
        return;
    }
    table[key] = value(field_value);
}

fn preserve_existing_value_decor(table: &mut Table, key: &str, field_value: Value) -> bool {
    let Some(existing_item) = table.get_mut(key) else {
        return false;
    };
    let Some(existing_value) = existing_item.as_value_mut() else {
        return false;
    };

    let existing_decor = existing_value.decor().clone();
    *existing_value = field_value;
    *existing_value.decor_mut() = existing_decor;
    true
}

fn normalize_requested_path(path: &Path, config_path: &Path) -> Result<PathBuf, ConfigLoadError> {
    let Some(raw_path) = path.to_str() else {
        return Err(ConfigLoadError::validation_root(
            config_path.to_path_buf(),
            "path",
            "path must be valid UTF-8 for TOML serialization",
        ));
    };

    let trimmed = raw_path.trim();
    if trimmed.is_empty() {
        return Err(ConfigLoadError::validation_root(
            config_path.to_path_buf(),
            "path",
            "path cannot be empty",
        ));
    }

    Ok(PathBuf::from(trimmed))
}

fn write_document(config_path: &Path, document: &DocumentMut) -> Result<(), ConfigLoadError> {
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            ConfigLoadError::io(
                config_path.to_path_buf(),
                format!(
                    "failed to create parent directory for {}: {err}",
                    CONFIG_FILE_NAME
                ),
            )
        })?;
    }

    let mut body = document.to_string();
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }

    fs::write(config_path, body).map_err(|err| {
        ConfigLoadError::io(
            config_path.to_path_buf(),
            format!("failed to write {}: {err}", CONFIG_FILE_NAME),
        )
    })
}

fn path_to_utf8(path: &Path, config_path: &Path) -> Result<String, ConfigLoadError> {
    path.to_str().map(ToString::to_string).ok_or_else(|| {
        ConfigLoadError::validation_root(
            config_path.to_path_buf(),
            "path",
            "path must be valid UTF-8 for TOML serialization",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::{CONFIG_FILE_NAME, load_scope_config};
    use crate::model::ConfigErrorKind;
    use tempfile::TempDir;

    fn roots(home: &TempDir, project: &TempDir) -> ResolvedRoots {
        ResolvedRoots {
            docs_home: home.path().to_path_buf(),
            project_path: project.path().to_path_buf(),
            is_linked_worktree: false,
            git_common_dir: None,
            primary_worktree_path: None,
        }
    }

    #[test]
    fn upsert_document_creates_missing_target_config_and_persists_entry() {
        let home = TempDir::new().expect("create home tempdir");
        let project = TempDir::new().expect("create project tempdir");
        let roots = roots(&home, &project);

        let request = AddDocumentRequest {
            target: Scope::Project,
            context: Context::ProjectDev,
            scope: Scope::Project,
            path: PathBuf::from("BINARY_DEPENDENCIES.md"),
            required: true,
            when: DocumentWhen::Always,
            notes: Some("External runtime tools required by this project".to_string()),
        };

        let report = upsert_document(&roots, request).expect("upsert should succeed");
        assert_eq!(report.target, Scope::Project);
        assert!(report.created_config);
        assert_eq!(report.action, AddDocumentAction::Inserted);
        assert_eq!(report.document_count, 1);
        assert_eq!(report.config_path, project.path().join(CONFIG_FILE_NAME));

        let written =
            fs::read_to_string(project.path().join(CONFIG_FILE_NAME)).expect("read written file");
        assert!(written.contains("[[document]]"));
        assert!(written.contains("context = \"project-dev\""));
        assert!(written.contains("scope = \"project\""));
        assert!(written.contains("path = \"BINARY_DEPENDENCIES.md\""));
        assert!(written.contains("required = true"));
        assert!(written.contains("when = \"always\""));

        let loaded = load_scope_config(Scope::Project, project.path())
            .expect("load config")
            .expect("config should exist");
        assert_eq!(loaded.documents, vec![report.entry]);
    }

    #[test]
    fn upsert_document_updates_existing_key_without_duplicate_entries() {
        let home = TempDir::new().expect("create home tempdir");
        let project = TempDir::new().expect("create project tempdir");
        let roots = roots(&home, &project);

        let initial = AddDocumentRequest {
            target: Scope::Home,
            context: Context::TaskTools,
            scope: Scope::Home,
            path: PathBuf::from("core/policies/cli-tools.md"),
            required: false,
            when: DocumentWhen::Always,
            notes: Some("initial".to_string()),
        };
        upsert_document(&roots, initial).expect("initial insert");

        let update = AddDocumentRequest {
            target: Scope::Home,
            context: Context::TaskTools,
            scope: Scope::Home,
            path: PathBuf::from("core/policies/cli-tools.md"),
            required: true,
            when: DocumentWhen::Always,
            notes: Some("updated".to_string()),
        };
        let report = upsert_document(&roots, update.clone()).expect("update should succeed");
        assert!(!report.created_config);
        assert_eq!(report.action, AddDocumentAction::Updated);
        assert_eq!(report.document_count, 1);

        let after_update =
            fs::read_to_string(home.path().join(CONFIG_FILE_NAME)).expect("read updated file");
        let second_report = upsert_document(&roots, update).expect("second upsert should succeed");
        assert_eq!(second_report.action, AddDocumentAction::Updated);
        let after_second_update =
            fs::read_to_string(home.path().join(CONFIG_FILE_NAME)).expect("read second update");
        assert_eq!(after_update, after_second_update);

        let loaded = load_scope_config(Scope::Home, home.path())
            .expect("load config")
            .expect("config should exist");
        assert_eq!(loaded.documents.len(), 1);
        let only = &loaded.documents[0];
        assert_eq!(only.context, Context::TaskTools);
        assert_eq!(only.scope, Scope::Home);
        assert_eq!(only.path, Path::new("core/policies/cli-tools.md"));
        assert!(only.required);
        assert_eq!(only.when, DocumentWhen::Always);
        assert_eq!(only.notes.as_deref(), Some("updated"));
    }

    #[test]
    fn upsert_document_deduplicates_existing_same_key_entries() {
        let home = TempDir::new().expect("create home tempdir");
        let project = TempDir::new().expect("create project tempdir");
        fs::write(
            project.path().join(CONFIG_FILE_NAME),
            r#"
[[document]]
context = "project-dev"
scope = "project"
path = "BINARY_DEPENDENCIES.md"
required = false
when = "always"
notes = "first"

[[document]]
context = "task-tools"
scope = "home"
path = "core/policies/cli-tools.md"
required = true
when = "always"
notes = "other"

[[document]]
context = "project-dev"
scope = "project"
path = "BINARY_DEPENDENCIES.md"
required = true
when = "always"
notes = "second"
"#,
        )
        .expect("seed duplicate config");

        let roots = roots(&home, &project);
        let report = upsert_document(
            &roots,
            AddDocumentRequest {
                target: Scope::Project,
                context: Context::ProjectDev,
                scope: Scope::Project,
                path: PathBuf::from("BINARY_DEPENDENCIES.md"),
                required: true,
                when: DocumentWhen::Always,
                notes: Some("deduped".to_string()),
            },
        )
        .expect("upsert should succeed");
        assert_eq!(report.action, AddDocumentAction::Updated);
        assert_eq!(report.document_count, 2);

        let loaded = load_scope_config(Scope::Project, project.path())
            .expect("load config")
            .expect("config should exist");
        let duplicates: Vec<_> = loaded
            .documents
            .iter()
            .filter(|document| {
                document.context == Context::ProjectDev
                    && document.scope == Scope::Project
                    && document.path == Path::new("BINARY_DEPENDENCIES.md")
            })
            .collect();
        assert_eq!(duplicates.len(), 1);
        assert_eq!(duplicates[0].notes.as_deref(), Some("deduped"));
    }

    #[test]
    fn upsert_document_rejects_empty_path_after_trim() {
        let home = TempDir::new().expect("create home tempdir");
        let project = TempDir::new().expect("create project tempdir");
        let roots = roots(&home, &project);

        let err = upsert_document(
            &roots,
            AddDocumentRequest {
                target: Scope::Project,
                context: Context::ProjectDev,
                scope: Scope::Project,
                path: PathBuf::from("   "),
                required: true,
                when: DocumentWhen::Always,
                notes: None,
            },
        )
        .expect_err("empty path should be rejected");
        assert_eq!(err.kind, ConfigErrorKind::Validation);
        assert_eq!(err.field.as_deref(), Some("path"));
        assert!(err.message.contains("path cannot be empty"));
    }

    #[test]
    fn upsert_document_preserves_entries_after_updated_key() {
        let home = TempDir::new().expect("create home tempdir");
        let project = TempDir::new().expect("create project tempdir");
        fs::write(
            home.path().join(CONFIG_FILE_NAME),
            r#"
[[document]]
context = "task-tools"
scope = "home"
path = "core/policies/cli-tools.md"
required = false
when = "always"
notes = "before"

[[document]]
context = "skill-dev"
scope = "home"
path = "DEVELOPMENT.md"
required = true
when = "always"
notes = "tail"
"#,
        )
        .expect("seed config");

        let roots = roots(&home, &project);
        let report = upsert_document(
            &roots,
            AddDocumentRequest {
                target: Scope::Home,
                context: Context::TaskTools,
                scope: Scope::Home,
                path: PathBuf::from("core/policies/cli-tools.md"),
                required: true,
                when: DocumentWhen::Always,
                notes: Some("after".to_string()),
            },
        )
        .expect("upsert should succeed");
        assert_eq!(report.action, AddDocumentAction::Updated);
        assert_eq!(report.document_count, 2);

        let loaded = load_scope_config(Scope::Home, home.path())
            .expect("load config")
            .expect("config should exist");
        assert_eq!(loaded.documents.len(), 2);
        assert_eq!(loaded.documents[0].context, Context::TaskTools);
        assert_eq!(loaded.documents[0].notes.as_deref(), Some("after"));
        assert_eq!(loaded.documents[1].context, Context::SkillDev);
        assert_eq!(loaded.documents[1].notes.as_deref(), Some("tail"));
    }

    #[test]
    fn upsert_document_preserves_top_level_comments() {
        let home = TempDir::new().expect("create home tempdir");
        let project = TempDir::new().expect("create project tempdir");
        fs::write(
            home.path().join(CONFIG_FILE_NAME),
            r#"# keep this comment

[[document]]
context = "task-tools"
scope = "home"
path = "core/policies/cli-tools.md"
required = false
when = "always"
notes = "before"
"#,
        )
        .expect("seed commented config");

        let roots = roots(&home, &project);
        upsert_document(
            &roots,
            AddDocumentRequest {
                target: Scope::Home,
                context: Context::TaskTools,
                scope: Scope::Home,
                path: PathBuf::from("core/policies/cli-tools.md"),
                required: true,
                when: DocumentWhen::Always,
                notes: Some("after".to_string()),
            },
        )
        .expect("upsert should succeed");

        let written = fs::read_to_string(home.path().join(CONFIG_FILE_NAME)).expect("read config");
        assert!(written.contains("# keep this comment"));
        assert!(written.contains("notes = \"after\""));
    }

    #[test]
    fn upsert_document_preserves_inline_comments_on_updated_table() {
        let home = TempDir::new().expect("create home tempdir");
        let project = TempDir::new().expect("create project tempdir");
        fs::write(
            home.path().join(CONFIG_FILE_NAME),
            r#"# keep file header

[[document]]
context = "task-tools" # keep context comment
scope = "home" # keep scope comment
path = "core/policies/cli-tools.md" # keep path comment
required = false # keep required comment
when = "always" # keep when comment
notes = "before" # keep notes comment
"#,
        )
        .expect("seed commented config");

        let roots = roots(&home, &project);
        upsert_document(
            &roots,
            AddDocumentRequest {
                target: Scope::Home,
                context: Context::TaskTools,
                scope: Scope::Home,
                path: PathBuf::from("core/policies/cli-tools.md"),
                required: true,
                when: DocumentWhen::Always,
                notes: Some("after".to_string()),
            },
        )
        .expect("upsert should succeed");

        let written = fs::read_to_string(home.path().join(CONFIG_FILE_NAME)).expect("read config");
        assert!(written.contains("# keep file header"));
        assert!(written.contains("# keep context comment"));
        assert!(written.contains("# keep scope comment"));
        assert!(written.contains("# keep path comment"));
        assert!(written.contains("# keep required comment"));
        assert!(written.contains("# keep when comment"));
        assert!(written.contains("# keep notes comment"));
        assert!(written.contains("required = true"));
        assert!(written.contains("notes = \"after\""));
    }
}
