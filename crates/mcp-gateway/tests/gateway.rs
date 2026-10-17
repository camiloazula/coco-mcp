//! The gateway against a scripted server (what is routed where, who is let
//! in) and against the mock server over both of its transports (that what
//! the agent gets is what a real server said).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use mcp_gateway::{
    Authorizer, Bearer, ClientInfo, Connector, Direction, Frame, Gateway, HttpConnector, Limits,
    NoAuth, Observer, Opened, RelayServer, RequestContext, StderrSink, Traffic, Upstream,
    UpstreamError, stdio_over_streams,
};
use mcp_mockserver::http::{HttpAuth, serve_http_with};
use mcp_mockserver::{Faults, MockServer, Schema};
use serde_json::{Value, json};
use tokio::sync::mpsc;

const TOKEN: &str = "coco_test_token";

// ------------------------------------------------------------ the observer

#[derive(Default)]
struct Recorder(Mutex<Vec<String>>, Mutex<Vec<(String, String)>>);

impl Recorder {
    fn log(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }

    /// What was said about one server, in order.
    fn of(&self, namespace: &str) -> Vec<String> {
        self.1
            .lock()
            .unwrap()
            .iter()
            .filter(|(n, _)| n == namespace)
            .map(|(_, line)| line.clone())
            .collect()
    }

    fn push(&self, what: String) {
        self.0.lock().unwrap().push(what);
    }

    fn push_for(&self, namespace: &str, what: String) {
        self.1
            .lock()
            .unwrap()
            .push((namespace.to_owned(), what.clone()));
        self.push(what);
    }
}

impl Observer for Recorder {
    fn opened(&self, namespace: &str, _: &str) {
        self.push_for(namespace, format!("opened {namespace}"));
    }

    fn client(&self, namespace: &str, _: &str, info: ClientInfo) {
        self.push_for(
            namespace,
            format!("client {} {}", info.name, info.version.unwrap_or_default()),
        );
    }

    fn frame(&self, namespace: &str, _: &str, traffic: Traffic<'_>) {
        let (direction, frame) = (traffic.direction, traffic.frame);
        let arrow = if direction == Direction::Outbound {
            "->"
        } else {
            "<-"
        };
        let what = frame["method"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("result {}", frame["id"]));
        self.push_for(namespace, format!("{arrow} {what}"));
    }

    fn stderr(&self, namespace: &str, _: &str, line: &str) {
        self.push_for(namespace, format!("stderr {line}"));
    }

    fn closed(&self, namespace: &str, _: &str, reason: &str) {
        self.push_for(namespace, format!("closed {reason}"));
    }
}

// ----------------------------------------------------------- a fake server

/// What the fake server does with a message it is sent.
type Script = Arc<dyn Fn(&Value, &mpsc::Sender<Value>) + Send + Sync>;

struct Fake {
    script: Script,
    fail: Option<fn() -> UpstreamError>,
    closes: Arc<Mutex<u32>>,
}

struct FakeUpstream {
    tx: mpsc::Sender<Value>,
    script: Script,
    fail: Option<fn() -> UpstreamError>,
    closes: Arc<Mutex<u32>>,
}

#[async_trait]
impl Connector for Fake {
    async fn open(&self, _: StderrSink) -> Result<Opened, UpstreamError> {
        let (tx, incoming) = mpsc::channel(64);
        Ok(Opened {
            upstream: Arc::new(FakeUpstream {
                tx,
                script: self.script.clone(),
                fail: self.fail,
                closes: self.closes.clone(),
            }),
            incoming,
        })
    }
}

#[async_trait]
impl Upstream for FakeUpstream {
    async fn send(&self, frame: &Frame, _: &RequestContext) -> Result<(), UpstreamError> {
        if let Some(fail) = self.fail {
            return Err(fail());
        }
        (self.script)(&frame.value, &self.tx);
        Ok(())
    }

    async fn close(&self) {
        *self.closes.lock().unwrap() += 1;
    }
}

fn reply(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// A server that answers every request with `{"ok": <method>}`, and says
/// something on its own around `tools/call` and `notify`.
fn script() -> Script {
    Arc::new(|message, tx| {
        let Some(method) = message["method"].as_str() else {
            return;
        };
        let Some(id) = message.get("id").cloned() else {
            return;
        };
        let tx = tx.clone();
        let method = method.to_owned();
        tokio::spawn(async move {
            match method.as_str() {
                "tools/call" => {
                    let note = json!({"jsonrpc": "2.0", "method": "notifications/progress",
                        "params": {"progress": 1}});
                    let _ = tx.send(note).await;
                    let _ = tx.send(reply(&id, json!({"ok": method}))).await;
                }
                "notify" => {
                    let _ = tx.send(reply(&id, json!({"ok": method}))).await;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    let note = json!({"jsonrpc": "2.0",
                        "method": "notifications/tools/list_changed"});
                    let _ = tx.send(note).await;
                }
                "hang_up" => {}
                _ => {
                    let _ = tx.send(reply(&id, json!({"ok": method}))).await;
                }
            }
        });
    })
}

fn fake() -> Arc<Fake> {
    Arc::new(Fake {
        script: script(),
        fail: None,
        closes: Arc::default(),
    })
}

// ------------------------------------------------------------- the harness

struct Harness {
    gateway: Gateway,
    base: String,
    http: reqwest::Client,
    observer: Arc<Recorder>,
}

async fn start(servers: Vec<(&str, Arc<dyn Connector>)>) -> Harness {
    start_with(servers, Limits::default()).await
}

async fn start_with(servers: Vec<(&str, Arc<dyn Connector>)>, limits: Limits) -> Harness {
    let observer = Arc::new(Recorder::default());
    let servers = servers
        .into_iter()
        .map(|(namespace, connector)| RelayServer {
            namespace: namespace.into(),
            connector,
        })
        .collect();
    let gateway = Gateway::start_with(0, TOKEN.into(), servers, observer.clone(), limits)
        .await
        .unwrap();
    let base = format!("http://127.0.0.1:{}", gateway.addr().port());
    Harness {
        gateway,
        base,
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
        observer,
    }
}

impl Harness {
    fn request(&self, method: reqwest::Method, namespace: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}/{namespace}", self.base))
            .bearer_auth(TOKEN)
    }

    async fn post(
        &self,
        namespace: &str,
        session: Option<&str>,
        body: &Value,
    ) -> reqwest::Response {
        let mut request = self
            .request(reqwest::Method::POST, namespace)
            .header("accept", "application/json, text/event-stream")
            .json(body);
        if let Some(session) = session {
            request = request.header("mcp-session-id", session);
        }
        request.send().await.unwrap()
    }
}

/// The messages of an event stream that has ended.
async fn messages(response: reqwest::Response) -> Vec<Value> {
    assert_eq!(response.status(), 200);
    let text = response.text().await.unwrap();
    text.split("\n\n")
        .flat_map(|event| {
            event
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(|data| data.trim().to_owned())
                .filter(|data| !data.is_empty())
        })
        .map(|data| serde_json::from_str(&data).unwrap())
        .collect()
}

fn session_of(response: &reqwest::Response) -> String {
    response
        .headers()
        .get("mcp-session-id")
        .expect("a session id")
        .to_str()
        .unwrap()
        .to_owned()
}

fn initialize() -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-11-25", "capabilities": {},
        "clientInfo": {"name": "test-agent", "version": "9.1"}
    }})
}

async fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..200 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

// ----------------------------------------------------------------- who may

