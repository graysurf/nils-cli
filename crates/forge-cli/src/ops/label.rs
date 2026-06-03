//! Provider-neutral repository label operations.
//!
//! The catalog lives outside `nils-cli`; this module intentionally accepts a
//! small, explicit YAML/JSON shape and turns it into backend `gh label` /
//! `glab label` calls without taking ownership of taxonomy policy.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::{Deserialize, Serialize};

use crate::backend::{BackendCall, BackendProgram, BackendRunner, BackendSuccess, ProcessRunner};
use crate::cli::{BINARY, GlobalFlags, LabelAuditArgs, LabelCommand, LabelEnsureArgs};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};

const LIST_SCHEMA: &str = "label.list";
const AUDIT_SCHEMA: &str = "label.audit";
const ENSURE_SCHEMA: &str = "label.ensure";
const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProviderLabel {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub color: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize)]
struct LabelListPayload {
    provider: &'static str,
    labels: Vec<ProviderLabel>,
}

#[derive(Debug, Clone, Serialize)]
struct LabelAuditPayload {
    provider: &'static str,
    status: &'static str,
    missing: Vec<CatalogLabel>,
    drift: Vec<LabelDrift>,
    unknown_shared: Vec<ProviderLabel>,
}

#[derive(Debug, Clone, Serialize)]
struct LabelEnsurePayload {
    provider: &'static str,
    dry_run: bool,
    actions: Vec<EnsureAction>,
}

