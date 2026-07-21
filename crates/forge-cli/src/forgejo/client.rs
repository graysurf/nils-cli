use std::cmp;
use std::io::Read;
use std::time::Duration;

use nils_common::cli_contract::schema_version_for;
use reqwest::Method;
use reqwest::blocking::{Client, Response};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderValue, USER_AGENT};
use reqwest::redirect::Policy;
use url::{Position, Url};

use crate::cli::{BINARY, GlobalFlags, IssueListArgs, RepoBootstrapOwnerKind};
use crate::error::ForgeError;
use crate::provider_registry::{ProviderKind, ProviderRecord};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const MIN_SUPPORTED_MAJOR: u64 = 15;
const MAX_SUPPORTED_MAJOR: u64 = 16;
const TIMEOUT_ENV: &str = "FORGE_CLI_FORGEJO_TIMEOUT_MS";
pub(super) const ISSUE_PAGE_SIZE: u32 = 50;

pub(crate) struct ForgejoClient {
    name: String,
    base_url: Url,
    authority: String,
    token: String,
    token_env: String,
    client: Client,
}

impl ForgejoClient {
    pub(crate) fn from_global(global: &GlobalFlags) -> Result<Self, ForgeError> {
        let name = global.named_provider().ok_or_else(|| {
            ForgeError::provider_unsupported(
                schema(),
                "Forgejo HTTP operations require a named provider",
                None,
            )
        })?;
        let record = crate::provider_registry::get(name)?;
        Self::new(name, record)
    }