#[tokio::test]
async fn only_this_machine_with_the_token_gets_in() {
    let h = start(vec![("fake", fake())]).await;
    let body = initialize();
    let post = |request: reqwest::RequestBuilder| {
        let request = request.json(&body);
        async move { request.send().await.unwrap() }
    };

    let none = post(h.http.post(format!("{}/fake", h.base))).await;
    assert_eq!(none.status(), 401);
    assert!(none.headers().contains_key("www-authenticate"));
    let wrong = post(h.http.post(format!("{}/fake", h.base)).bearer_auth("nope")).await;
    assert_eq!(wrong.status(), 401);
    let short = post(
        h.http
            .post(format!("{}/fake", h.base))
            .bearer_auth("coco_test"),
    )
    .await;
    assert_eq!(
        short.status(),
        401,
        "a prefix of the token is not the token"
    );

    let host = post(
        h.request(reqwest::Method::POST, "fake")
            .header("host", "evil.test"),
    )
    .await;
    assert_eq!(host.status(), 403, "a name that rebinds to this machine");
    let origin = post(
        h.request(reqwest::Method::POST, "fake")
            .header("origin", "http://evil.test"),
    )
    .await;
    assert_eq!(origin.status(), 403, "a web page");
    let port = h.gateway.addr().port();
    let local = post(
        h.request(reqwest::Method::POST, "fake")
            .header("origin", format!("http://localhost:{port}")),
    )
    .await;
    assert_eq!(local.status(), 200);

    // Nothing was opened for the turned-away.
    assert_eq!(
        h.observer
            .log()
            .iter()
            .filter(|l| l.starts_with("opened"))
            .count(),
        1
    );
    h.gateway.stop().await;
}

#[tokio::test]
async fn paths_sessions_and_messages_that_are_wrong_are_refused() {
    let h = start(vec![("fake", fake())]).await;
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});
    assert_eq!(h.post("nobody", None, &initialize()).await.status(), 404);
    assert_eq!(
        h.post("fake", Some("not-a-session"), &ping).await.status(),
        404
    );
    let batch = json!([ping.clone(), ping.clone()]);
    assert_eq!(h.post("fake", None, &batch).await.status(), 400);
    let garbage = h
        .request(reqwest::Method::POST, "fake")
        .body("{not json")
        .send()
        .await
        .unwrap();
    assert_eq!(garbage.status(), 400);
    let put = h
        .request(reqwest::Method::PUT, "fake")
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), 405);

    // A session belongs to the server it was opened for.
    let other = start(vec![("a", fake()), ("b", fake())]).await;
    let first = other.post("a", None, &initialize()).await;
    let session = session_of(&first);
    assert_eq!(other.post("b", Some(&session), &ping).await.status(), 404);
    assert_eq!(other.post("a", Some(&session), &ping).await.status(), 200);
    other.gateway.stop().await;
    h.gateway.stop().await;
}

// ------------------------------------------------------------- the routing

#[tokio::test]
async fn a_request_is_answered_on_a_stream_in_the_order_the_server_said_it() {
    let h = start(vec![("fake", fake())]).await;
    let response = h.post("fake", None, &initialize()).await;
    let session = session_of(&response);
    let answer = messages(response).await;
    assert_eq!(answer, [reply(&json!(1), json!({"ok": "initialize"}))]);

    let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    let accepted = h.post("fake", Some(&session), &note).await;
    assert_eq!(accepted.status(), 202);

    let call = json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {}});
    let got = messages(h.post("fake", Some(&session), &call).await).await;
    assert_eq!(got.len(), 2);
    assert_eq!(got[0]["method"], "notifications/progress", "said first");
    assert_eq!(got[1], reply(&json!(7), json!({"ok": "tools/call"})));

    assert_eq!(
        h.observer.log(),
        [
            "opened fake",
            "client test-agent 9.1",
            "-> initialize",
            "<- result 1",
            "-> notifications/initialized",
            "-> tools/call",
            "<- notifications/progress",
            "<- result 7",
        ]
    );
    h.gateway.stop().await;
}

#[tokio::test]
async fn what_the_server_says_alone_goes_to_the_agents_stream() {
    let h = start(vec![("fake", fake())]).await;
    let response = h.post("fake", None, &initialize()).await;
    let session = session_of(&response);
    messages(response).await;

    let listening = h
        .request(reqwest::Method::GET, "fake")
        .header("mcp-session-id", &session)
        .send()
        .await
        .unwrap();
    assert_eq!(listening.status(), 200);
    let again = h
        .request(reqwest::Method::GET, "fake")
        .header("mcp-session-id", &session)
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 409, "one stream at a time");

    let notify = json!({"jsonrpc": "2.0", "id": 2, "method": "notify"});
    let got = messages(h.post("fake", Some(&session), &notify).await).await;
    assert_eq!(got, [reply(&json!(2), json!({"ok": "notify"}))]);

    // The notification came after the answer, with no request waiting.
    let mut stream = listening.bytes_stream();
    use futures::StreamExt as _;
    let chunk = tokio::time::timeout(Duration::from_secs(5), async {
        let mut seen = String::new();
        while let Some(chunk) = stream.next().await {
            seen.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
            if seen.contains("list_changed") {
                break;
            }
        }
        seen
    })
    .await
    .expect("the stream carried it");
    assert!(
        chunk.contains("notifications/tools/list_changed"),
        "{chunk}"
    );
    h.gateway.stop().await;
}

#[tokio::test]
async fn what_nobody_is_waiting_for_is_kept_until_a_stream_opens() {
    let h = start(vec![("fake", fake())]).await;
    let response = h.post("fake", None, &initialize()).await;
    let session = session_of(&response);
    messages(response).await;
    let notify = json!({"jsonrpc": "2.0", "id": 2, "method": "notify"});
    messages(h.post("fake", Some(&session), &notify).await).await;
    wait_for("the notification", || {
        h.observer
            .log()
            .iter()
            .any(|l| l == "<- notifications/tools/list_changed")
    })
    .await;

    let listening = h
        .request(reqwest::Method::GET, "fake")
        .header("mcp-session-id", &session)
        .send()
        .await
        .unwrap();
    use futures::StreamExt as _;
    let mut stream = listening.bytes_stream();
    let first = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("delivered")
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&first).contains("list_changed"));
    h.gateway.stop().await;
}

// ------------------------------------------------------------ the lifetime

#[tokio::test]
async fn an_agent_ends_its_session_and_the_server_behind_it() {
    let connector = fake();
    let closes = connector.closes.clone();
    let h = start(vec![("fake", connector)]).await;
    let response = h.post("fake", None, &initialize()).await;
    let session = session_of(&response);
    messages(response).await;
    assert_eq!(h.gateway.connections(), 1);

    let ended = h
        .request(reqwest::Method::DELETE, "fake")
        .header("mcp-session-id", &session)
        .send()
        .await
        .unwrap();
    assert_eq!(ended.status(), 204);
    assert_eq!(*closes.lock().unwrap(), 1, "the server was closed");
    assert_eq!(h.gateway.connections(), 0);
    let ping = json!({"jsonrpc": "2.0", "id": 3, "method": "ping"});
    assert_eq!(h.post("fake", Some(&session), &ping).await.status(), 404);
    assert!(
        h.observer
            .log()
            .contains(&"closed the agent ended the session".to_owned())
    );
    h.gateway.stop().await;
}

#[tokio::test]
async fn a_server_that_goes_away_ends_the_connection() {
    // A server whose output ends, as a process that exited does.
    struct Exits;
    #[async_trait]
    impl Connector for Exits {
        async fn open(&self, _: StderrSink) -> Result<Opened, UpstreamError> {
            let (reader, _kept) = tokio::io::duplex(64);
            let (_, writer) = tokio::io::duplex(64);
            Ok(stdio_over_streams(reader, writer, None))
        }
    }
    let h = start(vec![("gone", Arc::new(Exits))]).await;
    let _ = h.post("gone", None, &initialize()).await;
    wait_for("the connection to end", || h.gateway.connections() == 0).await;
    h.gateway.stop().await;
}

