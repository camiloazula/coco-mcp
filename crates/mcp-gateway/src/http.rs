//! A streamable HTTP server: messages posted to its endpoint, and what it
//! says read from the responses and from the stream it opens for itself.
//!
//! This is a client that interprets nothing: a response is JSON or a
//! `text/event-stream` and its messages go on to the agent as they are, and
//! an error status is handed back to the agent as the error it is. The
//! agent does the negotiating, so none of an MCP client's own rules are
//! needed here.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt as _;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::frame::{self, Kind};
use crate::upstream::{
    Connector, Frame, INCOMING, MAX_MESSAGE, Opened, RequestContext, StderrSink, Upstream,
    UpstreamError,
};

/// Longest error body kept, in bytes.
const MAX_ERROR_BODY: usize = 1024 * 1024;
/// How long ending the server's session may take.
const CLOSE_AFTER: Duration = Duration::from_secs(5);

/// What the server is called with: the `Authorization` header value, if it
/// takes one. Asked before each request, so a token that expires is
/// refreshed by whoever hands it out.
#[async_trait]
pub trait Authorizer: Send + Sync + 'static {
    /// The header value, or `None` for none.
    async fn authorization(&self) -> Result<Option<String>, String>;
}

/// No credentials.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoAuth;

#[async_trait]
impl Authorizer for NoAuth {
    async fn authorization(&self) -> Result<Option<String>, String> {
        Ok(None)
    }
}

/// A bearer token.
#[derive(Clone)]
pub struct Bearer(pub String);

impl std::fmt::Debug for Bearer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Bearer(..)")
    }
}

#[async_trait]
impl Authorizer for Bearer {
    async fn authorization(&self) -> Result<Option<String>, String> {
        Ok(Some(format!("Bearer {}", self.0)))
    }
}

/// Opens an HTTP session to a server for each agent.
#[derive(Clone)]
pub struct HttpConnector {
    /// The endpoint.
    pub url: String,
    /// Headers every request carries, from the server's settings.
    pub headers: Vec<(String, String)>,
    /// How it is authorized.
    pub authorizer: Arc<dyn Authorizer>,
    /// The client requests go through.
    pub client: reqwest::Client,
}

impl std::fmt::Debug for HttpConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpConnector")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Connector for HttpConnector {
    async fn open(&self, _stderr: StderrSink) -> Result<Opened, UpstreamError> {
        let (tx, incoming) = mpsc::channel(INCOMING);
        let session = Session {
            client: self.client.clone(),
            url: self.url.clone(),
            headers: self.headers.clone(),
            authorizer: self.authorizer.clone(),
            id: std::sync::Mutex::new(None),
            tx,
            listening: AtomicBool::new(false),
            cancel: CancellationToken::new(),
        };
        Ok(Opened {
            upstream: Arc::new(Http(Arc::new(session))),
            incoming,
        })
    }
}

struct Session {
    client: reqwest::Client,
    url: String,
    headers: Vec<(String, String)>,
    authorizer: Arc<dyn Authorizer>,
    /// The `Mcp-Session-Id` the server gave.
    id: std::sync::Mutex<Option<String>>,
    tx: mpsc::Sender<Value>,
    /// Whether the stream for what the server says on its own was opened.
    listening: AtomicBool,
    cancel: CancellationToken,
}

struct Http(Arc<Session>);

#[async_trait]
impl Upstream for Http {
    async fn send(&self, frame: &Frame, context: &RequestContext) -> Result<(), UpstreamError> {
        self.0.post(frame, context).await
    }

    async fn close(&self) {
        self.0.cancel.cancel();
        let id = self.0.id.lock().ok().and_then(|id| id.clone());
        if id.is_none() {
            return;
        }
        let request = self.0.client.delete(&self.0.url);
        if let Ok(request) = self.0.decorate(request, &[]).await {
            let _ = tokio::time::timeout(CLOSE_AFTER, request.send()).await;
        }
    }
}

fn failed(e: impl std::fmt::Display) -> UpstreamError {
    UpstreamError::Failed(e.to_string())
}

/// A failed request, without the URL reqwest names it by: the URL may carry
/// the server's credential, and this goes back to the agent.
fn unreached(e: reqwest::Error) -> UpstreamError {
    UpstreamError::Failed(e.without_url().to_string())
}