    fn new(name: &str, record: ProviderRecord) -> Result<Self, ForgeError> {
        if record.kind != ProviderKind::Forgejo {
            return Err(ForgeError::provider_unsupported(
                schema(),
                format!("named provider '{name}' is not a Forgejo provider"),
                None,
            ));
        }
        let base_url = Url::parse(&record.base_url).map_err(|_| {
            ForgeError::validation(
                schema(),
                "provider_base_url_invalid",
                "registered provider base URL is invalid",
                None,
            )
        })?;
        let authority = base_url[Position::BeforeHost..Position::AfterPort].to_string();
        let token = std::env::var(&record.token_env)
            .ok()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                ForgeError::backend_unauthenticated(
                    schema(),
                    format!(
                        "named provider '{name}' requires token environment variable ${}",
                        record.token_env
                    ),
                    None,
                )
            })?;
        let timeout = request_timeout();
        let origin = Origin::from_url(&base_url);
        let client = Client::builder()
            .connect_timeout(cmp::min(CONNECT_TIMEOUT, timeout))
            .timeout(timeout)
            .redirect(Policy::custom(move |attempt| {
                if attempt.previous().len() >= MAX_REDIRECTS {
                    return attempt.error("Forgejo redirect limit exceeded");
                }
                if origin.matches(attempt.url()) {
                    attempt.follow()
                } else {
                    attempt.stop()
                }
            }))
            .build()
            .map_err(|error| {
                ForgeError::unavailable(
                    schema(),
                    "forgejo_client_unavailable",
                    "failed to initialize the Forgejo HTTP client",
                    Some(redact(&error.to_string(), &token)),
                )
            })?;
        Ok(Self {
            name: name.to_string(),
            base_url,
            authority,
            token,
            token_env: record.token_env,
            client,
        })
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn authority(&self) -> &str {
        &self.authority
    }

    pub(crate) fn token_env(&self) -> &str {
        &self.token_env
    }

    pub(crate) fn discover_version(&self) -> Result<(), ForgeError> {
        let url = self.endpoint(&["api", "v1", "version"])?;
        let value = self.get_json(url)?;
        let version = value
            .get("version")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                ForgeError::software(
                    schema(),
                    "Forgejo version response missing required field: version",
                    None,
                )
            })?;
        let major = version
            .split('.')
            .next()
            .and_then(|part| part.parse::<u64>().ok())
            .ok_or_else(|| unsupported_version(version))?;
        if !(MIN_SUPPORTED_MAJOR..=MAX_SUPPORTED_MAJOR).contains(&major) {
            return Err(unsupported_version(version));
        }
        Ok(())
    }

    pub(crate) fn authenticated_user(&self) -> Result<String, ForgeError> {
        let url = self.endpoint(&["api", "v1", "user"])?;
        let value = self.get_json(url)?;
        value
            .get("login")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                ForgeError::software(
                    schema(),
                    "Forgejo auth response missing required field: login",
                    None,
                )
            })
    }

    pub(crate) fn repo(&self, owner: &str, repo: &str) -> Result<serde_json::Value, ForgeError> {
        let url = self.endpoint(&["api", "v1", "repos", owner, repo])?;
        self.get_json(url)
    }

    pub(crate) fn repo_optional(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<Option<serde_json::Value>, ForgeError> {
        let url = self.endpoint(&["api", "v1", "repos", owner, repo])?;
        self.request_json(Method::GET, url, None, true)
    }

    pub(crate) fn create_repo(
        &self,
        owner_kind: RepoBootstrapOwnerKind,
        owner: &str,
        repo: &str,
    ) -> Result<serde_json::Value, ForgeError> {
        let url = match owner_kind {
            RepoBootstrapOwnerKind::User => self.endpoint(&["api", "v1", "user", "repos"]),
            RepoBootstrapOwnerKind::Org => self.endpoint(&["api", "v1", "orgs", owner, "repos"]),
        }?;
        let body = serde_json::json!({
            "name": repo,
            "private": true,
            "auto_init": false
        });
        self.request_json(Method::POST, url, Some(&body), false)?
            .ok_or_else(|| {
                ForgeError::software(schema(), "Forgejo create response was empty", None)
            })
    }

    pub(crate) fn branch_optional(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
    ) -> Result<Option<serde_json::Value>, ForgeError> {
        let url = self.endpoint(&["api", "v1", "repos", owner, repo, "branches", branch])?;
        self.request_json(Method::GET, url, None, true)
    }

    pub(crate) fn update_default_branch(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
    ) -> Result<serde_json::Value, ForgeError> {
        let url = self.endpoint(&["api", "v1", "repos", owner, repo])?;
        let body = serde_json::json!({"default_branch": branch});
        self.request_json(Method::PATCH, url, Some(&body), false)?
            .ok_or_else(|| {
                ForgeError::software(schema(), "Forgejo update response was empty", None)
            })
    }

    pub(crate) fn commit(
        &self,
        owner: &str,
        repo: &str,
        sha: &str,
    ) -> Result<serde_json::Value, ForgeError> {
        let url = self.endpoint(&["api", "v1", "repos", owner, repo, "git", "commits", sha])?;
        self.request_json(Method::GET, url, None, false)?
            .ok_or_else(|| {
                ForgeError::software(schema(), "Forgejo commit response was empty", None)
            })
    }

    pub(crate) fn same_origin_clone_url(&self, raw: &str) -> Result<String, ForgeError> {
        let url = Url::parse(raw).map_err(|_| invalid_clone_url())?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !Origin::from_url(&self.base_url).matches(&url)
        {
            return Err(invalid_clone_url());
        }
        Ok(url.to_string())
    }

    pub(crate) fn issues(
        &self,
        owner: &str,
        repo: &str,
        args: &IssueListArgs,
        page: u32,
    ) -> Result<Vec<serde_json::Value>, ForgeError> {
        let mut url = self.endpoint(&["api", "v1", "repos", owner, repo, "issues"])?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("state", args.state.as_str());
            query.append_pair("type", "issues");
            query.append_pair("page", &page.to_string());
            query.append_pair("limit", &ISSUE_PAGE_SIZE.to_string());
            if !args.labels.is_empty() {
                query.append_pair("labels", &args.labels.join(","));
            }
            if let Some(author) = args.author.as_deref() {
                query.append_pair("created_by", author);
            }
            if let Some(assignee) = args.assignee.as_deref() {
                query.append_pair("assigned_by", assignee);
            }
        }
        let value = self.get_json(url)?;
        value.as_array().cloned().ok_or_else(|| {
            ForgeError::software(
                schema(),
                "Forgejo issue list response is not an array",
                None,
            )
        })
    }

    fn endpoint(&self, segments: &[&str]) -> Result<Url, ForgeError> {
        let mut url = self.base_url.clone();
        {
            let mut path = url.path_segments_mut().map_err(|_| {
                ForgeError::validation(
                    schema(),
                    "provider_base_url_invalid",
                    "registered provider base URL cannot contain API path segments",
                    None,
                )
            })?;
            path.pop_if_empty();
            for segment in segments {
                path.push(segment);
            }
        }
        Ok(url)
    }

    fn get_json(&self, url: Url) -> Result<serde_json::Value, ForgeError> {
        self.request_json(Method::GET, url, None, false)?
            .ok_or_else(|| ForgeError::software(schema(), "Forgejo response was empty", None))
    }

    fn request_json(
        &self,
        method: Method,
        url: Url,
        body: Option<&serde_json::Value>,
        allow_not_found: bool,
    ) -> Result<Option<serde_json::Value>, ForgeError> {
        let mut authorization =
            HeaderValue::from_str(&format!("token {}", self.token)).map_err(|_| {
                ForgeError::backend_unauthenticated(
                    schema(),
                    format!(
                        "named provider '{}' token is not a valid HTTP credential",
                        self.name
                    ),
                    None,
                )
            })?;
        authorization.set_sensitive(true);
        let mut request = self
            .client
            .request(method, url.clone())
            .header(ACCEPT, "application/json")
            .header(
                USER_AGENT,
                concat!("nils-forge-cli/", env!("CARGO_PKG_VERSION")),
            )
            .header(AUTHORIZATION, authorization);
        if let Some(body) = body {
            request = request
                .header(CONTENT_TYPE, "application/json")
                .body(body.to_string());
        }
        let response = request
            .send()
            .map_err(|error| self.transport_error(error))?;
        if allow_not_found && response.status().as_u16() == 404 {
            return Ok(None);
        }
        self.parse_response(response, &url).map(Some)
    }

    fn parse_response(
        &self,
        response: Response,
        request_url: &Url,
    ) -> Result<serde_json::Value, ForgeError> {
        let status = response.status();
        if status.is_redirection() {
            return Err(ForgeError::unavailable(
                schema(),
                "forgejo_redirect_forbidden",
                "Forgejo refused a cross-origin redirect",
                Some(format!("status={status}; url={request_url}")),
            ));
        }
        if matches!(status.as_u16(), 401 | 403) {
            return Err(ForgeError::backend_unauthenticated(
                schema(),
                format!(
                    "Forgejo rejected authentication for provider '{}'",
                    self.name
                ),
                Some(format!("status={status}; url={request_url}")),
            ));
        }
        if !status.is_success() {
            return Err(ForgeError::backend_error(
                schema(),
                format!("Forgejo request failed for provider '{}'", self.name),
                Some(format!("status={status}; url={request_url}")),
            ));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(response_too_large());
        }
        let mut body = Vec::new();
        response
            .take((MAX_RESPONSE_BYTES + 1) as u64)
            .read_to_end(&mut body)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::TimedOut {
                    timeout_error()
                } else {
                    ForgeError::unavailable(
                        schema(),
                        "forgejo_transport_error",
                        "failed to read the Forgejo response",
                        Some(redact(&error.to_string(), &self.token)),
                    )
                }
            })?;
        if body.len() > MAX_RESPONSE_BYTES {
            return Err(response_too_large());
        }
        serde_json::from_slice(&body).map_err(|error| {
            ForgeError::software(
                schema(),
                "Forgejo response is not valid JSON",
                Some(redact(&error.to_string(), &self.token)),
            )
        })
    }

    fn transport_error(&self, error: reqwest::Error) -> ForgeError {
        if error.is_timeout() {
            timeout_error()
        } else {
            ForgeError::unavailable(
                schema(),
                "forgejo_transport_error",
                format!("Forgejo request failed for provider '{}'", self.name),
                Some(redact(&error.to_string(), &self.token)),
            )
        }
    }
}