#[tokio::test]
async fn changing_the_servers_ends_the_connections_to_one_that_changed() {
    let a = fake();
    let h = start(vec![("a", a.clone()), ("b", fake())]).await;
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});
    let on_a = session_of(&h.post("a", None, &initialize()).await);
    let on_b = session_of(&h.post("b", None, &initialize()).await);
    assert_eq!(h.gateway.connections(), 2);

    // `a` stays as it is, `b` is replaced, `c` is new.
    let servers = vec![
        RelayServer {
            namespace: "a".into(),
            connector: a,
        },
        RelayServer {
            namespace: "b".into(),
            connector: fake(),
        },
        RelayServer {
            namespace: "c".into(),
            connector: fake(),
        },
    ];
    h.gateway.set_servers(servers).await;
    assert_eq!(h.post("a", Some(&on_a), &ping).await.status(), 200);
    assert_eq!(h.post("b", Some(&on_b), &ping).await.status(), 404);
    assert_eq!(h.post("c", None, &initialize()).await.status(), 200);

    h.gateway.set_servers(vec![]).await;
    assert_eq!(h.post("a", Some(&on_a), &ping).await.status(), 404);
    assert_eq!(
        h.post("a", None, &initialize()).await.status(),
        404,
        "no such path now"
    );
    h.gateway.stop().await;
}

#[tokio::test]
async fn stopping_ends_every_connection() {
    let connector = fake();
    let closes = connector.closes.clone();
    let h = start(vec![("fake", connector)]).await;
    for _ in 0..3 {
        h.post("fake", None, &initialize()).await;
    }
    assert_eq!(h.gateway.connections(), 3);
    let observer = h.observer.clone();
    h.gateway.stop().await;
    assert_eq!(*closes.lock().unwrap(), 3);
    assert_eq!(
        observer
            .log()
            .iter()
            .filter(|l| l.starts_with("closed"))
            .count(),
        3
    );
}

#[tokio::test]
async fn one_server_takes_only_so_many_agents() {
    let h = start(vec![("fake", fake())]).await;
    for _ in 0..16 {
        assert_eq!(h.post("fake", None, &initialize()).await.status(), 200);
    }
    assert_eq!(h.post("fake", None, &initialize()).await.status(), 429);
    h.gateway.stop().await;
}

// --------------------------------------------------- a server that refuses

#[tokio::test]
async fn an_http_error_from_the_server_reaches_the_agent_as_it_was() {
    let refusing = Arc::new(Fake {
        script: script(),
        fail: Some(|| UpstreamError::Http {
            status: 406,
            headers: vec![("content-type".into(), "application/json".into())],
            body: br#"{"error":"not acceptable"}"#.to_vec(),
        }),
        closes: Arc::default(),
    });
    let h = start(vec![("fake", refusing)]).await;
    let response = h.post("fake", None, &initialize()).await;
    assert_eq!(response.status(), 406);
    assert_eq!(response.headers()["content-type"], "application/json");
    assert_eq!(
        response.text().await.unwrap(),
        r#"{"error":"not acceptable"}"#
    );
    // The connection that could not start is not left behind.
    wait_for("it to end", || h.gateway.connections() == 0).await;
    h.gateway.stop().await;
}

#[tokio::test]
async fn a_servers_refusal_of_the_gateway_is_not_taken_for_the_agents_own() {
    let refusing = Arc::new(Fake {
        script: script(),
        fail: Some(|| UpstreamError::Http {
            status: 401,
            headers: vec![(
                "www-authenticate".into(),
                "Bearer realm=\"upstream\"".into(),
            )],
            body: br#"{"error":"bad token"}"#.to_vec(),
        }),
        closes: Arc::default(),
    });
    let h = start(vec![("fake", refusing)]).await;
    let response = h.post("fake", None, &initialize()).await;
    assert_eq!(response.status(), 502, "not the 401 the agent would act on");
    assert!(!response.headers().contains_key("www-authenticate"));
    let text = response.text().await.unwrap();
    assert!(
        text.contains("HTTP 401") && text.contains("bad token"),
        "{text}"
    );
    h.gateway.stop().await;
}

// -------------------------------------------------- a real server, stdio

/// The mock server, spoken to over a pair of streams as a process is.
struct MockOverStreams(Schema);

#[async_trait]
impl Connector for MockOverStreams {
    async fn open(&self, _: StderrSink) -> Result<Opened, UpstreamError> {
        let (reader, writer) = tokio::io::split(MockServer::serve_duplex(self.0));
        Ok(stdio_over_streams(reader, writer, None))
    }
}

#[tokio::test]
async fn a_stdio_server_is_relayed_unchanged() {
    let h = start(vec![("mock", Arc::new(MockOverStreams(Schema::V1)))]).await;
    let response = h.post("mock", None, &initialize()).await;
    let session = session_of(&response);
    let init = messages(response).await;
    assert_eq!(init.len(), 1);
    assert_eq!(init[0]["id"], 1);
    assert_eq!(init[0]["result"]["serverInfo"]["name"], "mcp-mockserver");

    let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    assert_eq!(h.post("mock", Some(&session), &note).await.status(), 202);

    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    let tools = messages(h.post("mock", Some(&session), &list).await).await;
    let names: Vec<&str> = tools[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"echo") && names.contains(&"add"),
        "{names:?}"
    );

    let call = json!({"jsonrpc": "2.0", "id": "x", "method": "tools/call",
        "params": {"name": "echo", "arguments": {"text": "through the gateway"}}});
    let answer = messages(h.post("mock", Some(&session), &call).await).await;
    assert_eq!(answer[0]["id"], "x", "a string id stays a string");
    assert_eq!(
        answer[0]["result"]["content"][0]["text"],
        "through the gateway"
    );

    // A server error is a response like any other.
    let missing = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "no_such_tool", "arguments": {}}});
    let answer = messages(h.post("mock", Some(&session), &missing).await).await;
    assert!(answer[0].get("error").is_some() || answer[0]["result"]["isError"] == true);
    h.gateway.stop().await;
}

// ---------------------------------------------------- a real server, HTTP

async fn http_mock(auth: HttpAuth) -> mcp_mockserver::http::HttpServer {
    serve_http_with(Schema::V1, Faults::default(), auth, "127.0.0.1:0")
        .await
        .unwrap()
}

fn connector(url: &str, authorizer: Arc<dyn Authorizer>) -> Arc<HttpConnector> {
    Arc::new(HttpConnector {
        url: url.to_owned(),
        headers: vec![],
        authorizer,
        client: reqwest::Client::builder().no_proxy().build().unwrap(),
    })
}

#[tokio::test]
async fn an_http_server_is_relayed_unchanged() {
    let server = http_mock(HttpAuth::None).await;
    let h = start(vec![("web", connector(&server.url, Arc::new(NoAuth)))]).await;
    let response = h.post("web", None, &initialize()).await;
    let session = session_of(&response);
    let init = messages(response).await;
    assert_eq!(init[0]["result"]["serverInfo"]["name"], "mcp-mockserver");

    let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    assert_eq!(h.post("web", Some(&session), &note).await.status(), 202);
    let call = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "add", "arguments": {"a": 2, "b": 3}}});
    let answer = messages(h.post("web", Some(&session), &call).await).await;
    assert_eq!(answer[0]["id"], 2);
    assert_eq!(answer[0]["result"]["structuredContent"]["value"], 5.0);

    let ended = h
        .request(reqwest::Method::DELETE, "web")
        .header("mcp-session-id", &session)
        .send()
        .await
        .unwrap();
    assert_eq!(ended.status(), 204);
    h.gateway.stop().await;
    server.shutdown();
}

#[tokio::test]
async fn the_servers_credentials_are_added_and_its_refusal_passed_on() {
    let server = http_mock(HttpAuth::Bearer("upstream-secret".into())).await;
    let good = connector(&server.url, Arc::new(Bearer("upstream-secret".into())));
    let bad = connector(&server.url, Arc::new(Bearer("wrong".into())));
    let h = start(vec![("good", good), ("bad", bad)]).await;

    // The agent never sends the server's token: the gateway does.
    let response = h.post("good", None, &initialize()).await;
    assert_eq!(response.status(), 200);
    let init = messages(response).await;
    assert_eq!(init[0]["result"]["serverInfo"]["name"], "mcp-mockserver");

    let refused = h.post("bad", None, &initialize()).await;
    assert_eq!(
        refused.status(),
        502,
        "the gateway's credentials, not the agent's"
    );
    assert!(refused.text().await.unwrap().contains("HTTP 401"));
    h.gateway.stop().await;
    server.shutdown();
}