#[derive(Debug, Clone, Serialize)]
struct EnsureAction {
    kind: &'static str,
    label: CatalogLabel,
    #[serde(skip_serializing_if = "Option::is_none")]
    current: Option<ProviderLabel>,
    fields: Vec<DriftField>,
    plan: Vec<String>,
    status: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct LabelDrift {
    name: String,
    current: ProviderLabel,
    expected: CatalogLabel,
    fields: Vec<DriftField>,
}

#[derive(Debug, Clone, Serialize)]
struct DriftField {
    field: &'static str,
    expected: String,
    actual: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CatalogRaw {
    #[serde(default)]
    groups: Vec<CatalogGroup>,
    labels: Vec<CatalogLabelRaw>,
}

#[derive(Debug, Clone, Deserialize)]
struct CatalogGroup {
    name: String,
    prefix: String,
    #[serde(default)]
    exclusive: bool,
    #[serde(default)]
    allow_extensions: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct CatalogLabelRaw {
    name: String,
    #[serde(default)]
    group: Option<String>,
    color: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    applies_to: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CatalogLabel {
    pub name: String,
    pub group: String,
    pub color: String,
    pub description: String,
    pub applies_to: Vec<String>,
}

#[derive(Debug, Clone)]
struct Catalog {
    labels: Vec<CatalogLabel>,
    groups: Vec<CatalogGroup>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelTarget {
    Issue,
    Pr,
    Mr,
}

impl LabelTarget {
    fn as_str(self) -> &'static str {
        match self {
            Self::Issue => "issue",
            Self::Pr => "pr",
            Self::Mr => "mr",
        }
    }
}

pub fn run(
    global: &GlobalFlags,
    command: LabelCommand,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = ProcessRunner;
    run_with(&runner, global, command, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    command: LabelCommand,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        remote_url_lookup,
    )?;

    match command {
        LabelCommand::List(args) => {
            let labels = fetch_labels(runner, &ctx, args.limit)?;
            Ok(emit_success(
                schema_ok(LIST_SCHEMA),
                LabelListPayload {
                    provider: ctx.provider.as_str(),
                    labels,
                },
                format,
                render_list_text,
            ))
        }
        LabelCommand::Audit(args) => run_audit(runner, &ctx, args, format),
        LabelCommand::Ensure(args) => run_ensure(runner, global.dry_run, &ctx, args, format),
    }
}

fn run_audit<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    args: LabelAuditArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let catalog = load_catalog(&args.catalog)?;
    let current = fetch_labels(runner, ctx, args.limit)?;
    let audit = audit_labels(&catalog, &current);
    let status =
        if audit.missing.is_empty() && audit.drift.is_empty() && audit.unknown_shared.is_empty() {
            "pass"
        } else {
            "fail"
        };
    Ok(emit_success(
        schema_ok(AUDIT_SCHEMA),
        LabelAuditPayload {
            provider: ctx.provider.as_str(),
            status,
            missing: audit.missing,
            drift: audit.drift,
            unknown_shared: audit.unknown_shared,
        },
        format,
        render_audit_text,
    ))
}

fn run_ensure<R: BackendRunner>(
    runner: &R,
    dry_run: bool,
    ctx: &ProviderContext,
    args: LabelEnsureArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let catalog = load_catalog(&args.catalog)?;
    let current = fetch_labels(runner, ctx, args.limit)?;
    let audit = audit_labels(&catalog, &current);
    let mut actions = Vec::new();

    for label in audit.missing {
        let call = build_create_call(ctx, &label);
        let plan = call.plan_argv();
        if !dry_run {
            let _ = runner.run(&call)?;
        }
        actions.push(EnsureAction {
            kind: "create",
            label,
            current: None,
            fields: Vec::new(),
            plan,
            status: if dry_run { "planned" } else { "applied" },
        });
    }

    if args.update_existing {
        for drift in audit.drift {
            if let Some(call) = build_update_call(ctx, &drift.expected, &drift.current) {
                let plan = call.plan_argv();
                if !dry_run {
                    let _ = runner.run(&call)?;
                }
                actions.push(EnsureAction {
                    kind: "update",
                    label: drift.expected,
                    current: Some(drift.current),
                    fields: drift.fields,
                    plan,
                    status: if dry_run { "planned" } else { "applied" },
                });
            }
        }
    }

    Ok(emit_success(
        schema_ok(ENSURE_SCHEMA),
        LabelEnsurePayload {
            provider: ctx.provider.as_str(),
            dry_run,
            actions,
        },
        format,
        render_ensure_text,
    ))
}

fn fetch_labels<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    limit: u32,
) -> Result<Vec<ProviderLabel>, ForgeError> {
    let call = build_list_call(ctx, limit);
    let output = runner.run(&call)?;
    parse_list_output(ctx, &output)
}

fn build_list_call(ctx: &ProviderContext, limit: u32) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let mut argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub | Provider::Local => vec![
            OsString::from("label"),
            OsString::from("list"),
            OsString::from("--limit"),
            OsString::from(limit.to_string()),
            OsString::from("--json"),
            OsString::from("id,name,color,description"),
        ],
        Provider::GitLab => vec![
            OsString::from("label"),
            OsString::from("list"),
            OsString::from("--output"),
            OsString::from("json"),
            OsString::from("--per-page"),
            OsString::from(limit.to_string()),
        ],
    };
    ctx.push_repo_override(&mut argv);
    BackendCall::new(program, argv)
}

fn build_create_call(ctx: &ProviderContext, label: &CatalogLabel) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let mut argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub | Provider::Local => vec![
            OsString::from("label"),
            OsString::from("create"),
            OsString::from(&label.name),
            OsString::from("--color"),
            OsString::from(&label.color),
            OsString::from("--description"),
            OsString::from(&label.description),
        ],
        Provider::GitLab => vec![
            OsString::from("label"),
            OsString::from("create"),
            OsString::from("--name"),
            OsString::from(&label.name),
            OsString::from("--color"),
            OsString::from(format!("#{}", label.color)),
            OsString::from("--description"),
            OsString::from(&label.description),
        ],
    };
    ctx.push_repo_override(&mut argv);
    BackendCall::new(program, argv)
}

fn build_update_call(
    ctx: &ProviderContext,
    expected: &CatalogLabel,
    current: &ProviderLabel,
) -> Option<BackendCall> {
    let program = BackendProgram::for_provider(ctx.provider);
    let mut argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub | Provider::Local => vec![
            OsString::from("label"),
            OsString::from("edit"),
            OsString::from(&expected.name),
            OsString::from("--color"),
            OsString::from(&expected.color),
            OsString::from("--description"),
            OsString::from(&expected.description),
        ],
        Provider::GitLab => {
            let id = current.id.as_ref()?;
            vec![
                OsString::from("label"),
                OsString::from("edit"),
                OsString::from("--label-id"),
                OsString::from(id),
                OsString::from("--color"),
                OsString::from(format!("#{}", expected.color)),
                OsString::from("--description"),
                OsString::from(&expected.description),
            ]
        }
    };
    ctx.push_repo_override(&mut argv);
    Some(BackendCall::new(program, argv))
}

