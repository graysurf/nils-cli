use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

use crate::Result;

fn default_auth_required() -> bool {
    true
}

fn default_auth_secret_env() -> String {
    "API_TEST_AUTH_JSON".to_string()
}

/// Canonical jq selector for extracting auth tokens from login responses.
///
/// Shared by:
/// - [`default_auth_token_jq`] for `SuiteRestAuth::token_jq` / `SuiteGraphqlAuth::token_jq`
///   (the schema-level default for explicit `auth.*` blocks).
/// - `suite::runner::context::default_rest_flow_token_jq` (the per-flow default applied
///   when a suite case omits `token_jq`).
///
/// Keep both consumers in sync — see the regression test
/// `default_auth_token_jq_matches_rest_flow_default` in
/// `crates/api-testing-core/src/suite/runner/context.rs`.
pub(super) const DEFAULT_AUTH_TOKEN_JQ: &str = ".. | objects | (.accessToken? // .access_token? // .token? // .jwt? // empty) | select(type==\"string\" and length>0) | .";

fn default_auth_token_jq() -> String {
    DEFAULT_AUTH_TOKEN_JQ.to_string()
}

fn default_rest_config_dir() -> String {
    "setup/rest".to_string()
}

fn default_graphql_config_dir() -> String {
    "setup/graphql".to_string()
}

fn default_grpc_config_dir() -> String {
    "setup/grpc".to_string()
}

