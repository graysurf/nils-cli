use std::path::{Path, PathBuf};

use anyhow::Context;
use base64::Engine;

use crate::Result;
use crate::http::{HttpBody, HttpResponse, execute_request};
use crate::rest::schema::{RestMultipartPart, RestRequestFile};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestExecutedRequest {
    pub method: String,
    pub url: String,
    pub response: HttpResponse,
}

fn resolve_part_file_path(request_file: &Path, raw: &str) -> Result<PathBuf> {
    let path = Path::new(raw);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }

    let base_dir = request_file.parent().unwrap_or_else(|| Path::new("."));
    Ok(base_dir.join(path))
}

fn build_multipart_form(
    request_file: &Path,
    parts: &[RestMultipartPart],
) -> Result<Option<reqwest::blocking::multipart::Form>> {
    if parts.is_empty() {
        return Ok(None);
    }

    let mut form = reqwest::blocking::multipart::Form::new();
    let mut added_parts = 0usize;

    for part in parts {
        let name = part.name.trim();
        if name.is_empty() {
            anyhow::bail!("Multipart part is missing required field: name");
        }

        if let Some(value) = &part.value {
            let value = value.trim();
            if !value.is_empty() {
                form = form.text(name.to_string(), value.to_string());
                added_parts += 1;
                continue;
            }
        }

        if let Some(payload) = &part.base64 {
            let payload = payload.trim();
            if !payload.is_empty() {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(payload)
                    .context("failed to decode multipart base64 payload")?;
                let mut p = reqwest::blocking::multipart::Part::bytes(bytes);
                let filename = part
                    .filename
                    .clone()
                    .unwrap_or_else(|| "rest.multipart.bin".to_string());
                p = p.file_name(filename);
                if let Some(ct) = &part.content_type {
                    p = p
                        .mime_str(ct)
                        .with_context(|| format!("invalid multipart contentType: {ct}"))?;
                }
                form = form.part(name.to_string(), p);
                added_parts += 1;
                continue;
            }
        }

        let Some(file_path_raw) = part.file_path.as_deref() else {
            anyhow::bail!("Multipart part '{name}' must include value, filePath, or base64.");
        };

        let file_path = resolve_part_file_path(request_file, file_path_raw)?;
        if !file_path.is_file() {
            anyhow::bail!(
                "Multipart part '{name}' file not found: {}",
                file_path.display()
            );
        }

        let mut p = reqwest::blocking::multipart::Part::file(&file_path)
            .with_context(|| format!("failed to open multipart file: {}", file_path.display()))?;

        let filename = part.filename.clone().unwrap_or_else(|| {
            file_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("file")
                .to_string()
        });
        p = p.file_name(filename);

        if let Some(ct) = &part.content_type {
            p = p
                .mime_str(ct)
                .with_context(|| format!("invalid multipart contentType: {ct}"))?;
        }

        form = form.part(name.to_string(), p);
        added_parts += 1;
    }

    if added_parts == 0 {
        Ok(None)
    } else {
        Ok(Some(form))
    }
}