// ------------------------------------------------- a real process (unix)

#[cfg(unix)]
#[tokio::test]
async fn a_process_is_started_per_agent_and_stopped_with_its_session() {
    use mcp_gateway::StdioConnector;
    // Says something on stderr, then answers every line with a result.
    let script = r#"echo started >&2; while read line; do echo '{"jsonrpc":"2.0","id":1,"result":{"ok":true}}'; done"#;
    let connector = Arc::new(StdioConnector {
        command: "/bin/sh".into(),
        args: vec!["-c".into(), script.into()],
        env: Default::default(),
        cwd: None,
    });
    let h = start(vec![("proc", connector)]).await;
    let response = h.post("proc", None, &initialize()).await;
    let session = session_of(&response);
    assert_eq!(
        messages(response).await,
        [reply(&json!(1), json!({"ok": true}))]
    );
    wait_for("its stderr", || {
        h.observer.log().contains(&"stderr started".to_owned())
    })
    .await;
    let ended = h
        .request(reqwest::Method::DELETE, "proc")
        .header("mcp-session-id", &session)
        .send()
        .await
        .unwrap();
    assert_eq!(ended.status(), 204);
    assert_eq!(h.gateway.connections(), 0);

    // A program that does not exist is a gateway error, not a hang.
    let missing = start(vec![(
        "nope",
        Arc::new(StdioConnector {
            command: "/no/such/program".into(),
            args: vec![],
            env: Default::default(),
            cwd: None,
        }),
    )])
    .await;
    let failed = missing.post("nope", None, &initialize()).await;
    assert_eq!(failed.status(), 502);
    assert!(failed.text().await.unwrap().contains("failed to start"));
    missing.gateway.stop().await;
    h.gateway.stop().await;
}

// ----------------------------------------------------- what the review found

#[tokio::test]
async fn a_message_that_does_not_start_a_connection_makes_none() {
    let h = start(vec![("fake", fake())]).await;
    let call = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {}});
    assert_eq!(h.post("fake", None, &call).await.status(), 400);
    let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    assert_eq!(h.post("fake", None, &note).await.status(), 400);
    assert_eq!(h.gateway.connections(), 0);
    assert!(h.observer.log().is_empty(), "nothing was opened");
    h.gateway.stop().await;
}

#[tokio::test]
async fn a_request_the_agent_gave_up_on_does_not_keep_its_connection() {
    let limits = Limits {
        idle: Duration::from_millis(150),
        sweep: Duration::from_millis(50),
        ..Limits::default()
    };
    let h = start_with(vec![("fake", fake())], limits).await;
    let response = h.post("fake", None, &initialize()).await;
    let session = session_of(&response);
    messages(response).await;
    // A request the server never answers, whose stream the agent drops.
    let hang = json!({"jsonrpc": "2.0", "id": 9, "method": "hang_up"});
    let abandoned = h.post("fake", Some(&session), &hang).await;
    assert_eq!(h.gateway.connections(), 1);
    drop(abandoned);
    wait_for("the sweep to end it", || h.gateway.connections() == 0).await;
    assert!(h.observer.log().iter().any(|l| l.contains("idle")));
    h.gateway.stop().await;
}

#[tokio::test]
async fn stopping_frees_the_port_for_the_next_start_at_once() {
    let server = http_mock(HttpAuth::None).await;
    let h = start(vec![("web", connector(&server.url, Arc::new(NoAuth)))]).await;
    let response = h.post("web", None, &initialize()).await;
    messages(response).await;
    assert_eq!(h.gateway.connections(), 1);
    let port = h.gateway.addr().port();
    h.gateway.stop().await;
    let again = Gateway::start(port, TOKEN.into(), vec![], Arc::new(mcp_gateway::Silent))
        .await
        .expect("the port is free after stop");
    again.stop().await;
    server.shutdown();
}

#[tokio::test]
async fn a_server_url_that_carries_a_credential_is_not_given_to_the_agent() {
    // Nothing listens on port 1.
    let leaky = connector(
        "http://127.0.0.1:1/mcp?api_key=SUPERSECRETKEY",
        Arc::new(NoAuth),
    );
    let h = start(vec![("leaky", leaky)]).await;
    let response = h.post("leaky", None, &initialize()).await;
    assert_eq!(response.status(), 502);
    let text = response.text().await.unwrap();
    assert!(!text.contains("SUPERSECRETKEY"), "{text}");
    assert!(!text.contains("api_key"), "{text}");
    h.gateway.stop().await;
}