fn parse_list_output(
    ctx: &ProviderContext,
    output: &BackendSuccess,
) -> Result<Vec<ProviderLabel>, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema_err(),
            format!("{} label list JSON is invalid", ctx.provider.as_str()),
            Some(e.to_string()),
        )
    })?;
    let labels = value
        .as_array()
        .ok_or_else(|| {
            ForgeError::software(
                schema_err(),
                "label list JSON must be an array",
                Some(output.stdout.clone()),
            )
        })?
        .iter()
        .filter_map(parse_provider_label)
        .collect();
    Ok(labels)
}

fn parse_provider_label(value: &serde_json::Value) -> Option<ProviderLabel> {
    let name = value.get("name")?.as_str()?.to_string();
    let color = value
        .get("color")
        .and_then(|v| v.as_str())
        .map(normalize_color)
        .unwrap_or_default();
    let description = value
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let id = value.get("id").and_then(|v| {
        v.as_str()
            .map(str::to_string)
            .or_else(|| v.as_i64().map(|i| i.to_string()))
            .or_else(|| v.as_u64().map(|i| i.to_string()))
    });
    Some(ProviderLabel {
        id,
        name,
        color,
        description,
    })
}

struct AuditResult {
    missing: Vec<CatalogLabel>,
    drift: Vec<LabelDrift>,
    unknown_shared: Vec<ProviderLabel>,
}

fn audit_labels(catalog: &Catalog, current: &[ProviderLabel]) -> AuditResult {
    let current_by_name: BTreeMap<&str, &ProviderLabel> =
        current.iter().map(|l| (l.name.as_str(), l)).collect();
    let catalog_names: BTreeSet<&str> = catalog.labels.iter().map(|l| l.name.as_str()).collect();

    let mut missing = Vec::new();
    let mut drift = Vec::new();
    for label in &catalog.labels {
        let Some(current_label) = current_by_name.get(label.name.as_str()) else {
            missing.push(label.clone());
            continue;
        };
        let fields = drift_fields(label, current_label);
        if !fields.is_empty() {
            drift.push(LabelDrift {
                name: label.name.clone(),
                current: (*current_label).clone(),
                expected: label.clone(),
                fields,
            });
        }
    }

    let unknown_shared = current
        .iter()
        .filter(|label| {
            !catalog_names.contains(label.name.as_str())
                && catalog.is_known_shared_label(&label.name)
        })
        .cloned()
        .collect();

    AuditResult {
        missing,
        drift,
        unknown_shared,
    }
}

fn drift_fields(expected: &CatalogLabel, current: &ProviderLabel) -> Vec<DriftField> {
    let mut fields = Vec::new();
    if expected.color != current.color {
        fields.push(DriftField {
            field: "color",
            expected: expected.color.clone(),
            actual: current.color.clone(),
        });
    }
    if expected.description != current.description {
        fields.push(DriftField {
            field: "description",
            expected: expected.description.clone(),
            actual: current.description.clone(),
        });
    }
    fields
}

pub fn validate_label_inputs(
    labels: &[String],
    catalog_path: Option<&str>,
    strict: bool,
    target: LabelTarget,
) -> Result<(), ForgeError> {
    if !strict {
        return Ok(());
    }
    let path = catalog_path.ok_or_else(|| {
        ForgeError::validation(
            schema_err(),
            "label_catalog_missing",
            "--strict-labels requires --label-catalog",
            None,
        )
    })?;
    let catalog = load_catalog(path)?;
    catalog.validate_selected_labels(labels, target)
}

fn load_catalog(path: &str) -> Result<Catalog, ForgeError> {
    let raw = fs::read_to_string(path).map_err(|e| {
        ForgeError::software(
            schema_err(),
            format!("failed to read label catalog '{path}'"),
            Some(e.to_string()),
        )
    })?;
    let parsed: CatalogRaw = serde_yaml_ng::from_str(&raw).map_err(|e| {
        ForgeError::software(
            schema_err(),
            format!("failed to parse label catalog '{path}'"),
            Some(e.to_string()),
        )
    })?;
    Catalog::from_raw(parsed)
}

