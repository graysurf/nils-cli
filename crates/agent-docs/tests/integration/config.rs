use std::fs;
use std::path::{Path, PathBuf};

use agent_docs::config::{CONFIG_FILE_NAME, load_configs, load_scope_config};
use agent_docs::model::{ConfigErrorKind, Context, DocumentWhen, Scope};
use tempfile::TempDir;

fn fixture_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("config")
        .join(relative)
}

fn write_config(root: &Path, body: &str) {
    fs::write(root.join(CONFIG_FILE_NAME), body).expect("write AGENT_DOCS.toml");
}

#[test]
fn load_configs_reads_valid_toml_from_home_and_project_scopes() {
    let home = TempDir::new().expect("create home dir");
    let project = TempDir::new().expect("create project dir");
    let valid = fs::read_to_string(fixture_path("AGENT_DOCS.valid.toml")).expect("read fixture");
    write_config(home.path(), &valid);
    write_config(project.path(), &valid);

    let loaded = load_configs(home.path(), project.path()).expect("load configs");
    let home_config = loaded.home.as_ref().expect("home config should be loaded");
    let project_config = loaded
        .project
        .as_ref()
        .expect("project config should be loaded");

    assert_eq!(home_config.source_scope, Scope::Home);
    assert_eq!(project_config.source_scope, Scope::Project);
    assert_eq!(home_config.documents.len(), 7);
    assert_eq!(project_config.documents.len(), 7);
    assert_eq!(loaded.in_load_order().len(), 2);

    let first = &home_config.documents[0];
    assert_eq!(first.context, Context::Startup);
    assert_eq!(first.scope, Scope::Home);
    assert_eq!(first.path, PathBuf::from("AGENTS.md"));
    assert!(!first.required);
    assert_eq!(first.when, DocumentWhen::Always);
    assert_eq!(first.notes.as_deref(), Some("dup-same-file:first"));
}

#[test]
fn load_scope_config_applies_defaults_for_required_and_when() {
    let home = TempDir::new().expect("create home dir");
    write_config(
        home.path(),
        r#"
[[document]]
context = "startup"
scope = "home"
path = "AGENTS.md"
"#,
    );

    let loaded = load_scope_config(Scope::Home, home.path())
        .expect("load scope config")
        .expect("config should exist");
    assert_eq!(loaded.documents.len(), 1);
    assert!(!loaded.documents[0].required);
    assert_eq!(loaded.documents[0].when, DocumentWhen::Always);
    assert_eq!(loaded.documents[0].notes, None);
}

#[test]
fn load_scope_config_rejects_unsupported_context_with_actionable_error() {
    let home = TempDir::new().expect("create home dir");
    write_config(
        home.path(),
        r#"
[[document]]
context = "project"
scope = "project"
path = "BINARY_DEPENDENCIES.md"
"#,
    );

    let err =
        load_scope_config(Scope::Home, home.path()).expect_err("should reject invalid context");
    assert_eq!(err.kind, ConfigErrorKind::Validation);
    assert_eq!(err.document_index, Some(0));
    assert_eq!(err.field.as_deref(), Some("context"));
    assert!(err.message.contains("unsupported context"));
    assert!(err.message.contains("startup"));
    assert_eq!(err.file_path, home.path().join(CONFIG_FILE_NAME));
}

#[test]
fn load_scope_config_rejects_unsupported_when_with_actionable_error() {
    let home = TempDir::new().expect("create home dir");
    write_config(
        home.path(),
        r#"
[[document]]
context = "task-tools"
scope = "home"
path = "CLI_TOOLS.md"
required = true
when = "if-env:CI"
"#,
    );

    let err = load_scope_config(Scope::Home, home.path()).expect_err("should reject invalid when");
    assert_eq!(err.kind, ConfigErrorKind::Validation);
    assert_eq!(err.document_index, Some(0));
    assert_eq!(err.field.as_deref(), Some("when"));
    assert!(err.message.contains("unsupported when value"));
    assert!(err.message.contains("always"));
}

#[test]
fn load_scope_config_rejects_missing_required_field_path() {
    let home = TempDir::new().expect("create home dir");
    write_config(
        home.path(),
        r#"
[[document]]
context = "startup"
scope = "home"
"#,
    );

    let err = load_scope_config(Scope::Home, home.path()).expect_err("should reject missing path");
    assert_eq!(err.kind, ConfigErrorKind::Validation);
    assert_eq!(err.document_index, Some(0));
    assert_eq!(err.field.as_deref(), Some("path"));
    assert!(err.message.contains("missing required field"));
}