#[tokio::test]
async fn a_host_or_origin_that_is_not_exactly_this_machine_is_turned_away() {
    let h = start(vec![("fake", fake())]).await;
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});
    for (name, value) in [
        ("host", "localhost".to_owned()),
        ("host", format!("127.0.0.1:{}", h.gateway.addr().port() + 1)),
        ("origin", "null".to_owned()),
    ] {
        let response = h
            .request(reqwest::Method::POST, "fake")
            .header(name, &value)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 403, "{name}: {value}");
    }
    // No Host at all (HTTP/1.0), which a client has no reason to send.
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut raw = tokio::net::TcpStream::connect(h.gateway.addr())
        .await
        .unwrap();
    raw.write_all(
        format!(
            "POST /fake HTTP/1.0\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: 2\r\n\r\n{{}}"
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    let mut answer = String::new();
    raw.read_to_string(&mut answer).await.unwrap();
    assert!(
        answer.starts_with("HTTP/1.0 403") || answer.starts_with("HTTP/1.1 403"),
        "{answer}"
    );
    h.gateway.stop().await;
}

#[tokio::test]
async fn agents_that_arrive_together_cannot_pass_the_cap() {
    // A server slow to open, so every request is in the middle of it at once.
    struct Slow(Arc<Fake>);
    #[async_trait]
    impl Connector for Slow {
        async fn open(&self, stderr: StderrSink) -> Result<Opened, UpstreamError> {
            tokio::time::sleep(Duration::from_millis(150)).await;
            self.0.open(stderr).await
        }
    }
    let limits = Limits {
        connections: 4,
        ..Limits::default()
    };
    let h = Arc::new(start_with(vec![("slow", Arc::new(Slow(fake())))], limits).await);
    let mut tasks = Vec::new();
    for _ in 0..12 {
        let h = h.clone();
        tasks.push(tokio::spawn(async move {
            h.post("slow", None, &initialize()).await.status().as_u16()
        }));
    }
    let mut statuses = Vec::new();
    for task in tasks {
        statuses.push(task.await.unwrap());
    }
    assert_eq!(
        statuses.iter().filter(|s| **s == 200).count(),
        4,
        "{statuses:?}"
    );
    assert_eq!(
        statuses.iter().filter(|s| **s == 429).count(),
        8,
        "{statuses:?}"
    );
    assert_eq!(h.gateway.connections(), 4);
    h.gateway.stop().await;
}

// ------------------------------------------------------------------ the root

/// A server with something to say about each part of the protocol, named
/// `namespace`: tools, resources, a template, a prompt (when asked for), a
/// page break in its tool list (when asked for), a `sampling` request in the
/// middle of the `ask` tool, and tools that hang, die or cancel their own
/// request.
struct Rich {
    namespace: &'static str,
    prompts: bool,
    pages: bool,
    version: &'static str,
    fail_lists: bool,
}

struct RichUp {
    namespace: &'static str,
    prompts: bool,
    pages: bool,
    version: &'static str,
    fail_lists: bool,
    /// What the server says goes out through this; dropped when it dies.
    tx: Mutex<Option<mpsc::Sender<Value>>>,
    asked: Arc<Mutex<Option<Value>>>,
}

#[async_trait]
impl Connector for Rich {
    async fn open(&self, _: StderrSink) -> Result<Opened, UpstreamError> {
        let (tx, incoming) = mpsc::channel(64);
        Ok(Opened {
            upstream: Arc::new(RichUp {
                namespace: self.namespace,
                prompts: self.prompts,
                pages: self.pages,
                version: self.version,
                fail_lists: self.fail_lists,
                tx: Mutex::new(Some(tx)),
                asked: Arc::default(),
            }),
            incoming,
        })
    }
}

#[async_trait]
impl Upstream for RichUp {
    async fn send(&self, frame: &Frame, _: &RequestContext) -> Result<(), UpstreamError> {
        let message = &frame.value;
        let ns = self.namespace;
        let Some(tx) = self.tx.lock().unwrap().clone() else {
            return Err(UpstreamError::Closed);
        };
        let id = message.get("id").cloned();
        let params = message["params"].clone();
        let Some(method) = message["method"].as_str() else {
            // The agent answered the sampling request.
            if message["id"] == json!(0)
                && let Some(call) = self.asked.lock().unwrap().take()
            {
                let text = format!("answer:{}", message["result"]["text"]);
                tokio::spawn(async move {
                    let result = json!({"content": [{"type": "text", "text": text}]});
                    let _ = tx.send(reply(&call, result)).await;
                });
            }
            return Ok(());
        };
        let Some(id) = id else {
            return Ok(());
        };
        let tool = params["name"].as_str().unwrap_or_default().to_owned();
        let result = match method {
            "initialize" => {
                let mut caps =
                    json!({"tools": {"listChanged": true}, "resources": {}, "logging": {}});
                if self.prompts {
                    caps["prompts"] = json!({});
                }
                json!({"protocolVersion": self.version, "capabilities": caps,
                    "serverInfo": {"name": ns}})
            }
            "tools/list" if self.fail_lists => {
                let error = json!({"jsonrpc": "2.0", "id": id,
                    "error": {"code": -32603, "message": "list is broken"}});
                tokio::spawn(async move {
                    let _ = tx.send(error).await;
                });
                return Ok(());
            }
            "tools/list" if self.pages && params["cursor"].is_null() => {
                json!({"tools": [{"name": "echo"}], "nextCursor": "p2"})
            }
            "tools/list" if self.pages => json!({"tools": [{"name": format!("only_{ns}")}]}),
            "tools/list" => json!({"tools": [{"name": "echo"}, {"name": format!("only_{ns}")}]}),
            // Never answered, as a server does with a request it was told to
            // give up on.
            "tools/call" if tool == "hang" => return Ok(()),
            // Gone, as a process that exited.
            "tools/call" if tool == "die" => {
                *self.tx.lock().unwrap() = None;
                return Ok(());
            }
            "tools/call" if tool == "ask" || tool == "ask_then_cancel" => {
                *self.asked.lock().unwrap() = Some(id.clone());
                let ask = json!({"jsonrpc": "2.0", "id": 0, "method": "sampling/createMessage",
                    "params": {"from": ns}});
                let cancel = tool == "ask_then_cancel";
                tokio::spawn(async move {
                    let _ = tx.send(ask).await;
                    if cancel {
                        let _ = tx
                            .send(
                                json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
                                "params": {"requestId": 0}}),
                            )
                            .await;
                    }
                });
                return Ok(());
            }
            "tools/call" => json!({"content": [
                {"type": "text", "text": format!("{ns}:{}:{}", params["name"], params["arguments"])},
                {"type": "resource_link", "uri": format!("linked://{ns}/x"), "name": "x"},
            ]}),
            "prompts/list" => json!({"prompts": [{"name": "brief"}]}),
            "prompts/get" => json!({"messages": [{"role": "user",
                "content": {"type": "text", "text": format!("{ns} prompt {}", params["name"])}}]}),
            "resources/list" => json!({"resources": [
                {"uri": "shared://both", "name": "shared"},
                {"uri": format!("own://{ns}/doc"), "name": "own"}]}),
            "resources/templates/list" => json!({"resourceTemplates": [
                {"uriTemplate": format!("tpl://{ns}/{{id}}"), "name": "tpl"}]}),
            "resources/read" => json!({"contents": [
                {"uri": params["uri"], "text": format!("{ns} read")}]}),
            "resources/subscribe" | "resources/unsubscribe" | "logging/setLevel" => json!({}),
            "completion/complete" => json!({"completion": {"values": [ns]}}),
            _ => {
                let error = json!({"jsonrpc": "2.0", "id": id,
                    "error": {"code": -32601, "message": "Method not found"}});
                tokio::spawn(async move {
                    let _ = tx.send(error).await;
                });
                return Ok(());
            }
        };
        tokio::spawn(async move {
            let _ = tx.send(reply(&id, result)).await;
        });
        Ok(())
    }

    async fn close(&self) {}
}

/// A server that cannot be opened.
struct Broken;

#[async_trait]
impl Connector for Broken {
    async fn open(&self, _: StderrSink) -> Result<Opened, UpstreamError> {
        Err(UpstreamError::Failed("it will not start".into()))
    }
}

fn rich(namespace: &'static str, prompts: bool, pages: bool) -> Arc<Rich> {
    Arc::new(Rich {
        namespace,
        prompts,
        pages,
        version: "2025-11-25",
        fail_lists: false,
    })
}

/// The root of a gateway over `a` (with prompts) and `b` (with pages).
async fn root_of_two() -> (Harness, String) {
    let h = start(vec![
        ("a", rich("a", true, false)),
        ("b", rich("b", false, true)),
    ])
    .await;
    let response = h.post("", None, &initialize()).await;
    assert_eq!(response.status(), 200);
    let session = session_of(&response);
    messages(response).await;
    let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    assert_eq!(h.post("", Some(&session), &note).await.status(), 202);
    (h, session)
}

async fn ask(h: &Harness, session: &str, id: i64, method: &str, params: Value) -> Value {
    let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let got = messages(h.post("", Some(session), &message).await).await;
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0]["id"], id);
    got[0].clone()
}

#[tokio::test]
async fn the_root_answers_for_all_the_servers_at_once() {
    let h = start(vec![
        ("a", rich("a", true, false)),
        ("b", rich("b", false, true)),
    ])
    .await;
    let response = h.post("", None, &initialize()).await;
    assert_eq!(response.status(), 200);
    let session = session_of(&response);
    let init = messages(response).await;
    let result = &init[0]["result"];
    assert_eq!(init[0]["id"], 1);
    assert_eq!(result["serverInfo"]["name"], "coco-mcp-gateway");
    assert_eq!(result["protocolVersion"], "2025-11-25");
    let caps = &result["capabilities"];
    assert_eq!(caps["tools"]["listChanged"], true);
    assert!(caps.get("resources").is_some() && caps.get("logging").is_some());
    assert!(caps.get("prompts").is_some(), "one of them has prompts");

    // Each server was started, told who the agent is, and shaken hands with.
    for ns in ["a", "b"] {
        assert_eq!(
            h.observer.of(ns),
            [
                format!("opened {ns}"),
                "client test-agent 9.1".to_owned(),
                "-> initialize".to_owned(),
                "<- result \"coco:init\"".to_owned(),
                "-> notifications/initialized".to_owned(),
            ],
            "{ns}"
        );
    }
    // It is its own session, and ping is answered without asking anyone.
    let pong = ask(&h, &session, 2, "ping", json!({})).await;
    assert_eq!(pong["result"], json!({}));
    h.gateway.stop().await;
}

#[tokio::test]
async fn the_root_speaks_initialize_and_nothing_else_first() {
    let h = start(vec![("a", rich("a", false, false))]).await;
    let discover = json!({"jsonrpc": "2.0", "id": 1, "method": "server/discover"});
    let refused = h.post("", None, &discover).await;
    assert_eq!(refused.status(), 400);
    assert!(refused.text().await.unwrap().contains("-32601"));
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
    assert_eq!(h.post("", None, &list).await.status(), 400);
    assert_eq!(h.gateway.connections(), 0);
    // A second initialize is refused: the handshake was done.
    let response = h.post("", None, &initialize()).await;
    let session = session_of(&response);
    messages(response).await;
    let again = ask(&h, &session, 5, "initialize", json!({})).await;
    assert_eq!(again["error"]["code"], -32600);
    h.gateway.stop().await;
}