impl Catalog {
    fn from_raw(raw: CatalogRaw) -> Result<Self, ForgeError> {
        if raw.labels.is_empty() {
            return Err(ForgeError::validation(
                schema_err(),
                "label_catalog_empty",
                "label catalog must contain at least one label",
                None,
            ));
        }
        let mut labels = Vec::with_capacity(raw.labels.len());
        for label in raw.labels {
            if label.name.trim().is_empty() {
                return Err(ForgeError::validation(
                    schema_err(),
                    "label_catalog_invalid",
                    "label catalog contains an empty label name",
                    None,
                ));
            }
            let group = label
                .group
                .clone()
                .or_else(|| label.name.split_once("::").map(|(g, _)| g.to_string()))
                .unwrap_or_default();
            labels.push(CatalogLabel {
                name: label.name,
                group,
                color: normalize_color(&label.color),
                description: label.description,
                applies_to: label.applies_to,
            });
        }
        Ok(Self {
            labels,
            groups: raw.groups,
        })
    }

    fn label_by_name(&self, name: &str) -> Option<&CatalogLabel> {
        self.labels.iter().find(|label| label.name == name)
    }

    fn group_for_label(&self, name: &str) -> Option<&CatalogGroup> {
        self.groups
            .iter()
            .find(|group| name.starts_with(&group.prefix))
    }

    fn is_extension_label(&self, name: &str) -> bool {
        self.group_for_label(name)
            .map(|group| group.allow_extensions)
            .unwrap_or(false)
    }

    fn is_known_shared_label(&self, name: &str) -> bool {
        self.group_for_label(name)
            .map(|group| !group.allow_extensions)
            .unwrap_or(false)
    }

    fn validate_selected_labels(
        &self,
        labels: &[String],
        target: LabelTarget,
    ) -> Result<(), ForgeError> {
        let mut seen_exclusive: BTreeMap<String, String> = BTreeMap::new();
        for name in labels {
            let label = self.label_by_name(name);
            if label.is_none() && !self.is_extension_label(name) {
                return Err(ForgeError::validation(
                    schema_err(),
                    "label_unknown",
                    format!("label '{name}' is not in the catalog"),
                    None,
                ));
            }
            if let Some(label) = label
                && !label.applies_to.is_empty()
                && !label.applies_to.iter().any(|v| v == target.as_str())
            {
                return Err(ForgeError::validation(
                    schema_err(),
                    "label_not_applicable",
                    format!("label '{name}' does not apply to {}", target.as_str()),
                    None,
                ));
            }
            if let Some(group) = self.group_for_label(name)
                && group.exclusive
                && let Some(previous) = seen_exclusive.insert(group.name.clone(), name.to_string())
            {
                return Err(ForgeError::validation(
                    schema_err(),
                    "label_group_conflict",
                    format!(
                        "labels '{previous}' and '{name}' both belong to exclusive group '{}'",
                        group.name
                    ),
                    None,
                ));
            }
        }
        Ok(())
    }
}

fn normalize_color(value: &str) -> String {
    value.trim().trim_start_matches('#').to_ascii_uppercase()
}

fn schema_ok(op: &str) -> String {
    schema_version_for(BINARY, op, SCHEMA_VERSION)
}

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

fn render_list_text(payload: &LabelListPayload) {
    println!("{} labels: {}", payload.provider, payload.labels.len());
}

fn render_audit_text(payload: &LabelAuditPayload) {
    println!(
        "{} label audit: {} (missing={}, drift={}, unknown_shared={})",
        payload.provider,
        payload.status,
        payload.missing.len(),
        payload.drift.len(),
        payload.unknown_shared.len()
    );
}

