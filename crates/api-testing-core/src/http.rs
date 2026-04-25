use anyhow::Context;
use reqwest::Method;
use reqwest::blocking::{Body, Client, multipart::Form};
use reqwest::header::{CONTENT_TYPE, HeaderMap};

use crate::Result;

/// Minimal protocol-agnostic HTTP response, shared by every backend runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub content_type: Option<String>,
}

/// Body shape for [`execute_request`].
///
/// `Multipart` intentionally exposes [`reqwest::blocking::multipart::Form`]
/// because the form is composed by the caller (REST runners assemble fields,
/// files, and base64 payloads outside the generic HTTP layer). Inline reqwest
/// types are an acceptable leak inside this workspace: every consumer already
/// depends on reqwest, and abstracting `Form` would only add boilerplate.
pub enum HttpBody {
    None,
    Bytes(Vec<u8>),
    Multipart(Form),
}

/// Send a blocking HTTP request and read the response into [`HttpResponse`].
///
/// This helper is deliberately protocol-agnostic: the caller composes the URL,
/// headers, and body, then hands them in. REST-aware decisions
/// (Accept defaults, bearer-token shaping, JSON serialisation, multipart
/// assembly) live in `rest::runner::execute_rest_request`; future
/// non-REST backends layer their own conventions on top of this primitive.
pub fn execute_request(
    method: Method,
    url: &str,
    headers: HeaderMap,
    body: HttpBody,
) -> Result<HttpResponse> {
    let client = Client::new();
    let mut builder = client.request(method.clone(), url).headers(headers);

    builder = match body {
        HttpBody::None => builder,
        HttpBody::Bytes(bytes) => builder.body(Body::from(bytes)),
        HttpBody::Multipart(form) => builder.multipart(form),
    };

    let response = builder
        .send()
        .with_context(|| format!("HTTP request failed: {method} {url}"))?;

    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let body = response
        .bytes()
        .context("failed to read response body")?
        .to_vec();

    Ok(HttpResponse {
        status,
        body,
        content_type,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nils_test_support::http::{HttpResponse as StubResponse, LoopbackServer};
    use reqwest::header::{ACCEPT, HeaderValue};

    #[test]
    fn execute_request_returns_status_body_and_content_type_for_get() {
        let server = LoopbackServer::new().expect("server");
        server.add_route(
            "GET",
            "/echo",
            StubResponse::new(200, r#"{"ok":true}"#)
                .with_header("Content-Type", "application/json"),
        );

        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        let url = format!("{}/echo", server.url());
        let response =
            execute_request(Method::GET, &url, headers, HttpBody::None).expect("execute");

        assert_eq!(response.status, 200);
        assert_eq!(response.content_type.as_deref(), Some("application/json"));
        assert_eq!(response.body, br#"{"ok":true}"#.to_vec());
    }

    #[test]
    fn execute_request_sends_body_bytes_for_post() {
        let server = LoopbackServer::new().expect("server");
        server.add_route(
            "POST",
            "/widgets",
            StubResponse::new(201, "").with_header("Content-Type", "text/plain"),
        );

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let url = format!("{}/widgets", server.url());
        let body = br#"{"name":"alpha"}"#.to_vec();
        let response =
            execute_request(Method::POST, &url, headers, HttpBody::Bytes(body)).expect("execute");

        assert_eq!(response.status, 201);
        let received = server.take_requests();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].method, "POST");
        assert!(received[0].body_text().contains("\"name\":\"alpha\""));
    }

    #[test]
    fn execute_request_surfaces_transport_errors_with_method_and_url() {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        // Port 1 is reserved (tcpmux) and effectively guaranteed to refuse a
        // local TCP connection on test infrastructure.
        let url = "http://127.0.0.1:1/unreachable";
        let err = execute_request(Method::GET, url, headers, HttpBody::None).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("HTTP request failed: GET") && msg.contains(url),
            "expected error to mention method + URL; got: {msg}"
        );
    }
}