#[tokio::test]
async fn tools_and_prompts_are_listed_with_their_server_in_front() {
    let (h, session) = root_of_two().await;
    let tools = ask(&h, &session, 2, "tools/list", json!({})).await;
    let mut names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    names.sort_unstable();
    // b's list came in two pages; both are here.
    assert_eq!(names, ["a__echo", "a__only_a", "b__echo", "b__only_b"]);
    let prompts = ask(&h, &session, 3, "prompts/list", json!({})).await;
    assert_eq!(
        prompts["result"]["prompts"][0]["name"], "a__brief",
        "only a has any"
    );
    h.gateway.stop().await;
}

#[tokio::test]
async fn a_call_goes_to_the_server_its_name_says() {
    let (h, session) = root_of_two().await;
    let call = ask(
        &h,
        &session,
        2,
        "tools/call",
        json!({"name": "b__echo", "arguments": {"x": 1}}),
    )
    .await;
    assert_eq!(
        call["result"]["content"][0]["text"], "b:\"echo\":{\"x\":1}",
        "stripped for b"
    );
    assert!(h.observer.of("b").contains(&"-> tools/call".to_owned()));
    assert!(!h.observer.of("a").contains(&"-> tools/call".to_owned()));

    let prompt = ask(&h, &session, 3, "prompts/get", json!({"name": "a__brief"})).await;
    assert_eq!(
        prompt["result"]["messages"][0]["content"]["text"],
        "a prompt \"brief\""
    );
    let unknown = ask(
        &h,
        &session,
        4,
        "tools/call",
        json!({"name": "nobody__echo"}),
    )
    .await;
    assert_eq!(unknown["error"]["code"], -32602);
    let bare = ask(&h, &session, 5, "tools/call", json!({"name": "echo"})).await;
    assert_eq!(
        bare["error"]["code"], -32602,
        "a name with no server in front"
    );
    let missing = ask(&h, &session, 6, "no/such/method", json!({})).await;
    assert_eq!(missing["error"]["code"], -32601);
    h.gateway.stop().await;
}

#[tokio::test]
async fn resources_keep_their_uris_and_are_read_from_the_server_that_serves_them() {
    let (h, session) = root_of_two().await;
    let list = ask(&h, &session, 2, "resources/list", json!({})).await;
    let uris: Vec<&str> = list["result"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["uri"].as_str().unwrap())
        .collect();
    assert_eq!(
        uris,
        ["shared://both", "own://a/doc", "own://b/doc"],
        "listed once"
    );
    // The server after the first is told it lost the URI.
    assert!(
        h.observer
            .of("b")
            .iter()
            .any(|l| l.contains("shared://both") && l.contains("serves it at the root")),
        "{:?}",
        h.observer.of("b")
    );
    let templates = ask(&h, &session, 3, "resources/templates/list", json!({})).await;
    assert_eq!(
        templates["result"]["resourceTemplates"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let read = |uri: &'static str| json!({"uri": uri});
    let first = ask(&h, &session, 4, "resources/read", read("shared://both")).await;
    assert_eq!(
        first["result"]["contents"][0]["text"], "a read",
        "the first in order: {first}"
    );
    let own = ask(&h, &session, 5, "resources/read", read("own://b/doc")).await;
    assert_eq!(own["result"]["contents"][0]["text"], "b read");
    let by_template = ask(&h, &session, 6, "resources/read", read("tpl://b/7")).await;
    assert_eq!(by_template["result"]["contents"][0]["text"], "b read");
    let nowhere = ask(&h, &session, 7, "resources/read", read("nowhere://x")).await;
    assert_eq!(nowhere["error"]["code"], -32002);
    assert_eq!(nowhere["error"]["data"]["uri"], "nowhere://x");

    // A URI a tool result linked to is served by the tool's server.
    ask(
        &h,
        &session,
        8,
        "tools/call",
        json!({"name": "b__echo", "arguments": {}}),
    )
    .await;
    let linked = ask(&h, &session, 9, "resources/read", read("linked://b/x")).await;
    assert_eq!(linked["result"]["contents"][0]["text"], "b read");
    // Completion and subscription follow the same ways.
    let completion = ask(
        &h,
        &session,
        10,
        "completion/complete",
        json!({"ref": {"type": "ref/resource", "uri": "own://b/doc"}, "argument": {}}),
    )
    .await;
    assert_eq!(completion["result"]["completion"]["values"][0], "b");
    let prompt = ask(
        &h,
        &session,
        11,
        "completion/complete",
        json!({"ref": {"type": "ref/prompt", "name": "a__brief"}, "argument": {}}),
    )
    .await;
    assert_eq!(prompt["result"]["completion"]["values"][0], "a");
    h.gateway.stop().await;
}

#[tokio::test]
async fn a_request_a_server_makes_goes_to_the_agent_and_its_answer_back() {
    let (h, session) = root_of_two().await;
    let call = json!({"jsonrpc": "2.0", "id": 21, "method": "tools/call",
        "params": {"name": "b__ask", "arguments": {}}});
    let response = h.post("", Some(&session), &call).await;
    assert_eq!(response.status(), 200);
    use futures::StreamExt as _;
    let mut stream = response.bytes_stream();
    let mut seen = String::new();
    // The server (b, the second) asks the agent something, under an id that
    // says who asked and what it was.
    let asked = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(chunk) = stream.next().await {
            seen.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
            if seen.contains("sampling/createMessage") {
                break;
            }
        }
    })
    .await;
    assert!(asked.is_ok(), "{seen}");
    let line = seen.lines().find(|l| l.contains("sampling")).unwrap();
    let request: Value = serde_json::from_str(line.trim_start_matches("data:").trim()).unwrap();
    assert_eq!(request["id"], "coco-1-0", "server 1, its own id 0");
    assert_eq!(request["params"]["from"], "b");

    let answer = json!({"jsonrpc": "2.0", "id": "coco-1-0", "result": {"text": "yes"}});
    assert_eq!(h.post("", Some(&session), &answer).await.status(), 202);
    let finished = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(chunk) = stream.next().await {
            seen.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
        }
    })
    .await;
    assert!(finished.is_ok(), "{seen}");
    assert!(
        seen.contains("answer:\\\"yes\\\"") || seen.contains("answer:"),
        "{seen}"
    );
    assert!(
        seen.contains("\"id\":21"),
        "the call's own answer, under the agent's id"
    );
    h.gateway.stop().await;
}

#[tokio::test]
async fn a_server_that_cannot_open_is_left_out_and_a_root_of_none_is_refused() {
    let h = start(vec![
        ("a", rich("a", false, false)),
        ("broken", Arc::new(Broken)),
    ])
    .await;
    let response = h.post("", None, &initialize()).await;
    assert_eq!(response.status(), 200);
    let session = session_of(&response);
    messages(response).await;
    let tools = ask(&h, &session, 2, "tools/list", json!({})).await;
    assert_eq!(
        tools["result"]["tools"].as_array().unwrap().len(),
        2,
        "only a's"
    );
    assert!(
        h.observer
            .of("broken")
            .iter()
            .any(|l| l.contains("could not open") && l.contains("it will not start"))
    );
    h.gateway.stop().await;

    let none = start(vec![("broken", Arc::new(Broken))]).await;
    let refused = none.post("", None, &initialize()).await;
    assert_eq!(refused.status(), 502);
    assert_eq!(none.gateway.connections(), 0);
    none.gateway.stop().await;
}