impl Session {
    /// A request carrying what every one does: the server's own headers,
    /// the agent's `Mcp-*` ones, the session and the credentials.
    async fn decorate(
        &self,
        mut request: reqwest::RequestBuilder,
        forwarded: &[(String, String)],
    ) -> Result<reqwest::RequestBuilder, UpstreamError> {
        for (name, value) in self.headers.iter().chain(forwarded) {
            request = request.header(name.as_str(), value.as_str());
        }
        if let Some(id) = self.id.lock().ok().and_then(|id| id.clone()) {
            request = request.header("mcp-session-id", id);
        }
        if let Some(value) = self.authorizer.authorization().await.map_err(failed)? {
            request = request.header(reqwest::header::AUTHORIZATION, value);
        }
        Ok(request)
    }

    fn remember(&self, headers: &reqwest::header::HeaderMap) {
        if let Some(id) = headers.get("mcp-session-id").and_then(|v| v.to_str().ok())
            && let Ok(mut slot) = self.id.lock()
        {
            *slot = Some(id.to_owned());
        }
    }

    async fn post(
        self: &Arc<Self>,
        frame: &Frame,
        context: &RequestContext,
    ) -> Result<(), UpstreamError> {
        let request = self
            .client
            .post(&self.url)
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(frame.raw.clone());
        let response = self
            .decorate(request, &context.headers)
            .await?
            .send()
            .await
            .map_err(unreached)?;
        self.remember(response.headers());
        let status = response.status();
        if !status.is_success() {
            return Err(error_of(response).await);
        }
        if status == reqwest::StatusCode::ACCEPTED || status == reqwest::StatusCode::NO_CONTENT {
            return Ok(());
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        // The id of the request this answers, if it was one: an answer that
        // never comes is told to the agent, which would otherwise wait.
        let awaiting = match frame::kind(&frame.value) {
            Kind::Request(id) => Some(id),
            _ => None,
        };
        if content_type.starts_with("text/event-stream") {
            self.clone().stream(response, awaiting);
        } else if content_type.contains("json") {
            let body = read_capped(response, MAX_MESSAGE).await?;
            self.deliver_json(&body, awaiting.as_ref()).await;
        } else if response.content_length() != Some(0) {
            return Err(UpstreamError::Failed(format!(
                "the server answered with `{content_type}`, which is neither JSON nor an event stream"
            )));
        }
        if matches!(frame::kind(&frame.value), Kind::Request(_)) {
            self.listen();
        }
        Ok(())
    }

    /// A JSON body: one message, or a batch.
    async fn deliver_json(&self, body: &[u8], awaiting: Option<&Value>) {
        match serde_json::from_slice::<Value>(body) {
            Ok(Value::Array(messages)) => {
                for message in messages {
                    let _ = self.tx.send(message).await;
                }
            }
            Ok(message) => {
                let _ = self.tx.send(message).await;
            }
            Err(e) => {
                tracing::warn!("an HTTP server answered with JSON that does not parse: {e}");
                if let Some(id) = awaiting {
                    self.unanswered(id, "the server answered with JSON that does not parse")
                        .await;
                }
            }
        }
    }

    /// Answer request `id` with an error, for a server that will not.
    async fn unanswered(&self, id: &Value, why: &str) {
        let error = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32603, "message": why},
        });
        let _ = self.tx.send(error).await;
    }

    /// Read an event stream to its end, delivering each message as it comes.
    ///
    /// `awaiting` is the request the stream answers, if it does: when the
    /// stream ends or breaks before its response, the agent is given an error
    /// for it rather than left waiting.
    fn stream(self: Arc<Self>, response: reqwest::Response, awaiting: Option<Value>) {
        tokio::spawn(async move {
            let mut events = Events::default();
            let mut body = response.bytes_stream();
            let mut answered = false;
            loop {
                let chunk = tokio::select! {
                    () = self.cancel.cancelled() => return,
                    chunk = body.next() => chunk,
                };
                let Some(Ok(chunk)) = chunk else {
                    break;
                };
                let Ok(data) = events.feed(&chunk) else {
                    tracing::warn!("an event of an HTTP server is over {MAX_MESSAGE} bytes");
                    break;
                };
                for data in data {
                    match serde_json::from_str::<Value>(&data) {
                        Ok(message) => {
                            if let (Some(id), Kind::Response(of)) =
                                (&awaiting, frame::kind(&message))
                                && *id == of
                            {
                                answered = true;
                            }
                            if self.tx.send(message).await.is_err() {
                                return;
                            }
                        }
                        Err(e) => tracing::warn!("an event of an HTTP server is not JSON: {e}"),
                    }
                }
            }
            if let (false, Some(id)) = (answered, &awaiting) {
                self.unanswered(id, "the server's event stream ended before it answered")
                    .await;
            }
        });
    }

    /// Open the stream a server uses to say what no request asked for, once
    /// the first request is on its way. A server without one answers `405`,
    /// and the gateway goes without.
    fn listen(self: &Arc<Self>) {
        if self.listening.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            let request = this
                .client
                .get(&this.url)
                .header(reqwest::header::ACCEPT, "text/event-stream");
            let Ok(request) = this.decorate(request, &[]).await else {
                return;
            };
            let Ok(response) = request.send().await else {
                return;
            };
            let streams = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|t| t.to_ascii_lowercase().starts_with("text/event-stream"));
            if response.status().is_success() && streams {
                this.stream(response, None);
            }
        });
    }
}

