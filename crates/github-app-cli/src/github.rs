//! Minimal GitHub REST client for App auth: list installations and mint
//! installation access tokens. All calls are blocking and authenticated with a
//! short-lived App JWT (see [`crate::jwt`]).

use std::time::Duration;

use serde::Deserialize;

use crate::error::CommandError;

const USER_AGENT: &str = concat!("nils-github-app-cli/", env!("CARGO_PKG_VERSION"));
const API_VERSION: &str = "2022-11-28";

/// Response from `POST /app/installations/{id}/access_tokens`.
#[derive(Debug, Clone, Deserialize)]
pub struct InstallationToken {
    pub token: String,
    pub expires_at: String,
    #[serde(default)]
    pub repository_selection: Option<String>,
    #[serde(default)]
    pub permissions: serde_json::Value,
}

/// One element of `GET /app/installations`.
#[derive(Debug, Clone, Deserialize)]
pub struct Installation {
    pub id: i64,
    #[serde(default)]
    pub account: Option<Account>,
    #[serde(default)]
    pub repository_selection: Option<String>,
    #[serde(default)]
    pub permissions: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Account {
    pub login: String,
}

/// Thin blocking client over the GitHub REST API base URL.
pub struct Client {
    http: reqwest::blocking::Client,
    api_base: String,
}

impl Client {
    /// Build a client targeting `api_base` (e.g. `https://api.github.com`).
    pub fn new(api_base: &str) -> Result<Self, CommandError> {
        let http = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| {
                CommandError::unavailable("http-client", format!("build HTTP client: {e}"))
            })?;
        Ok(Self {
            http,
            api_base: api_base.trim_end_matches('/').to_string(),
        })
    }

    /// `GET /app/installations`, following `Link: rel="next"` pagination so an
    /// App with more than one page of installations is not silently truncated.
    pub fn list_installations(&self, jwt: &str) -> Result<Vec<Installation>, CommandError> {
        let mut next = Some(format!("{}/app/installations?per_page=100", self.api_base));
        let mut all = Vec::new();
        while let Some(url) = next {
            let resp = self.send(self.http.get(&url), jwt)?;
            next = next_link(resp.headers());
            let page = resp.json::<Vec<Installation>>().map_err(|e| {
                CommandError::unavailable("decode", format!("decode installations response: {e}"))
            })?;
            all.extend(page);
        }
        Ok(all)
    }

    /// `POST /app/installations/{installation_id}/access_tokens`.
    pub fn mint_installation_token(
        &self,
        jwt: &str,
        installation_id: &str,
    ) -> Result<InstallationToken, CommandError> {
        let url = format!(
            "{}/app/installations/{}/access_tokens",
            self.api_base, installation_id
        );
        let resp = self.send(self.http.post(&url), jwt)?;
        resp.json::<InstallationToken>()
            .map_err(|e| CommandError::unavailable("decode", format!("decode token response: {e}")))
    }

    fn send(
        &self,
        req: reqwest::blocking::RequestBuilder,
        jwt: &str,
    ) -> Result<reqwest::blocking::Response, CommandError> {
        let resp = req
            .bearer_auth(jwt)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .send()
            .map_err(|e| CommandError::unavailable("network", format!("request failed: {e}")))?;

        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        // Surface GitHub's own error message (a status string, never our token).
        let body = resp.text().unwrap_or_default();
        let detail = github_message(&body)
            .unwrap_or_else(|| format!("GitHub API returned HTTP {}", status.as_u16()));
        Err(CommandError::unavailable(
            "github-api",
            format!("HTTP {}: {detail}", status.as_u16()),
        ))
    }
}

/// Extract the `message` field from a GitHub JSON error body, if present.
fn github_message(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("message")?
        .as_str()
        .map(str::to_string)
}

/// The `rel="next"` URL from a response's `Link` header, if any.
fn next_link(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let link = headers.get(reqwest::header::LINK)?.to_str().ok()?;
    parse_next_link(link)
}