#[tokio::test]
async fn changing_a_server_ends_the_root_and_a_new_one_has_the_new_servers() {
    let (h, session) = root_of_two().await;
    let a = h.gateway.connections();
    assert_eq!(a, 1);
    h.gateway
        .set_servers(vec![
            RelayServer {
                namespace: "a".into(),
                connector: rich("a", true, false),
            },
            RelayServer {
                namespace: "c".into(),
                connector: rich("c", false, false),
            },
        ])
        .await;
    let ping = json!({"jsonrpc": "2.0", "id": 9, "method": "ping"});
    assert_eq!(h.post("", Some(&session), &ping).await.status(), 404);
    let response = h.post("", None, &initialize()).await;
    let fresh = session_of(&response);
    messages(response).await;
    let tools = ask(&h, &fresh, 2, "tools/list", json!({})).await;
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"c__echo") && !names.iter().any(|n| n.starts_with("b__")),
        "{names:?}"
    );
    h.gateway.set_servers(vec![]).await;
    assert_eq!(
        h.post("", None, &initialize()).await.status(),
        404,
        "no servers, no root"
    );
    h.gateway.stop().await;
}

#[tokio::test]
async fn the_mock_servers_merged_at_the_root() {
    let h = start(vec![
        ("one", Arc::new(MockOverStreams(Schema::V1))),
        ("two", Arc::new(MockOverStreams(Schema::V2))),
    ])
    .await;
    let response = h.post("", None, &initialize()).await;
    assert_eq!(response.status(), 200);
    let session = session_of(&response);
    messages(response).await;
    let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    assert_eq!(h.post("", Some(&session), &note).await.status(), 202);

    let tools = ask(&h, &session, 2, "tools/list", json!({})).await;
    let names: Vec<String> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(names.contains(&"one__echo".to_owned()), "{names:?}");
    assert!(names.contains(&"two__add".to_owned()), "{names:?}");
    assert!(
        names
            .iter()
            .all(|n| n.starts_with("one__") || n.starts_with("two__"))
    );

    let echoed = ask(
        &h,
        &session,
        3,
        "tools/call",
        json!({"name": "one__echo", "arguments": {"text": "merged"}}),
    )
    .await;
    assert_eq!(echoed["result"]["content"][0]["text"], "merged");
    assert!(h.observer.of("one").contains(&"-> tools/call".to_owned()));
    assert!(!h.observer.of("two").contains(&"-> tools/call".to_owned()));

    // Both list `mock://text/hello`: it is listed once, and read from one.
    let list = ask(&h, &session, 4, "resources/list", json!({})).await;
    let hello = list["result"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["uri"] == "mock://text/hello")
        .count();
    assert_eq!(hello, 1);
    let read = ask(
        &h,
        &session,
        5,
        "resources/read",
        json!({"uri": "mock://text/hello"}),
    )
    .await;
    assert!(
        read["result"]["contents"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("Hello")
    );
    h.gateway.stop().await;
}

// ---------------------------------------------- what the root's review found

#[tokio::test]
async fn a_root_connection_the_agent_walked_away_from_is_closed() {
    let limits = Limits {
        idle: Duration::from_millis(150),
        sweep: Duration::from_millis(50),
        ..Limits::default()
    };
    let h = start_with(
        vec![
            ("a", rich("a", false, false)),
            ("b", rich("b", false, false)),
        ],
        limits,
    )
    .await;
    let response = h.post("", None, &initialize()).await;
    let session = session_of(&response);
    messages(response).await;
    // A request no server answers, whose stream the agent drops.
    let hang = json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call",
        "params": {"name": "b__hang", "arguments": {}}});
    let abandoned = h.post("", Some(&session), &hang).await;
    assert_eq!(h.gateway.connections(), 1);
    drop(abandoned);
    wait_for("the sweep to end the root", || h.gateway.connections() == 0).await;
    h.gateway.stop().await;
}

#[tokio::test]
async fn a_request_with_what_it_needs_missing_is_answered_not_left_waiting() {
    let (h, session) = root_of_two().await;
    for (id, method, params) in [
        (2, "resources/read", json!({})),
        (3, "resources/subscribe", json!({"nothing": true})),
        (
            4,
            "completion/complete",
            json!({"ref": {"type": "ref/resource"}}),
        ),
        (
            5,
            "completion/complete",
            json!({"ref": {"type": "ref/prompt"}}),
        ),
        (
            6,
            "completion/complete",
            json!({"ref": {"type": "ref/other"}}),
        ),
        (7, "tools/call", json!({})),
    ] {
        let got = ask(&h, &session, id, method, params).await;
        assert_eq!(got["error"]["code"], -32602, "{method}: {got}");
    }
    // And the connection is not held by them.
    assert_eq!(h.gateway.connections(), 1);
    h.gateway.stop().await;
}

#[tokio::test]
async fn what_a_tool_linked_is_still_served_after_the_lists_are_read_again() {
    let (h, session) = root_of_two().await;
    ask(
        &h,
        &session,
        2,
        "tools/call",
        json!({"name": "a__echo", "arguments": {}}),
    )
    .await;
    // Listing again forgets what was listed, not what a result linked.
    ask(&h, &session, 3, "resources/list", json!({})).await;
    ask(&h, &session, 4, "resources/templates/list", json!({})).await;
    let read = ask(
        &h,
        &session,
        5,
        "resources/read",
        json!({"uri": "linked://a/x"}),
    )
    .await;
    assert_eq!(read["result"]["contents"][0]["text"], "a read", "{read}");
    h.gateway.stop().await;
}

#[tokio::test]
async fn reads_of_uris_nobody_listed_yet_that_arrive_together_all_find_their_server() {
    let (h, session) = root_of_two().await;
    let h = Arc::new(h);
    let mut tasks = Vec::new();
    for id in 10..16 {
        let (h, session) = (h.clone(), session.clone());
        tasks.push(tokio::spawn(async move {
            ask(
                &h,
                &session,
                id,
                "resources/read",
                json!({"uri": "own://b/doc"}),
            )
            .await
        }));
    }
    for task in tasks {
        let read = task.await.unwrap();
        assert_eq!(read["result"]["contents"][0]["text"], "b read", "{read}");
    }
    h.gateway.stop().await;
}

#[tokio::test]
async fn ids_the_gateway_makes_for_itself_are_not_the_agents_to_use() {
    let (h, session) = root_of_two().await;
    for reserved in ["coco:3", "coco-0-1"] {
        let message = json!({"jsonrpc": "2.0", "id": reserved, "method": "tools/list"});
        let got = messages(h.post("", Some(&session), &message).await).await;
        assert_eq!(got[0]["error"]["code"], -32600, "{reserved}: {got:?}");
    }
    // Other string ids are the agent's, and come back as they went.
    let message = json!({"jsonrpc": "2.0", "id": "my-id", "method": "ping"});
    let got = messages(h.post("", Some(&session), &message).await).await;
    assert_eq!(got[0]["id"], "my-id");
    h.gateway.stop().await;
}

#[tokio::test]
async fn the_root_speaks_the_oldest_version_every_server_agreed_to() {
    let old = Arc::new(Rich {
        namespace: "old",
        prompts: false,
        pages: false,
        version: "2025-06-18",
        fail_lists: false,
    });
    let h = start(vec![("a", rich("a", false, false)), ("old", old)]).await;
    let response = h.post("", None, &initialize()).await;
    let init = messages(response).await;
    assert_eq!(init[0]["result"]["protocolVersion"], "2025-06-18");
    h.gateway.stop().await;

    let same = start(vec![("a", rich("a", false, false))]).await;
    let init = messages(same.post("", None, &initialize()).await).await;
    assert_eq!(
        init[0]["result"]["protocolVersion"], "2025-11-25",
        "as asked"
    );
    same.gateway.stop().await;
}

#[tokio::test]
async fn a_server_that_cancels_its_own_request_names_it_as_the_agent_knows_it() {
    let (h, session) = root_of_two().await;
    let call = json!({"jsonrpc": "2.0", "id": 30, "method": "tools/call",
        "params": {"name": "b__ask_then_cancel", "arguments": {}}});
    let response = h.post("", Some(&session), &call).await;
    use futures::StreamExt as _;
    let mut stream = response.bytes_stream();
    let mut seen = String::new();
    let got = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(chunk) = stream.next().await {
            seen.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
            if seen.contains("notifications/cancelled") {
                break;
            }
        }
    })
    .await;
    assert!(got.is_ok(), "{seen}");
    // The request was `coco-1-0`; the cancellation says so.
    assert!(seen.contains("\"id\":\"coco-1-0\""), "{seen}");
    assert!(seen.contains("\"requestId\":\"coco-1-0\""), "{seen}");
    h.gateway.stop().await;
}

