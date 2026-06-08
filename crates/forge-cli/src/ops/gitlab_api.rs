//! Small helpers for GitLab REST calls made through `glab api`.
//!
//! Keep endpoint construction here so GitLab-specific API paths do not spread
//! across PR/MR atoms.

use std::ffi::OsString;

use crate::backend::{BackendCall, BackendProgram};
use crate::provider::ProviderContext;

pub(crate) fn api_call(host: &str, path: impl Into<String>) -> BackendCall {
    BackendCall::new(
        BackendProgram::Glab,
        [
            OsString::from("api"),
            OsString::from("--hostname"),
            OsString::from(host),
            OsString::from(path.into()),
        ],
    )
}

pub(crate) fn api_call_with_method_fields(
    host: &str,
    method: &str,
    path: impl Into<String>,
    fields: &[(&str, String)],
) -> BackendCall {
    let mut argv = vec![
        OsString::from("api"),
        OsString::from("--method"),
        OsString::from(method),
        OsString::from("--hostname"),
        OsString::from(host),
        OsString::from(path.into()),
    ];
    for (key, value) in fields {
        argv.push(OsString::from("--field"));
        argv.push(OsString::from(format!("{key}={value}")));
    }
    BackendCall::new(BackendProgram::Glab, argv)
}

pub(crate) fn host_from_url(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest)?;
    let host = after_scheme.split_once('/').map(|(host, _)| host)?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// Extract `group[/subgroup]/project` from a GitLab MR URL such as
/// `https://gitlab.example.com/group/project/-/merge_requests/12`.
pub(crate) fn project_path_from_mr_url(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let path = after_scheme.split_once('/').map(|(_, rest)| rest)?;
    let path = path.trim_end_matches('/');
    let idx = path
        .find("/-/merge_requests/")
        .or_else(|| path.find("/merge_requests/"))?;
    let project_path = &path[..idx];
    if project_path.is_empty() {
        None
    } else {
        Some(project_path.to_string())
    }
}

pub(crate) fn project_path_from_ctx(ctx: &ProviderContext) -> Option<&str> {
    ctx.repo.as_deref()
}

pub(crate) fn encode_project_path(path: &str) -> String {
    percent_encode(path)
}

pub(crate) fn encode_query_value(value: &str) -> String {
    percent_encode(value)
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(b));
            }
            _ => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                out.push('%');
                out.push(char::from(HEX[(b >> 4) as usize]));
                out.push(char::from(HEX[(b & 0x0f) as usize]));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn project_path_from_mr_url_handles_nested_groups() {
        assert_eq!(
            project_path_from_mr_url(
                "https://gitlab.example.com/group/sub/project/-/merge_requests/12"
            )
            .as_deref(),
            Some("group/sub/project"),
        );
        assert_eq!(
            project_path_from_mr_url("https://gitlab.com/group/project/merge_requests/3")
                .as_deref(),
            Some("group/project"),
        );
    }

    #[test]
    fn host_from_url_extracts_gitlab_host() {
        assert_eq!(
            host_from_url("https://gitlab.example.com/group/project/-/merge_requests/12")
                .as_deref(),
            Some("gitlab.example.com"),
        );
    }

    #[test]
    fn encode_project_path_url_encodes_nested_slashes() {
        assert_eq!(
            encode_project_path("group/sub/project"),
            "group%2Fsub%2Fproject"
        );
    }

    #[test]
    fn api_call_renders_hostname_and_path() {
        let plan = api_call("gitlab.example.com", "projects/group%2Fproject").plan_argv();
        assert_eq!(
            plan[1..],
            [
                "api".to_string(),
                "--hostname".to_string(),
                "gitlab.example.com".to_string(),
                "projects/group%2Fproject".to_string(),
            ],
        );
    }

    #[test]
    fn api_call_with_method_fields_renders_typed_fields() {
        let plan = api_call_with_method_fields(
            "gitlab.example.com",
            "PUT",
            "projects/group%2Fproject/merge_requests/1/merge",
            &[("squash", "true".to_string())],
        )
        .plan_argv();
        assert!(plan.iter().any(|arg| arg == "--method"));
        assert!(plan.iter().any(|arg| arg == "PUT"));
        assert!(plan.iter().any(|arg| arg == "--field"));
        assert!(plan.iter().any(|arg| arg == "squash=true"));
    }
}