/// Parse a GitHub `Link` header value and return the `rel="next"` URL.
///
/// Example value: `<https://api.github.com/app/installations?page=2>; rel="next",
/// <https://api.github.com/app/installations?page=5>; rel="last"`.
fn parse_next_link(link: &str) -> Option<String> {
    for part in link.split(',') {
        let mut segments = part.split(';');
        let url_segment = match segments.next() {
            Some(s) => s.trim(),
            None => continue,
        };
        if segments.any(|s| s.trim() == "rel=\"next\"") {
            return Some(
                url_segment
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_string(),
            );
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;

    /// Serve a fixed sequence of raw HTTP responses on loopback, recording the
    /// request lines so pagination and endpoint shape can be asserted.
    struct StubApi {
        base: String,
        handle: Option<thread::JoinHandle<Vec<String>>>,
    }

    impl StubApi {
        fn start(responses: Vec<String>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            let base = format!("http://{}", listener.local_addr().unwrap());
            let handle = thread::spawn(move || {
                let mut requests = Vec::new();
                for response in responses {
                    let Ok((mut stream, _)) = listener.accept() else {
                        break;
                    };
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut request_line = String::new();
                    reader.read_line(&mut request_line).ok();
                    // Drain headers so the client is not left blocked on write.
                    loop {
                        let mut header = String::new();
                        match reader.read_line(&mut header) {
                            Ok(0) => break,
                            Ok(_) if header.trim().is_empty() => break,
                            Ok(_) => {}
                            Err(_) => break,
                        }
                    }
                    requests.push(request_line.trim().to_string());
                    stream.write_all(response.as_bytes()).ok();
                    stream.flush().ok();
                }
                requests
            });
            Self {
                base,
                handle: Some(handle),
            }
        }

        fn requests(&mut self) -> Vec<String> {
            self.handle
                .take()
                .expect("server joined once")
                .join()
                .expect("server thread")
        }
    }

    fn http_response(status: &str, body: &str, extra_headers: &[&str]) -> String {
        let mut response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\n");
        for header in extra_headers {
            response.push_str(header);
            response.push_str("\r\n");
        }
        response.push_str(&format!(
            "Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ));
        response
    }

    #[test]
    fn list_installations_follows_link_pagination_to_the_last_page() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().unwrap();
        // The advertised `next` URL points back at this same stub, which is
        // what the client must follow to reach the second page.
        let next_link = format!("Link: <http://{addr}/app/installations?page=2>; rel=\"next\"");
        let base = format!("http://{addr}");
        let handle = thread::spawn(move || {
            let responses = [
                http_response(
                    "200 OK",
                    r#"[{"id":1,"account":{"login":"acme"},"repository_selection":"all"}]"#,
                    &[&next_link],
                ),
                http_response(
                    "200 OK",
                    r#"[{"id":2,"account":{"login":"widgets"},"permissions":{"contents":"read"}}]"#,
                    &[],
                ),
            ];
            let mut seen = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                reader.read_line(&mut request_line).ok();
                loop {
                    let mut header = String::new();
                    match reader.read_line(&mut header) {
                        Ok(0) => break,
                        Ok(_) if header.trim().is_empty() => break,
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
                seen.push(request_line.trim().to_string());
                stream.write_all(response.as_bytes()).ok();
                stream.flush().ok();
            }
            seen
        });

        let client = Client::new(&base).expect("client");
        let installations = client.list_installations("jwt-token").expect("list");
        let requests = handle.join().expect("server");

        assert_eq!(installations.len(), 2, "both pages must be returned");
        assert_eq!(installations[0].id, 1);
        assert_eq!(
            installations[0].account.as_ref().map(|a| a.login.as_str()),
            Some("acme")
        );
        assert_eq!(installations[1].id, 2);
        assert_eq!(requests.len(), 2);
        assert!(
            requests[0].contains("/app/installations?per_page=100"),
            "{:?}",
            requests[0]
        );
        assert!(requests[1].contains("page=2"), "{:?}", requests[1]);
    }

    #[test]
    fn mint_installation_token_posts_to_the_installation_endpoint() {
        let mut stub = StubApi::start(vec![http_response(
            "201 Created",
            r#"{"token":"ghs_example","expires_at":"2026-08-03T01:00:00Z","repository_selection":"selected"}"#,
            &[],
        )]);

        let client = Client::new(&format!("{}/", stub.base)).expect("client");
        let token = client
            .mint_installation_token("jwt-token", "12345")
            .expect("token");
        let requests = stub.requests();

        assert_eq!(token.token, "ghs_example");
        assert_eq!(token.expires_at, "2026-08-03T01:00:00Z");
        assert_eq!(token.repository_selection.as_deref(), Some("selected"));
        assert_eq!(
            requests,
            vec!["POST /app/installations/12345/access_tokens HTTP/1.1".to_string()],
            "a trailing slash in api_base must not double up in the path"
        );
    }

    #[test]
    fn a_github_error_body_is_surfaced_without_the_credential() {
        let mut stub = StubApi::start(vec![http_response(
            "401 Unauthorized",
            r#"{"message":"A JWT could not be decoded"}"#,
            &[],
        )]);

        let client = Client::new(&stub.base).expect("client");
        let err = client
            .mint_installation_token("jwt-token", "1")
            .expect_err("unauthorized");
        stub.requests();

        let rendered = format!("{err:?}");
        assert!(rendered.contains("HTTP 401"), "{rendered}");
        assert!(
            rendered.contains("A JWT could not be decoded"),
            "{rendered}"
        );
        assert!(
            !rendered.contains("jwt-token"),
            "the App JWT must never appear in an error: {rendered}"
        );
    }

    #[test]
    fn a_non_json_error_body_degrades_to_the_status_code() {
        let mut stub = StubApi::start(vec![http_response("502 Bad Gateway", "upstream down", &[])]);

        let client = Client::new(&stub.base).expect("client");
        let err = client.list_installations("jwt").expect_err("bad gateway");
        stub.requests();

        assert!(format!("{err:?}").contains("GitHub API returned HTTP 502"));
    }

    #[test]
    fn an_undecodable_success_body_is_reported_as_unavailable() {
        let mut stub = StubApi::start(vec![http_response("200 OK", "{not json", &[])]);

        let client = Client::new(&stub.base).expect("client");
        let err = client.list_installations("jwt").expect_err("bad payload");
        stub.requests();

        assert!(format!("{err:?}").contains("decode installations response"));
    }

    #[test]
    fn a_refused_connection_is_reported_as_a_network_failure() {
        // Bind then drop so nothing is listening on the port.
        let listener = TcpListener::bind("127.0.0.1:0").expect("probe port");
        let addr = listener.local_addr().unwrap();
        drop(listener);
        // Another process can win that ephemeral port between the drop and the
        // request; if anything is listening the connect-refused branch is
        // unreachable rather than wrong, so skip instead of failing.
        if std::net::TcpStream::connect(addr).is_ok() {
            return;
        }

        let client = Client::new(&format!("http://{addr}")).expect("client");
        let err = client.list_installations("jwt").expect_err("no server");

        assert!(format!("{err:?}").contains("request failed"));
    }

    #[test]
    fn github_message_reads_only_a_string_message_field() {
        assert_eq!(
            github_message(r#"{"message":"Bad credentials"}"#).as_deref(),
            Some("Bad credentials")
        );
        assert!(github_message("not json").is_none());
        assert!(github_message(r#"{"other":"value"}"#).is_none());
        assert!(github_message(r#"{"message":42}"#).is_none());
    }

    #[test]
    fn parse_next_link_picks_the_next_rel() {
        let header = "<https://api.github.com/app/installations?per_page=100&page=2>; rel=\"next\", \
<https://api.github.com/app/installations?per_page=100&page=5>; rel=\"last\"";
        assert_eq!(
            parse_next_link(header).as_deref(),
            Some("https://api.github.com/app/installations?per_page=100&page=2")
        );
    }

    #[test]
    fn parse_next_link_is_none_without_a_next_rel() {
        let header = "<https://api.github.com/app/installations?page=1>; rel=\"prev\", \
<https://api.github.com/app/installations?page=1>; rel=\"first\"";
        assert!(parse_next_link(header).is_none());
    }
}