#[tokio::test]
async fn a_server_that_goes_away_mid_request_fails_it_and_leaves_the_others() {
    let (h, session) = root_of_two().await;
    let die = ask(
        &h,
        &session,
        2,
        "tools/call",
        json!({"name": "b__die", "arguments": {}}),
    )
    .await;
    assert_eq!(die["error"]["code"], -32603);
    assert!(
        die["error"]["message"]
            .as_str()
            .unwrap()
            .contains("went away"),
        "{die}"
    );
    assert!(h.observer.of("b").iter().any(|l| l.starts_with("closed")));
    // a goes on; b's tools are gone from the list and calls to it fail.
    let echo = ask(
        &h,
        &session,
        3,
        "tools/call",
        json!({"name": "a__echo", "arguments": {}}),
    )
    .await;
    assert!(echo.get("result").is_some(), "{echo}");
    let tools = ask(&h, &session, 4, "tools/list", json!({})).await;
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.iter().all(|n| n.starts_with("a__")), "{names:?}");
    // With every server gone the root is over.
    ask(
        &h,
        &session,
        5,
        "tools/call",
        json!({"name": "a__die", "arguments": {}}),
    )
    .await;
    wait_for("the root to end", || h.gateway.connections() == 0).await;
    h.gateway.stop().await;
}

#[tokio::test]
async fn a_list_that_fails_leaves_that_server_out_and_not_the_answer() {
    let broken = Arc::new(Rich {
        namespace: "broken",
        prompts: false,
        pages: false,
        version: "2025-11-25",
        fail_lists: true,
    });
    let h = start(vec![("a", rich("a", false, false)), ("broken", broken)]).await;
    let response = h.post("", None, &initialize()).await;
    let session = session_of(&response);
    messages(response).await;
    let tools = ask(&h, &session, 2, "tools/list", json!({})).await;
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["a__echo", "a__only_a"]);
    assert!(
        h.observer
            .of("broken")
            .iter()
            .any(|l| l.contains("left out of the list") && l.contains("list is broken"))
    );
    h.gateway.stop().await;
}

#[tokio::test]
async fn subscriptions_levels_and_cancellations_are_routed() {
    let (h, session) = root_of_two().await;
    let sub = ask(
        &h,
        &session,
        2,
        "resources/subscribe",
        json!({"uri": "own://b/doc"}),
    )
    .await;
    assert_eq!(sub["result"], json!({}));
    assert!(
        h.observer
            .of("b")
            .contains(&"-> resources/subscribe".to_owned())
    );
    assert!(
        !h.observer
            .of("a")
            .contains(&"-> resources/subscribe".to_owned())
    );

    // A level is set on every server that logs.
    let level = ask(
        &h,
        &session,
        3,
        "logging/setLevel",
        json!({"level": "debug"}),
    )
    .await;
    assert_eq!(level["result"], json!({}));
    for ns in ["a", "b"] {
        assert!(
            h.observer
                .of(ns)
                .contains(&"-> logging/setLevel".to_owned()),
            "{ns}"
        );
    }

    // A request in flight is cancelled at the server that has it.
    let hang = json!({"jsonrpc": "2.0", "id": 40, "method": "tools/call",
        "params": {"name": "b__hang", "arguments": {}}});
    let _waiting = h.post("", Some(&session), &hang).await;
    let cancel = json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
        "params": {"requestId": 40}});
    assert_eq!(h.post("", Some(&session), &cancel).await.status(), 202);
    wait_for("the cancellation at b", || {
        h.observer
            .of("b")
            .contains(&"-> notifications/cancelled".to_owned())
    })
    .await;
    assert!(
        !h.observer
            .of("a")
            .contains(&"-> notifications/cancelled".to_owned())
    );
    h.gateway.stop().await;
}

#[tokio::test]
async fn only_the_servers_after_the_first_to_list_a_uri_are_told_it_is_not_theirs() {
    let h = start(vec![
        ("a", rich("a", false, false)),
        ("b", rich("b", false, false)),
        ("c", rich("c", false, false)),
    ])
    .await;
    let response = h.post("", None, &initialize()).await;
    let session = session_of(&response);
    messages(response).await;
    ask(&h, &session, 2, "resources/list", json!({})).await;
    let told = |ns: &str| {
        h.observer
            .of(ns)
            .iter()
            .any(|l| l.contains("shared://both") && l.contains("serves it at the root"))
    };
    assert!(!told("a"), "the first lists it and serves it");
    assert!(told("b") && told("c"));
    h.gateway.stop().await;
}

#[tokio::test]
async fn a_server_that_cannot_open_leaves_a_trace_in_its_log() {
    let h = start(vec![
        ("a", rich("a", false, false)),
        ("broken", Arc::new(Broken)),
    ])
    .await;
    let response = h.post("", None, &initialize()).await;
    messages(response).await;
    // That an agent reached for it, who, why it could not be opened, and that
    // it is gone: in that order, so a log can show all of it.
    let lines = h.observer.of("broken");
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert_eq!(lines[0], "opened broken");
    assert_eq!(lines[1], "client test-agent 9.1");
    assert!(lines[2].contains("could not open") && lines[2].contains("it will not start"));
    assert_eq!(lines[3], "closed could not be opened");
    h.gateway.stop().await;
}

/// A server that answers every POST with an event stream that carries a
/// notification and ends: it never answers the request.
async fn unanswering_sse_server() -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                // The request: its head, then the body it announces.
                let mut seen = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let Ok(n) = socket.read(&mut chunk).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    seen.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&seen).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text[..end]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if seen.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                let body = "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{}}\n\n";
                let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nmcp-session-id: up-1\r\nconnection: close\r\n\r\n";
                let _ = socket.write_all(format!("{head}{body}").as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    url
}

#[tokio::test]
async fn an_event_stream_that_ends_without_an_answer_is_answered_with_an_error() {
    let url = unanswering_sse_server().await;
    let h = start(vec![("web", connector(&url, Arc::new(NoAuth)))]).await;
    let response = h.post("web", None, &initialize()).await;
    assert_eq!(response.status(), 200);
    let got = tokio::time::timeout(Duration::from_secs(5), messages(response))
        .await
        .expect("the stream ended instead of hanging");
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!(got[0]["method"], "notifications/progress");
    assert_eq!(got[1]["id"], 1);
    assert!(
        got[1]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("ended before it answered")
    );
    h.gateway.stop().await;
}

#[tokio::test]
async fn servers_over_http_are_merged_at_the_root_too() {
    let one = http_mock(HttpAuth::None).await;
    let two = http_mock(HttpAuth::Bearer("two-secret".into())).await;
    let h = start(vec![
        ("one", connector(&one.url, Arc::new(NoAuth))),
        (
            "two",
            connector(&two.url, Arc::new(Bearer("two-secret".into()))),
        ),
    ])
    .await;
    let response = h.post("", None, &initialize()).await;
    assert_eq!(response.status(), 200);
    let session = session_of(&response);
    messages(response).await;
    let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    assert_eq!(h.post("", Some(&session), &note).await.status(), 202);
    let tools = ask(&h, &session, 2, "tools/list", json!({})).await;
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"one__echo") && names.contains(&"two__echo"),
        "{names:?}"
    );
    let echoed = ask(
        &h,
        &session,
        3,
        "tools/call",
        json!({"name": "two__echo", "arguments": {"text": "over http"}}),
    )
    .await;
    assert_eq!(
        echoed["result"]["content"][0]["text"], "over http",
        "{echoed}"
    );
    h.gateway.stop().await;
    one.shutdown();
    two.shutdown();
}