/// An error response, with the parts worth handing back.
async fn error_of(response: reqwest::Response) -> UpstreamError {
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .filter(|(name, _)| {
            matches!(
                name.as_str(),
                "content-type" | "www-authenticate" | "retry-after"
            ) || name.as_str().starts_with("mcp-")
        })
        .filter_map(|(name, value)| Some((name.to_string(), value.to_str().ok()?.to_owned())))
        .collect();
    let body = read_capped(response, MAX_ERROR_BODY)
        .await
        .unwrap_or_default();
    UpstreamError::Http {
        status,
        headers,
        body,
    }
}

async fn read_capped(response: reqwest::Response, cap: usize) -> Result<Vec<u8>, UpstreamError> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(unreached)?;
        if body.len() + chunk.len() > cap {
            return Err(UpstreamError::Failed(format!(
                "the server's answer is over {cap} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Server-sent events, read a chunk at a time: the `data` of each event.
#[derive(Default)]
struct Events {
    buffer: Vec<u8>,
}

impl Events {
    /// Take in `chunk`; the data of every event it completed. `Err` when an
    /// event grows past [`MAX_MESSAGE`].
    fn feed(&mut self, chunk: &[u8]) -> Result<Vec<String>, ()> {
        self.buffer.extend_from_slice(chunk);
        let mut found = Vec::new();
        while let Some((end, separator)) = event_end(&self.buffer) {
            let event: Vec<u8> = self.buffer.drain(..end + separator).collect();
            let text = String::from_utf8_lossy(&event[..end]);
            let data: Vec<&str> = text
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(|rest| rest.strip_prefix(' ').unwrap_or(rest))
                .collect();
            let data = data.join("\n");
            // A priming event carries no message.
            if !data.trim().is_empty() {
                found.push(data);
            }
        }
        if self.buffer.len() > MAX_MESSAGE {
            return Err(());
        }
        Ok(found)
    }
}

/// Where the first event in `buffer` ends, and how long its separator is.
fn event_end(buffer: &[u8]) -> Option<(usize, usize)> {
    let find = |pattern: &[u8]| buffer.windows(pattern.len()).position(|w| w == pattern);
    match (find(b"\n\n"), find(b"\r\n\r\n")) {
        (Some(a), Some(b)) if b < a => Some((b, 4)),
        (Some(a), _) => Some((a, 2)),
        (None, Some(b)) => Some((b, 4)),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_separator_split_between_chunks_is_still_one() {
        let mut events = Events::default();
        assert!(events.feed(b"data: {\"a\":1}\r\n").unwrap().is_empty());
        assert!(events.feed(b"\r").unwrap().is_empty());
        assert_eq!(events.feed(b"\n").unwrap(), [r#"{"a":1}"#]);
    }

    #[test]
    fn events_are_read_across_chunks_and_separators() {
        let mut events = Events::default();
        assert!(events.feed(b"id: 1\ndata: {\"a\"").unwrap().is_empty());
        let found = events.feed(b":1}\n\ndata: {\"b\":2}\r\n\r\n").unwrap();
        assert_eq!(found, [r#"{"a":1}"#, r#"{"b":2}"#]);
        // A priming event, a comment and a multi-line event.
        let found = events
            .feed(b"id: 9\ndata:\n\n: keep-alive\n\ndata: [1,\ndata: 2]\n\n")
            .unwrap();
        assert_eq!(found, ["[1,\n2]"]);
    }
}
