use std::net::TcpStream;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::Context;
use serde::Serialize;
use tungstenite::WebSocket;
use tungstenite::client::IntoClientRequest;
use tungstenite::http::{HeaderName, HeaderValue};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, connect};

use crate::Result;
use crate::websocket::schema::{WebsocketExpect, WebsocketRequestFile, WebsocketStep};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WebsocketTranscriptEntry {
    pub direction: String,
    pub payload: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebsocketExecutedRequest {
    pub target: String,
    pub transcript: Vec<WebsocketTranscriptEntry>,
    pub last_received: Option<String>,
}

/// Apply a read timeout to the underlying TCP socket so per-step `read()`
/// calls cannot hang forever. Pass `None` to clear the timeout.
fn set_socket_read_timeout(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    timeout: Option<Duration>,
) -> std::io::Result<()> {
    let tcp = match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream,
        MaybeTlsStream::Rustls(stream) => stream.get_mut(),
        // Forward-compat: other variants (e.g. NativeTls if a future feature is
        // enabled) do not expose a TcpStream we can configure here.
        _ => return Ok(()),
    };
    tcp.set_read_timeout(timeout)
}

fn parse_message_text(message: Message) -> String {
    match message {
        Message::Text(t) => t.to_string(),
        Message::Binary(b) => String::from_utf8_lossy(&b).to_string(),
        Message::Ping(b) => format!("<PING:{}>", String::from_utf8_lossy(&b)),
        Message::Pong(b) => format!("<PONG:{}>", String::from_utf8_lossy(&b)),
        Message::Close(frame) => match frame {
            Some(f) => format!("<CLOSE:{}:{}>", f.code, f.reason),
            None => "<CLOSE>".to_string(),
        },
        Message::Frame(_) => "<FRAME>".to_string(),
    }
}

fn apply_expect(expect: Option<&WebsocketExpect>, text: &str, path: &str) -> Result<()> {
    if let Some(expect) = expect {
        crate::websocket::expect::evaluate_text_expect(expect, text, path)?;
    }
    Ok(())
}