pub fn execute_rest_request(
    request_file: &RestRequestFile,
    base_url: &str,
    bearer_token: Option<&str>,
) -> Result<RestExecutedRequest> {
    let req = &request_file.request;

    let base = base_url.trim_end_matches('/');
    let mut url = format!("{base}{}", req.path);
    let query_string = req.query_string();
    if !query_string.is_empty() {
        url.push('?');
        url.push_str(&query_string);
    }

    let method = reqwest::Method::from_bytes(req.method.as_bytes())
        .with_context(|| format!("invalid HTTP method: {}", req.method))?;

    let mut headers = reqwest::header::HeaderMap::new();
    if !req.headers.accept_key_present {
        headers.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
    }
    if req.body.is_some() {
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
    }
    if let Some(token) = bearer_token {
        let value = format!("Bearer {token}");
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str(&value)
                .context("invalid Authorization header value")?,
        );
    }

    for (k, v) in &req.headers.user_headers {
        let name = reqwest::header::HeaderName::from_bytes(k.as_bytes())
            .with_context(|| format!("invalid header name: {k}"))?;
        let value = reqwest::header::HeaderValue::from_str(v)
            .with_context(|| format!("invalid header value: {k}"))?;
        headers.append(name, value);
    }

    let body = if let Some(body) = &req.body {
        let bytes = serde_json::to_vec(body).context("failed to serialize request body as JSON")?;
        HttpBody::Bytes(bytes)
    } else if let Some(parts) = &req.multipart {
        match build_multipart_form(&request_file.path, parts)? {
            Some(form) => HttpBody::Multipart(form),
            None => HttpBody::None,
        }
    } else {
        HttpBody::None
    };

    let response = execute_request(method, &url, headers, body)?;

    Ok(RestExecutedRequest {
        method: req.method.clone(),
        url,
        response,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    use nils_test_support::http::{HttpResponse, LoopbackServer};
    use tempfile::TempDir;

    #[test]
    fn rest_runner_url_construction_includes_sorted_query() {
        let request_file = RestRequestFile {
            path: PathBuf::from("/tmp/req.request.json"),
            request: crate::rest::schema::parse_rest_request_json(serde_json::json!({
                "method": "GET",
                "path": "/health",
                "query": { "b": 1, "a": true }
            }))
            .unwrap(),
        };

        // Not actually sending a request here; just validate the derived URL logic via build_multipart_form
        // by calling execute_rest_request up to the point that constructs the URL would be awkward. Keep this
        // as a lightweight unit check by asserting the computed URL through the public helper path.
        let base = "http://localhost:6700/";
        let req = &request_file.request;
        let base = base.trim_end_matches('/');
        let mut url = format!("{base}{}", req.path);
        let qs = req.query_string();
        if !qs.is_empty() {
            url.push('?');
            url.push_str(&qs);
        }
        assert_eq!(url, "http://localhost:6700/health?a=true&b=1");
    }

    #[test]
    fn rest_runner_resolve_part_file_path_respects_absolute_and_relative() {
        let request_file = Path::new("/tmp/req/request.json");
        let absolute = resolve_part_file_path(request_file, "/var/data.bin").expect("abs");
        assert_eq!(absolute, PathBuf::from("/var/data.bin"));

        let relative = resolve_part_file_path(request_file, "data.bin").expect("rel");
        assert_eq!(relative, PathBuf::from("/tmp/req/data.bin"));
    }

    #[test]
    fn rest_runner_build_multipart_form_empty_returns_none() {
        let request_file = Path::new("/tmp/req/request.json");
        let form = build_multipart_form(request_file, &[]).expect("form");
        assert!(form.is_none());
    }

    #[test]
    fn rest_runner_build_multipart_form_errors_without_name() {
        let request_file = Path::new("/tmp/req/request.json");
        let parts = vec![RestMultipartPart {
            name: " ".to_string(),
            value: Some("hi".to_string()),
            file_path: None,
            base64: None,
            filename: None,
            content_type: None,
        }];
        let err = build_multipart_form(request_file, &parts).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("Multipart part is missing required field"));
    }

    #[test]
    fn rest_runner_build_multipart_form_accepts_value_base64_and_file() {
        let tmp = TempDir::new().expect("tmp");
        let request_file = tmp.path().join("req.request.json");
        let file_path = tmp.path().join("data.bin");
        std::fs::write(&file_path, b"abc").expect("write");

        let parts = vec![
            RestMultipartPart {
                name: "note".to_string(),
                value: Some("hello".to_string()),
                file_path: None,
                base64: None,
                filename: None,
                content_type: None,
            },
            RestMultipartPart {
                name: "raw".to_string(),
                value: None,
                file_path: None,
                base64: Some("AQID".to_string()),
                filename: Some("payload.bin".to_string()),
                content_type: Some("application/octet-stream".to_string()),
            },
            RestMultipartPart {
                name: "file".to_string(),
                value: None,
                file_path: Some("data.bin".to_string()),
                base64: None,
                filename: None,
                content_type: None,
            },
        ];

        let form = build_multipart_form(&request_file, &parts).expect("form");
        assert!(form.is_some());
    }

    #[test]
    fn rest_runner_execute_request_sends_headers_and_body() {
        let server = LoopbackServer::new().expect("server");
        server.add_route(
            "POST",
            "/widgets",
            HttpResponse::new(200, r#"{"ok":true}"#)
                .with_header("Content-Type", "application/json"),
        );

        let request_file = RestRequestFile {
            path: PathBuf::from("/tmp/req.request.json"),
            request: crate::rest::schema::parse_rest_request_json(serde_json::json!({
                "method": "POST",
                "path": "/widgets",
                "headers": { "X-Trace": "1" },
                "body": { "name": "alpha" }
            }))
            .unwrap(),
        };

        let executed =
            execute_rest_request(&request_file, &server.url(), Some("token")).expect("execute");
        assert_eq!(executed.response.status, 200);
        assert_eq!(
            executed.response.content_type.as_deref(),
            Some("application/json")
        );

        let requests = server.take_requests();
        assert_eq!(requests.len(), 1);
        let req = &requests[0];
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/widgets");
        assert_eq!(
            req.header_value("authorization").as_deref(),
            Some("Bearer token")
        );
        assert_eq!(
            req.header_value("accept").as_deref(),
            Some("application/json")
        );
        assert!(req.body_text().contains("\"name\":\"alpha\""));
    }
}