fn render_ensure_text(payload: &LabelEnsurePayload) {
    println!(
        "{} label ensure: {} action(s){}",
        payload.provider,
        payload.actions.len(),
        if payload.dry_run {
            " planned"
        } else {
            " applied"
        }
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::path::Path;
    use tempfile::TempDir;

    fn group(name: &str, prefix: &str, exclusive: bool, allow_extensions: bool) -> CatalogGroup {
        CatalogGroup {
            name: name.into(),
            prefix: prefix.into(),
            exclusive,
            allow_extensions,
        }
    }

    fn label(name: &str, group: &str, color: &str, description: &str) -> CatalogLabel {
        CatalogLabel {
            name: name.into(),
            group: group.into(),
            color: color.into(),
            description: description.into(),
            applies_to: Vec::new(),
        }
    }

    fn ctx(provider: Provider, repo: Option<&str>) -> ProviderContext {
        ProviderContext {
            provider,
            host: match provider {
                Provider::GitHub => "github.com",
                Provider::GitLab => "gitlab.com",
                Provider::Local => "local",
            }
            .into(),
            source: crate::provider::DetectionSource::Flag,
            repo: repo.map(str::to_string),
        }
    }

    fn argv(call: &BackendCall) -> Vec<String> {
        call.argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn write_catalog(dir: &Path, body: &str) -> String {
        let path = dir.join("forge-labels.yaml");
        std::fs::write(&path, body).expect("write label catalog");
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn normalize_color_drops_hash_and_uppercases() {
        assert_eq!(normalize_color("#d73a4a"), "D73A4A");
    }

    #[test]
    fn parse_list_output_accepts_provider_shapes() {
        let output = BackendSuccess {
            stdout: r##"[
              {"id": 123, "name": "type::bug", "color": "#d73a4a", "description": "Bug"},
              {"name": "area::runtime", "description": null}
            ]"##
            .into(),
            stderr: String::new(),
        };

        let labels = parse_list_output(&ctx(Provider::GitHub, None), &output).expect("parse");

        assert_eq!(
            labels,
            vec![
                ProviderLabel {
                    id: Some("123".into()),
                    name: "type::bug".into(),
                    color: "D73A4A".into(),
                    description: "Bug".into(),
                },
                ProviderLabel {
                    id: None,
                    name: "area::runtime".into(),
                    color: String::new(),
                    description: String::new(),
                },
            ]
        );
    }

    #[test]
    fn parse_list_output_rejects_invalid_json_shapes() {
        let invalid = BackendSuccess {
            stdout: "{not-json".into(),
            stderr: String::new(),
        };
        assert_eq!(
            parse_list_output(&ctx(Provider::GitHub, None), &invalid)
                .expect_err("invalid json")
                .kind(),
            "software_error"
        );

        let object = BackendSuccess {
            stdout: r#"{"labels":[]}"#.into(),
            stderr: String::new(),
        };
        assert_eq!(
            parse_list_output(&ctx(Provider::GitLab, None), &object)
                .expect_err("array required")
                .kind(),
            "software_error"
        );
    }

    #[test]
    fn provider_calls_render_expected_backend_arguments() {
        let expected = label("type::bug", "type", "D73A4A", "Bug");
        let current = ProviderLabel {
            id: Some("42".into()),
            name: "type::bug".into(),
            color: "FFFFFF".into(),
            description: "Old".into(),
        };

        assert_eq!(
            argv(&build_list_call(
                &ctx(Provider::GitHub, Some("sympoies/nils-cli")),
                25
            )),
            vec![
                "label",
                "list",
                "--limit",
                "25",
                "--json",
                "id,name,color,description",
                "--repo",
                "sympoies/nils-cli"
            ]
        );
        assert_eq!(
            argv(&build_create_call(&ctx(Provider::GitLab, None), &expected)),
            vec![
                "label",
                "create",
                "--name",
                "type::bug",
                "--color",
                "#D73A4A",
                "--description",
                "Bug"
            ]
        );
        assert_eq!(
            argv(
                &build_update_call(
                    &ctx(Provider::GitLab, Some("group/project")),
                    &expected,
                    &current,
                )
                .expect("gitlab update")
            ),
            vec![
                "label",
                "edit",
                "--label-id",
                "42",
                "--color",
                "#D73A4A",
                "--description",
                "Bug",
                "--repo",
                "group/project"
            ]
        );
        assert!(
            build_update_call(
                &ctx(Provider::GitLab, None),
                &expected,
                &ProviderLabel {
                    id: None,
                    ..current
                },
            )
            .is_none()
        );
    }

    #[test]
    fn audit_reports_missing_drift_and_unknown_shared_labels() {
        let catalog = Catalog {
            labels: vec![
                label("type::bug", "type", "D73A4A", "Bug"),
                label("status::blocked", "status", "B60205", "Blocked"),
            ],
            groups: vec![
                group("type", "type::", true, false),
                group("status", "status::", true, false),
                group("area", "area::", false, true),
            ],
        };
        let current = vec![
            ProviderLabel {
                id: None,
                name: "type::bug".into(),
                color: "FFFFFF".into(),
                description: "Bug".into(),
            },
            ProviderLabel {
                id: None,
                name: "status::unknown".into(),
                color: "000000".into(),
                description: "Unknown shared".into(),
            },
            ProviderLabel {
                id: None,
                name: "area::repo-local".into(),
                color: "000000".into(),
                description: "Allowed extension".into(),
            },
        ];

        let audit = audit_labels(&catalog, &current);

        assert_eq!(audit.missing[0].name, "status::blocked");
        assert_eq!(audit.drift[0].fields[0].field, "color");
        assert_eq!(audit.unknown_shared[0].name, "status::unknown");
    }

    #[test]
    fn catalog_loader_infers_groups_and_rejects_empty_catalogs() {
        let tempdir = TempDir::new().expect("tempdir");
        let path = write_catalog(
            tempdir.path(),
            r##"
groups:
  - name: type
    prefix: "type::"
labels:
  - name: "type::bug"
    color: "#d73a4a"
    description: Bug
"##,
        );

        let catalog = load_catalog(&path).expect("catalog");
        assert_eq!(catalog.labels[0].group, "type");
        assert_eq!(catalog.labels[0].color, "D73A4A");

        let empty = write_catalog(
            tempdir.path(),
            r##"
groups: []
labels: []
"##,
        );
        assert_eq!(
            load_catalog(&empty).expect_err("empty catalog").kind(),
            "label_catalog_empty"
        );
    }

    #[test]
    fn extension_labels_are_valid_when_group_allows_extensions() {
        let catalog = Catalog {
            labels: vec![CatalogLabel {
                name: "type::bug".into(),
                group: "type".into(),
                color: "D73A4A".into(),
                description: String::new(),
                applies_to: vec!["issue".into()],
            }],
            groups: vec![CatalogGroup {
                name: "area".into(),
                prefix: "area::".into(),
                exclusive: true,
                allow_extensions: true,
            }],
        };
        catalog
            .validate_selected_labels(&["area::local".into()], LabelTarget::Issue)
            .expect("area extension should pass");
    }

    #[test]
    fn exclusive_group_conflicts_are_rejected() {
        let catalog = Catalog {
            labels: Vec::new(),
            groups: vec![CatalogGroup {
                name: "type".into(),
                prefix: "type::".into(),
                exclusive: true,
                allow_extensions: true,
            }],
        };
        let err = catalog
            .validate_selected_labels(
                &["type::bug".into(), "type::feature".into()],
                LabelTarget::Issue,
            )
            .expect_err("exclusive conflict");
        assert_eq!(err.kind(), "label_group_conflict");
    }

    #[test]
    fn validation_rejects_unknown_and_non_applicable_labels() {
        let catalog = Catalog {
            labels: vec![CatalogLabel {
                name: "type::maintenance".into(),
                group: "type".into(),
                color: "0E8A16".into(),
                description: String::new(),
                applies_to: vec!["pr".into()],
            }],
            groups: vec![group("type", "type::", true, false)],
        };

        let unknown = catalog
            .validate_selected_labels(&["area::local".into()], LabelTarget::Pr)
            .expect_err("unknown label");
        assert_eq!(unknown.kind(), "label_unknown");

        let not_applicable = catalog
            .validate_selected_labels(&["type::maintenance".into()], LabelTarget::Issue)
            .expect_err("target mismatch");
        assert_eq!(not_applicable.kind(), "label_not_applicable");
    }
}