fn default_websocket_config_dir() -> String {
    "setup/websocket".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuiteManifest {
    pub version: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub defaults: SuiteDefaults,
    #[serde(default)]
    pub auth: Option<SuiteAuth>,
    pub cases: Vec<SuiteCase>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SuiteDefaults {
    #[serde(default)]
    pub env: String,
    #[serde(default)]
    pub no_history: bool,
    #[serde(default)]
    pub rest: SuiteDefaultsRest,
    #[serde(default)]
    pub graphql: SuiteDefaultsGraphql,
    #[serde(default)]
    pub grpc: SuiteDefaultsGrpc,
    #[serde(default)]
    pub websocket: SuiteDefaultsWebsocket,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuiteDefaultsRest {
    #[serde(default = "default_rest_config_dir")]
    pub config_dir: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub token: String,
}

impl Default for SuiteDefaultsRest {
    fn default() -> Self {
        Self {
            config_dir: default_rest_config_dir(),
            url: String::new(),
            token: String::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuiteDefaultsGraphql {
    #[serde(default = "default_graphql_config_dir")]
    pub config_dir: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub jwt: String,
}

impl Default for SuiteDefaultsGraphql {
    fn default() -> Self {
        Self {
            config_dir: default_graphql_config_dir(),
            url: String::new(),
            jwt: String::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuiteDefaultsGrpc {
    #[serde(default = "default_grpc_config_dir")]
    pub config_dir: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub token: String,
}

impl Default for SuiteDefaultsGrpc {
    fn default() -> Self {
        Self {
            config_dir: default_grpc_config_dir(),
            url: String::new(),
            token: String::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuiteDefaultsWebsocket {
    #[serde(default = "default_websocket_config_dir")]
    pub config_dir: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub token: String,
}

impl Default for SuiteDefaultsWebsocket {
    fn default() -> Self {
        Self {
            config_dir: default_websocket_config_dir(),
            url: String::new(),
            token: String::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuiteAuth {
    #[serde(default)]
    pub provider: String,
    #[serde(default = "default_auth_required")]
    pub required: bool,
    #[serde(default = "default_auth_secret_env")]
    pub secret_env: String,
    #[serde(default)]
    pub rest: Option<SuiteAuthRest>,
    #[serde(default)]
    pub graphql: Option<SuiteAuthGraphql>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuiteAuthRest {
    pub login_request_template: String,
    pub credentials_jq: String,
    #[serde(default = "default_auth_token_jq")]
    pub token_jq: String,
    #[serde(default)]
    pub config_dir: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub env: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuiteAuthGraphql {
    pub login_op: String,
    pub login_vars_template: String,
    pub credentials_jq: String,
    #[serde(default = "default_auth_token_jq")]
    pub token_jq: String,
    #[serde(default)]
    pub config_dir: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub env: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuiteCase {
    pub id: String,
    #[serde(rename = "type")]
    pub case_type: String,

    #[serde(default)]
    pub tags: Vec<String>,

    #[serde(default)]
    pub env: String,

    #[serde(default)]
    pub no_history: Option<bool>,

    #[serde(default)]
    pub allow_write: bool,

    // Shared config overrides (meaning depends on type).
    #[serde(default)]
    pub config_dir: String,
    #[serde(default)]
    pub url: String,

    // REST case fields
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub request: String,

    // gRPC case fields
    #[serde(default)]
    pub grpc_proto: Option<String>,
    #[serde(default)]
    pub grpc_import_paths: Option<Vec<String>>,
    #[serde(default)]
    pub grpc_plaintext: Option<bool>,

    // REST-flow case fields
    #[serde(default)]
    pub login_request: String,
    #[serde(default)]
    pub token_jq: String,

    // GraphQL case fields
    #[serde(default)]
    pub jwt: String,
    #[serde(default)]
    pub op: String,
    #[serde(default)]
    pub vars: Option<String>,
    #[serde(default)]
    pub allow_errors: bool,
    #[serde(default)]
    pub expect: Option<SuiteGraphqlExpect>,

    #[serde(default)]
    pub cleanup: Option<SuiteCleanup>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuiteGraphqlExpect {
    #[serde(default)]
    pub jq: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum SuiteCleanup {
    One(Box<SuiteCleanupStep>),
    Many(Vec<SuiteCleanupStep>),
}

impl SuiteCleanup {
    pub fn steps(&self) -> Vec<SuiteCleanupStep> {
        match self {
            Self::One(step) => vec![step.as_ref().clone()],
            Self::Many(steps) => steps.clone(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuiteCleanupStep {
    #[serde(rename = "type")]
    pub step_type: String,

    #[serde(default)]
    pub config_dir: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub env: String,
    #[serde(default)]
    pub no_history: Option<bool>,

    // REST cleanup fields
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub path_template: String,
    #[serde(default)]
    pub vars: Option<serde_json::Value>,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub expect: Option<SuiteCleanupExpect>,
    #[serde(default)]
    pub expect_status: Option<u16>,
    #[serde(default)]
    pub expect_jq: String,

    // GraphQL cleanup fields
    #[serde(default)]
    pub jwt: String,
    #[serde(default)]
    pub op: String,
    #[serde(default)]
    pub vars_jq: String,
    #[serde(default)]
    pub vars_template: String,
    #[serde(default)]
    pub allow_errors: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuiteCleanupExpect {
    #[serde(default)]
    pub status: Option<u16>,
    #[serde(default)]
    pub jq: String,
}

fn is_valid_env_var_name(raw: &str) -> bool {
    let mut chars = raw.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn schema_error(
    path: &str,
    case_id: Option<&str>,
    message: impl std::fmt::Display,
) -> anyhow::Error {
    match case_id {
        Some(id) if !id.trim().is_empty() => {
            anyhow::anyhow!("Suite schema error at {path} (case {id}): {message}")
        }
        _ => anyhow::anyhow!("Suite schema error at {path}: {message}"),
    }
}

fn canonical_case_type(raw: &str) -> String {
    let normalized = raw.trim().to_ascii_lowercase();
    if normalized == "ws" {
        "websocket".to_string()
    } else {
        normalized
    }
}

pub fn load_suite_manifest(path: impl AsRef<Path>) -> Result<SuiteManifest> {
    let path = path.as_ref();
    let bytes =
        std::fs::read(path).with_context(|| format!("read suite file: {}", path.display()))?;

    let manifest: SuiteManifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("Suite file is not valid JSON: {}", path.display()))?;
    Ok(manifest)
}

pub fn validate_suite_manifest(manifest: &SuiteManifest, suite_path: &Path) -> Result<()> {
    if manifest.version != 1 {
        anyhow::bail!(
            "Unsupported suite version: {} (expected 1): {}",
            manifest.version,
            suite_path.display()
        );
    }

    if let Some(auth) = &manifest.auth {
        let secret_env = auth.secret_env.trim();
        if secret_env.is_empty() {
            return Err(schema_error("auth.secretEnv", None, "must not be empty"));
        }
        if !is_valid_env_var_name(secret_env) {
            return Err(schema_error(
                "auth.secretEnv",
                None,
                "must be a valid env var name",
            ));
        }

        let provider_raw = auth.provider.trim().to_ascii_lowercase();
        let provider = if provider_raw.is_empty() {
            match (&auth.rest, &auth.graphql) {
                (Some(_), None) => "rest".to_string(),
                (None, Some(_)) => "graphql".to_string(),
                (Some(_), Some(_)) => {
                    return Err(schema_error(
                        "auth.provider",
                        None,
                        "is required when both auth.rest and auth.graphql are present",
                    ));
                }
                (None, None) => {
                    return Err(schema_error(
                        "auth",
                        None,
                        "must include either auth.rest or auth.graphql",
                    ));
                }
            }
        } else if provider_raw == "gql" {
            "graphql".to_string()
        } else {
            provider_raw
        };

        match provider.as_str() {
            "rest" => {
                let Some(rest) = &auth.rest else {
                    return Err(schema_error(
                        "auth.rest",
                        None,
                        "is required for provider=rest",
                    ));
                };
                if rest.login_request_template.trim().is_empty() {
                    return Err(schema_error(
                        "auth.rest.loginRequestTemplate",
                        None,
                        "is required",
                    ));
                }
                if rest.credentials_jq.trim().is_empty() {
                    return Err(schema_error("auth.rest.credentialsJq", None, "is required"));
                }
            }
            "graphql" => {
                let Some(graphql) = &auth.graphql else {
                    return Err(schema_error(
                        "auth.graphql",
                        None,
                        "is required for provider=graphql",
                    ));
                };
                if graphql.login_op.trim().is_empty() {
                    return Err(schema_error("auth.graphql.loginOp", None, "is required"));
                }
                if graphql.login_vars_template.trim().is_empty() {
                    return Err(schema_error(
                        "auth.graphql.loginVarsTemplate",
                        None,
                        "is required",
                    ));
                }
                if graphql.credentials_jq.trim().is_empty() {
                    return Err(schema_error(
                        "auth.graphql.credentialsJq",
                        None,
                        "is required",
                    ));
                }
            }
            _ => {
                return Err(schema_error(
                    "auth.provider",
                    None,
                    "must be one of: rest, graphql",
                ));
            }
        }
    }

    let mut seen_ids: HashSet<String> = HashSet::new();
    for (i, c) in manifest.cases.iter().enumerate() {
        let id = c.id.trim();
        if id.is_empty() {
            return Err(schema_error(&format!("cases[{i}].id"), None, "is required"));
        }
        if !seen_ids.insert(id.to_string()) {
            return Err(schema_error(
                &format!("cases[{i}].id"),
                Some(id),
                "must be unique",
            ));
        }

        let ty = canonical_case_type(&c.case_type);
        if ty.is_empty() {
            return Err(schema_error(
                &format!("cases[{i}].type"),
                Some(id),
                "is required",
            ));
        }

        match ty.as_str() {
            "rest" => {
                if c.request.trim().is_empty() {
                    return Err(schema_error(
                        &format!("cases[{i}].request"),
                        Some(id),
                        "is required for type=rest",
                    ));
                }
            }
            "rest-flow" | "rest_flow" => {
                if c.login_request.trim().is_empty() {
                    return Err(schema_error(
                        &format!("cases[{i}].loginRequest"),
                        Some(id),
                        "is required for type=rest-flow",
                    ));
                }
                if c.request.trim().is_empty() {
                    return Err(schema_error(
                        &format!("cases[{i}].request"),
                        Some(id),
                        "is required for type=rest-flow",
                    ));
                }
            }
            "graphql" => {
                if c.op.trim().is_empty() {
                    return Err(schema_error(
                        &format!("cases[{i}].op"),
                        Some(id),
                        "is required for type=graphql",
                    ));
                }

                if c.allow_errors {
                    let expect_jq = c.expect.as_ref().map(|e| e.jq.trim()).unwrap_or_default();
                    if expect_jq.is_empty() {
                        return Err(schema_error(
                            &format!("cases[{i}].expect.jq"),
                            Some(id),
                            "allowErrors=true requires expect.jq",
                        ));
                    }
                }
            }
            "grpc" => {
                if c.request.trim().is_empty() {
                    return Err(schema_error(
                        &format!("cases[{i}].request"),
                        Some(id),
                        "is required for type=grpc",
                    ));
                }
            }
            "websocket" => {
                if c.request.trim().is_empty() {
                    return Err(schema_error(
                        &format!("cases[{i}].request"),
                        Some(id),
                        "is required for type=websocket",
                    ));
                }
            }
            other => {
                return Err(schema_error(
                    &format!("cases[{i}].type"),
                    Some(id),
                    format!("unknown case type: {other}"),
                ));
            }
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
pub struct LoadedSuite {
    pub suite_path: PathBuf,
    pub manifest: SuiteManifest,
}

pub fn load_and_validate_suite(path: impl AsRef<Path>) -> Result<LoadedSuite> {
    let path = path.as_ref();
    let manifest = load_suite_manifest(path)?;
    let suite_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    validate_suite_manifest(&manifest, &suite_path)?;
    Ok(LoadedSuite {
        suite_path,
        manifest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    fn write_suite(tmp: &TempDir, value: &serde_json::Value) -> PathBuf {
        let path = tmp.path().join("suite.json");
        std::fs::write(&path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
        path
    }

    fn base_rest_case() -> serde_json::Value {
        serde_json::json!({
          "id": "rest.health",
          "type": "rest",
          "request": "setup/rest/requests/health.request.json"
        })
    }

    fn validate_err(value: serde_json::Value) -> String {
        let tmp = TempDir::new().unwrap();
        let path = write_suite(&tmp, &value);
        let err = load_and_validate_suite(&path).unwrap_err();
        format!("{err:#}")
    }

    #[test]
    fn suite_schema_rejects_unsupported_version() {
        let err = validate_err(serde_json::json!({
          "version": 2,
          "cases": [base_rest_case()]
        }));
        assert!(err.contains("Unsupported suite version"));
    }

    #[test]
    fn suite_schema_rejects_empty_auth_secret_env() {
        let err = validate_err(serde_json::json!({
          "version": 1,
          "auth": { "secretEnv": "   " },
          "cases": [base_rest_case()]
        }));
        assert!(err.contains("auth.secretEnv"));
        assert!(err.contains("must not be empty"));
    }

    #[test]
    fn suite_schema_rejects_invalid_auth_secret_env() {
        let err = validate_err(serde_json::json!({
          "version": 1,
          "auth": { "secretEnv": "123" },
          "cases": [base_rest_case()]
        }));
        assert!(err.contains("auth.secretEnv"));
        assert!(err.contains("valid env var name"));
    }

    #[test]
    fn suite_schema_requires_provider_when_both_auth_blocks_present() {
        let err = validate_err(serde_json::json!({
          "version": 1,
          "auth": {
            "rest": {
              "loginRequestTemplate": "setup/rest/requests/login.request.json",
              "credentialsJq": ".profiles[$profile]"
            },
            "graphql": {
              "loginOp": "setup/graphql/operations/login.graphql",
              "loginVarsTemplate": "setup/graphql/vars/login.json",
              "credentialsJq": ".profiles[$profile]"
            }
          },
          "cases": [base_rest_case()]
        }));
        assert!(err.contains("auth.provider"));
        assert!(err.contains("both auth.rest and auth.graphql"));
    }

    #[test]
    fn suite_schema_rejects_rest_auth_missing_login_request_template() {
        let err = validate_err(serde_json::json!({
          "version": 1,
          "auth": {
            "provider": "rest",
            "rest": {
              "loginRequestTemplate": " ",
              "credentialsJq": ".profiles[$profile]"
            }
          },
          "cases": [base_rest_case()]
        }));
        assert!(err.contains("auth.rest.loginRequestTemplate"));
    }

    #[test]
    fn suite_schema_rejects_rest_auth_missing_credentials_jq() {
        let err = validate_err(serde_json::json!({
          "version": 1,
          "auth": {
            "provider": "rest",
            "rest": {
              "loginRequestTemplate": "setup/rest/requests/login.request.json",
              "credentialsJq": " "
            }
          },
          "cases": [base_rest_case()]
        }));
        assert!(err.contains("auth.rest.credentialsJq"));
    }

    #[test]
    fn suite_schema_rejects_graphql_auth_missing_login_op() {
        let err = validate_err(serde_json::json!({
          "version": 1,
          "auth": {
            "provider": "graphql",
            "graphql": {
              "loginOp": " ",
              "loginVarsTemplate": "setup/graphql/vars/login.json",
              "credentialsJq": ".profiles[$profile]"
            }
          },
          "cases": [base_rest_case()]
        }));
        assert!(err.contains("auth.graphql.loginOp"));
    }

    #[test]
    fn suite_schema_rejects_graphql_auth_missing_login_vars_template() {
        let err = validate_err(serde_json::json!({
          "version": 1,
          "auth": {
            "provider": "graphql",
            "graphql": {
              "loginOp": "setup/graphql/operations/login.graphql",
              "loginVarsTemplate": " ",
              "credentialsJq": ".profiles[$profile]"
            }
          },
          "cases": [base_rest_case()]
        }));
        assert!(err.contains("auth.graphql.loginVarsTemplate"));
    }

    #[test]
    fn suite_schema_rejects_graphql_auth_missing_credentials_jq() {
        let err = validate_err(serde_json::json!({
          "version": 1,
          "auth": {
            "provider": "graphql",
            "graphql": {
              "loginOp": "setup/graphql/operations/login.graphql",
              "loginVarsTemplate": "setup/graphql/vars/login.json",
              "credentialsJq": " "
            }
          },
          "cases": [base_rest_case()]
        }));
        assert!(err.contains("auth.graphql.credentialsJq"));
    }

    #[test]
    fn suite_schema_rejects_unknown_auth_provider() {
        let err = validate_err(serde_json::json!({
          "version": 1,
          "auth": { "provider": "soap" },
          "cases": [base_rest_case()]
        }));
        assert!(err.contains("auth.provider"));
        assert!(err.contains("rest, graphql"));
    }

    #[test]
    fn suite_schema_rejects_empty_case_id() {
        let err = validate_err(serde_json::json!({
          "version": 1,
          "cases": [
            { "id": " ", "type": "rest", "request": "setup/rest/requests/health.request.json" }
          ]
        }));
        assert!(err.contains("cases[0].id"));
        assert!(err.contains("is required"));
    }

    #[test]
    fn suite_schema_rejects_duplicate_case_ids() {
        let err = validate_err(serde_json::json!({
          "version": 1,
          "cases": [
            { "id": "dup", "type": "rest", "request": "setup/rest/requests/health.request.json" },
            { "id": "dup", "type": "rest", "request": "setup/rest/requests/health.request.json" }
          ]
        }));
        assert!(err.contains("cases[1].id"));
        assert!(err.contains("must be unique"));
    }

    #[test]
    fn suite_schema_rejects_empty_case_type() {
        let err = validate_err(serde_json::json!({
          "version": 1,
          "cases": [
            { "id": "x", "type": " ", "request": "setup/rest/requests/health.request.json" }
          ]
        }));
        assert!(err.contains("cases[0].type"));
        assert!(err.contains("is required"));
    }

    #[test]
    fn suite_schema_rejects_rest_case_missing_request() {
        let err = validate_err(serde_json::json!({
          "version": 1,
          "cases": [
            { "id": "rest.missing", "type": "rest" }
          ]
        }));
        assert!(err.contains("cases[0].request"));
        assert!(err.contains("type=rest"));
    }

    #[test]
    fn suite_schema_rejects_rest_flow_missing_login_request() {
        let err = validate_err(serde_json::json!({
          "version": 1,
          "cases": [
            { "id": "rest.flow", "type": "rest-flow", "request": "setup/rest/requests/health.request.json" }
          ]
        }));
        assert!(err.contains("cases[0].loginRequest"));
        assert!(err.contains("type=rest-flow"));
    }

    #[test]
    fn suite_schema_rejects_rest_flow_missing_request() {
        let err = validate_err(serde_json::json!({
          "version": 1,
          "cases": [
            { "id": "rest.flow", "type": "rest-flow", "loginRequest": "setup/rest/requests/login.request.json" }
          ]
        }));
        assert!(err.contains("cases[0].request"));
        assert!(err.contains("type=rest-flow"));
    }

    #[test]
    fn suite_schema_rejects_graphql_case_missing_op() {
        let err = validate_err(serde_json::json!({
          "version": 1,
          "cases": [
            { "id": "graphql.missing", "type": "graphql" }
          ]
        }));
        assert!(err.contains("cases[0].op"));
        assert!(err.contains("type=graphql"));
    }

    #[test]
    fn suite_schema_rejects_grpc_case_missing_request() {
        let err = validate_err(serde_json::json!({
          "version": 1,
          "cases": [
            { "id": "grpc.missing", "type": "grpc" }
          ]
        }));
        assert!(err.contains("cases[0].request"));
        assert!(err.contains("type=grpc"));
    }

    #[test]
    fn suite_schema_rejects_websocket_case_missing_request() {
        let err = validate_err(serde_json::json!({
          "version": 1,
          "cases": [
            { "id": "ws.missing", "type": "websocket" }
          ]
        }));
        assert!(err.contains("cases[0].request"));
        assert!(err.contains("type=websocket"));
    }

    #[test]
    fn suite_cleanup_steps_supports_single_and_many() {
        let one = SuiteCleanup::One(Box::new(SuiteCleanupStep {
            step_type: "rest".to_string(),
            config_dir: String::new(),
            url: String::new(),
            env: String::new(),
            no_history: None,
            method: "DELETE".to_string(),
            path_template: "/health".to_string(),
            vars: None,
            token: String::new(),
            expect: None,
            expect_status: None,
            expect_jq: String::new(),
            jwt: String::new(),
            op: String::new(),
            vars_jq: String::new(),
            vars_template: String::new(),
            allow_errors: false,
        }));
        let many = SuiteCleanup::Many(vec![one.steps()[0].clone()]);

        assert_eq!(one.steps().len(), 1);
        assert_eq!(many.steps().len(), 1);
    }

    #[test]
    fn suite_schema_rejects_allow_errors_true_without_expect_jq() {
        let tmp = TempDir::new().unwrap();
        let path = write_suite(
            &tmp,
            &serde_json::json!({
              "version": 1,
              "name": "smoke",
              "cases": [
                {
                  "id": "graphql.countries",
                  "type": "graphql",
                  "allowErrors": true,
                  "op": "setup/graphql/ops/countries.graphql"
                }
              ]
            }),
        );

        let err = load_and_validate_suite(&path).unwrap_err();
        assert!(format!("{err:#}").contains("graphql.countries"));
        assert!(format!("{err:#}").contains("allowErrors=true requires expect.jq"));
    }

    #[test]
    fn suite_schema_unknown_type_includes_case_id() {
        let tmp = TempDir::new().unwrap();
        let path = write_suite(
            &tmp,
            &serde_json::json!({
              "version": 1,
              "cases": [
                { "id": "x", "type": "nope" }
              ]
            }),
        );
        let err = load_and_validate_suite(&path).unwrap_err();
        assert!(format!("{err:#}").contains("case x"));
        assert!(format!("{err:#}").contains("unknown case type"));
    }
}