#[derive(Clone)]
struct Origin {
    scheme: String,
    host: String,
    port: Option<u16>,
}

impl Origin {
    fn from_url(url: &Url) -> Self {
        Self {
            scheme: url.scheme().to_string(),
            host: url.host_str().unwrap_or_default().to_ascii_lowercase(),
            port: url.port_or_known_default(),
        }
    }

    fn matches(&self, url: &Url) -> bool {
        self.scheme == url.scheme()
            && self.host == url.host_str().unwrap_or_default().to_ascii_lowercase()
            && self.port == url.port_or_known_default()
    }
}

fn request_timeout() -> Duration {
    std::env::var(TIMEOUT_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0 && *value <= 120_000)
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_TIMEOUT)
}

fn unsupported_version(version: &str) -> ForgeError {
    ForgeError::unavailable(
        schema(),
        "forgejo_version_unsupported",
        format!("Forgejo version '{version}' is outside the supported major-version range"),
        Some(format!(
            "supported={MIN_SUPPORTED_MAJOR}..={MAX_SUPPORTED_MAJOR}"
        )),
    )
}

fn timeout_error() -> ForgeError {
    ForgeError::unavailable(
        schema(),
        "forgejo_timeout",
        "Forgejo request exceeded its deadline",
        None,
    )
}

fn response_too_large() -> ForgeError {
    ForgeError::unavailable(
        schema(),
        "forgejo_response_too_large",
        format!("Forgejo response exceeded {MAX_RESPONSE_BYTES} bytes"),
        None,
    )
}

fn invalid_clone_url() -> ForgeError {
    ForgeError::validation(
        schema(),
        "bootstrap_clone_url_invalid",
        "Forgejo clone URL must be same-origin HTTP(S) without embedded credentials, query, or fragment",
        None,
    )
}

fn redact(value: &str, token: &str) -> String {
    if token.is_empty() {
        value.to_string()
    } else {
        value.replace(token, "[REDACTED]")
    }
}

fn schema() -> String {
    schema_version_for(BINARY, "error", 1)
}