#[test]
fn load_scope_config_reports_parse_error_with_location_when_available() {
    let home = TempDir::new().expect("create home dir");
    write_config(
        home.path(),
        r#"
[[document
context = "startup"
"#,
    );

    let err =
        load_scope_config(Scope::Home, home.path()).expect_err("should reject malformed toml");
    assert_eq!(err.kind, ConfigErrorKind::Parse);
    assert!(err.location.is_some());
    assert!(err.message.contains("invalid TOML"));
}

#[test]
fn load_scope_config_reports_io_error_when_config_path_is_directory() {
    let home = TempDir::new().expect("create home dir");
    fs::create_dir(home.path().join(CONFIG_FILE_NAME)).expect("create config dir");

    let err = load_scope_config(Scope::Home, home.path())
        .expect_err("directory at config path should be rejected");
    assert_eq!(err.kind, ConfigErrorKind::Io);
    assert_eq!(err.file_path, home.path().join(CONFIG_FILE_NAME));
    assert!(err.message.contains("failed to read AGENT_DOCS.toml"));
}

#[test]
fn load_scope_config_rejects_root_literal_with_parse_error() {
    let home = TempDir::new().expect("create home dir");
    write_config(home.path(), r#""not-a-table""#);

    let err =
        load_scope_config(Scope::Home, home.path()).expect_err("root literal should be rejected");
    assert_eq!(err.kind, ConfigErrorKind::Parse);
    assert!(err.message.contains("invalid TOML"));
}

#[test]
fn load_scope_config_rejects_document_key_that_is_not_array() {
    let home = TempDir::new().expect("create home dir");
    write_config(home.path(), r#"document = "oops""#);

    let err =
        load_scope_config(Scope::Home, home.path()).expect_err("document key type should fail");
    assert_eq!(err.kind, ConfigErrorKind::Validation);
    assert_eq!(err.document_index, None);
    assert_eq!(err.field.as_deref(), Some("document"));
    assert!(
        err.message
            .contains("must be an array of [[document]] tables")
    );
}

#[test]
fn load_scope_config_rejects_document_entry_that_is_not_table() {
    let home = TempDir::new().expect("create home dir");
    write_config(home.path(), r#"document = ["oops"]"#);

    let err =
        load_scope_config(Scope::Home, home.path()).expect_err("document entry type should fail");
    assert_eq!(err.kind, ConfigErrorKind::Validation);
    assert_eq!(err.document_index, Some(0));
    assert_eq!(err.field.as_deref(), Some("document"));
    assert!(err.message.contains("entry must be a TOML table"));
}

#[test]
fn load_scope_config_rejects_unknown_document_field_with_allowed_list() {
    let home = TempDir::new().expect("create home dir");
    write_config(
        home.path(),
        r#"
[[document]]
context = "startup"
scope = "home"
path = "AGENTS.md"
unexpected = true
"#,
    );

    let err = load_scope_config(Scope::Home, home.path()).expect_err("unknown field should fail");
    assert_eq!(err.kind, ConfigErrorKind::Validation);
    assert_eq!(err.document_index, Some(0));
    assert_eq!(err.field.as_deref(), Some("unexpected"));
    assert!(err.message.contains("unsupported field `unexpected`"));
    assert!(err.message.contains("context"));
    assert!(err.message.contains("notes"));
}

#[test]
fn load_scope_config_rejects_unsupported_scope_with_actionable_error() {
    let home = TempDir::new().expect("create home dir");
    write_config(
        home.path(),
        r#"
[[document]]
context = "startup"
scope = "workspace"
path = "AGENTS.md"
"#,
    );

    let err = load_scope_config(Scope::Home, home.path()).expect_err("scope should be rejected");
    assert_eq!(err.kind, ConfigErrorKind::Validation);
    assert_eq!(err.document_index, Some(0));
    assert_eq!(err.field.as_deref(), Some("scope"));
    assert!(err.message.contains("unsupported scope"));
    assert!(err.message.contains("home"));
    assert!(err.message.contains("project"));
}

#[test]
fn load_scope_config_rejects_path_that_is_only_whitespace() {
    let home = TempDir::new().expect("create home dir");
    write_config(
        home.path(),
        r#"
[[document]]
context = "startup"
scope = "home"
path = "   "
"#,
    );

    let err = load_scope_config(Scope::Home, home.path()).expect_err("empty path should fail");
    assert_eq!(err.kind, ConfigErrorKind::Validation);
    assert_eq!(err.document_index, Some(0));
    assert_eq!(err.field.as_deref(), Some("path"));
    assert!(err.message.contains("path cannot be empty"));
}

#[test]
fn load_scope_config_reports_type_errors_for_optional_fields() {
    let home = TempDir::new().expect("create home dir");
    let cases = [
        ("required = \"yes\"", "required", "string"),
        ("when = 123", "when", "integer"),
        ("notes = []", "notes", "array"),
    ];

    for (assignment, expected_field, expected_type) in cases {
        write_config(
            home.path(),
            &format!(
                r#"
[[document]]
context = "startup"
scope = "home"
path = "AGENTS.md"
{assignment}
"#
            ),
        );

        let err = load_scope_config(Scope::Home, home.path())
            .expect_err("optional field type mismatch should fail");
        assert_eq!(err.kind, ConfigErrorKind::Validation);
        assert_eq!(err.document_index, Some(0));
        assert_eq!(err.field.as_deref(), Some(expected_field));
        assert!(err.message.contains(&format!("found {expected_type}")));
    }
}

#[test]
fn load_scope_config_reports_type_errors_for_required_string_fields() {
    let home = TempDir::new().expect("create home dir");
    let cases = [
        (
            r#"
context = 1979-05-27T07:32:00Z
scope = "home"
path = "AGENTS.md"
"#,
            "context",
            "datetime",
        ),
        (
            r#"
context = "startup"
scope = { kind = "home" }
path = "AGENTS.md"
"#,
            "scope",
            "table",
        ),
        (
            r#"
context = "startup"
scope = "home"
path = 1.5
"#,
            "path",
            "float",
        ),
    ];

    for (body, expected_field, expected_type) in cases {
        write_config(
            home.path(),
            &format!(
                r#"
[[document]]
{body}
"#
            ),
        );

        let err = load_scope_config(Scope::Home, home.path())
            .expect_err("required string field type mismatch should fail");
        assert_eq!(err.kind, ConfigErrorKind::Validation);
        assert_eq!(err.document_index, Some(0));
        assert_eq!(err.field.as_deref(), Some(expected_field));
        assert!(err.message.contains("expected string"));
        assert!(err.message.contains(&format!("found {expected_type}")));
    }
}