pub fn execute_websocket_request(
    request_file: &WebsocketRequestFile,
    target_override: &str,
    bearer_token: Option<&str>,
) -> Result<WebsocketExecutedRequest> {
    let target = if !target_override.trim().is_empty() {
        target_override.trim().to_string()
    } else if let Some(url) = request_file.request.url.as_deref() {
        url.to_string()
    } else {
        anyhow::bail!("websocket target URL is empty (set request.url or pass --url/--env)");
    };

    let mut request = target
        .as_str()
        .into_client_request()
        .context("invalid websocket target URL")?;

    for (key, value) in &request_file.request.headers {
        let header_name = HeaderName::from_bytes(key.as_bytes())
            .with_context(|| format!("invalid websocket header name: {key}"))?;
        let header_value = HeaderValue::from_str(value)
            .with_context(|| format!("invalid websocket header value for {key}"))?;
        request.headers_mut().insert(header_name, header_value);
    }

    if let Some(token) = bearer_token {
        let has_auth = request_file
            .request
            .headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("authorization"));
        if !has_auth {
            request.headers_mut().insert(
                HeaderName::from_static("authorization"),
                HeaderValue::from_str(&format!("Bearer {token}"))
                    .context("invalid bearer token for Authorization header")?,
            );
        }
    }

    let (mut socket, _resp) = match request_file.request.connect_timeout_seconds {
        Some(secs) => {
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let _ = tx.send(connect(request));
            });
            match rx.recv_timeout(Duration::from_secs(secs)) {
                Ok(Ok(pair)) => pair,
                Ok(Err(err)) => {
                    return Err(anyhow::Error::new(err))
                        .with_context(|| format!("failed to connect websocket target '{target}'"));
                }
                Err(_) => {
                    anyhow::bail!(
                        "websocket connect timed out after {secs}s for target '{target}'"
                    );
                }
            }
        }
        None => connect(request)
            .with_context(|| format!("failed to connect websocket target '{target}'"))?,
    };

    let mut transcript: Vec<WebsocketTranscriptEntry> = Vec::new();
    let mut last_received: Option<String> = None;

    for (idx, step) in request_file.request.steps.iter().enumerate() {
        match step {
            WebsocketStep::Send { text } => {
                socket
                    .send(Message::Text(text.clone().into()))
                    .with_context(|| format!("websocket send failed at step {idx}"))?;
                transcript.push(WebsocketTranscriptEntry {
                    direction: "send".to_string(),
                    payload: text.clone(),
                });
            }
            WebsocketStep::Receive {
                timeout_seconds,
                expect,
            } => {
                let timeout = timeout_seconds.map(Duration::from_secs);
                if let Some(dur) = timeout {
                    set_socket_read_timeout(&mut socket, Some(dur)).with_context(|| {
                        format!("failed to set read timeout for websocket step {idx}")
                    })?;
                }
                let read_result = socket.read();
                if timeout.is_some() {
                    let _ = set_socket_read_timeout(&mut socket, None);
                }
                let message = read_result.with_context(|| match timeout_seconds {
                    Some(secs) => {
                        format!("websocket receive timed out after {secs}s at step {idx}")
                    }
                    None => format!("websocket receive failed at step {idx}"),
                })?;
                let text = parse_message_text(message);
                apply_expect(
                    expect.as_ref(),
                    &text,
                    &format!("websocket steps[{idx}].expect"),
                )?;
                transcript.push(WebsocketTranscriptEntry {
                    direction: "receive".to_string(),
                    payload: text.clone(),
                });
                last_received = Some(text);
            }
            WebsocketStep::Close => {
                let _ = socket.close(None);
                transcript.push(WebsocketTranscriptEntry {
                    direction: "close".to_string(),
                    payload: String::new(),
                });
            }
        }
    }

    Ok(WebsocketExecutedRequest {
        target,
        transcript,
        last_received,
    })
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::thread;

    use pretty_assertions::assert_eq;
    use tempfile::TempDir;
    use tungstenite::Message;

    use super::*;
    use crate::websocket::schema::WebsocketRequestFile;

    fn spawn_echo_server() -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind websocket listener");
        let addr = listener.local_addr().expect("listener addr");

        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept websocket stream");
            let mut ws = tungstenite::accept(stream).expect("accept websocket handshake");
            loop {
                match ws.read() {
                    Ok(Message::Text(text)) => {
                        let response = if text.trim() == "ping" {
                            "{\"ok\":true}".to_string()
                        } else {
                            text.to_string()
                        };
                        ws.send(Message::Text(response.into()))
                            .expect("send response");
                    }
                    Ok(Message::Close(_)) => {
                        let _ = ws.close(None);
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });

        (format!("ws://{addr}"), handle)
    }

    fn spawn_silent_server() -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind silent listener");
        let addr = listener.local_addr().expect("listener addr");

        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept stream");
            let mut ws = tungstenite::accept(stream).expect("accept handshake on silent server");
            // Block on read until the client closes the connection — never
            // sending anything ourselves so the client read times out.
            let _ = ws.read();
            let _ = ws.close(None);
        });

        (format!("ws://{addr}"), handle)
    }

    #[test]
    fn websocket_runner_step_receive_timeout_surfaces_error() {
        let tmp = TempDir::new().expect("tmp");
        let request_path = tmp.path().join("silent.ws.json");

        let (url, handle) = spawn_silent_server();

        std::fs::write(
            &request_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "url": url,
                "steps": [
                    {"type": "receive", "timeoutSeconds": 1}
                ]
            }))
            .expect("serialize request"),
        )
        .expect("write request");

        let loaded = WebsocketRequestFile::load(&request_path).expect("load request");
        let started = std::time::Instant::now();
        let err =
            execute_websocket_request(&loaded, "", None).expect_err("expected receive to time out");
        let elapsed = started.elapsed();

        let msg = format!("{err:#}");
        assert!(
            msg.contains("timed out after 1s"),
            "expected timeout message; got: {msg}"
        );
        // Cushion for OS scheduling and TLS handshake — keep well under the
        // default integration-test budget.
        assert!(
            elapsed < Duration::from_secs(5),
            "expected timeout to fire within 5s; got {elapsed:?}"
        );

        let _ = handle.join();
    }

    #[test]
    fn websocket_runner_executes_send_receive_steps() {
        let tmp = TempDir::new().expect("tmp");
        let request_path = tmp.path().join("echo.ws.json");

        let (url, handle) = spawn_echo_server();

        std::fs::write(
            &request_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "url": url,
                "steps": [
                    {"type": "send", "text": "ping"},
                    {"type": "receive", "expect": {"jq": ".ok == true"}},
                    {"type": "close"}
                ]
            }))
            .expect("serialize request"),
        )
        .expect("write request");

        let loaded = WebsocketRequestFile::load(&request_path).expect("load request");
        let executed = execute_websocket_request(&loaded, "", None).expect("execute websocket");

        assert_eq!(executed.transcript.len(), 3);
        assert_eq!(executed.transcript[0].direction, "send");
        assert_eq!(executed.transcript[1].direction, "receive");
        assert_eq!(executed.last_received.as_deref(), Some("{\"ok\":true}"));

        handle.join().expect("join websocket server");
    }
}
