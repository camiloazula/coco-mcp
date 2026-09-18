//! Both protocol eras over streamable HTTP, against the mock server's HTTP
//! mode: a 2026-07-28 request is served without a session, so what works
//! over a duplex is proven again here.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use mcp_core::{
    AuthRef, ElicitationAction, Era, Event, EventKind, EventSink, LEGACY_VERSION, MODERN_VERSION,
    ProtocolMode, RequestControl, ServerRequestKind, ServerResponse, ServerSpec, Session,
    SessionOptions,
};
use mcp_mockserver::http::{HttpAuth, HttpServer, serve_http_with};
use mcp_mockserver::{COUNTER_URI, Faults, Schema};
use serde_json::json;
use tokio::sync::broadcast;

async fn http(faults: Faults) -> HttpServer {
    serve_http_with(Schema::V1, faults, HttpAuth::None, "127.0.0.1:0")
        .await
        .unwrap()
}

fn spec(server: &HttpServer) -> ServerSpec {
    ServerSpec::Http {
        url: server.url.clone(),
        headers: Default::default(),
        auth: AuthRef::None,
    }
}

async fn connect(
    server: &HttpServer,
    protocol: ProtocolMode,
    sink: Option<EventSink>,
) -> mcp_core::Result<Session> {
    let options = SessionOptions {
        protocol,
        sink,
        ..SessionOptions::default()
    };
    Session::connect(spec(server), options).await
}

async fn wait_for(events: &mut broadcast::Receiver<Event>, wanted: impl Fn(&EventKind) -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await {
                Ok(event) if wanted(&event.kind) => return,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => panic!("the session ended"),
            }
        }
    })
    .await
    .expect("the event arrived");
}

#[tokio::test]
async fn each_mode_connects_over_http() {
    let server = http(Faults::default()).await;
    for (mode, era, version) in [
        (ProtocolMode::Legacy, Era::Legacy, LEGACY_VERSION),
        (ProtocolMode::Auto, Era::Modern, MODERN_VERSION),
        (ProtocolMode::Modern, Era::Modern, MODERN_VERSION),
    ] {
        let session = connect(&server, mode, None).await.unwrap();
        assert_eq!(session.era(), era, "{mode:?}");
        let snapshot = session.snapshot().await.unwrap();
        assert_eq!(snapshot.protocol_version, version, "{mode:?}");
        assert!(snapshot.list_failures.is_empty(), "{mode:?}");
        let echo = session
            .call_tool("echo", json!({"text": "over http"}))
            .await
            .unwrap();
        assert_eq!(echo.content[0]["text"], "over http", "{mode:?}");
    }
}

#[tokio::test]
async fn auto_falls_back_over_http_and_modern_explains_why_it_cannot() {
    let server = http(Faults {
        legacy_only: true,
        ..Faults::default()
    })
    .await;
    let session = connect(&server, ProtocolMode::Auto, None).await.unwrap();
    assert_eq!(session.era(), Era::Legacy);
    assert!(session.snapshot().await.unwrap().tool("echo").is_some());
    let err = connect(&server, ProtocolMode::Modern, None)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("Legacy or Auto"), "{err}");
}

#[tokio::test]
async fn a_modern_http_session_round_trips_and_hears_its_stream() {
    let server = http(Faults::default()).await;
    let sink = EventSink::new(1024);
    let mut events = sink.subscribe();
    let mut requests = sink.subscribe();
    tokio::spawn(async move {
        while let Ok(event) = requests.recv().await {
            if let EventKind::ServerRequest(request) = event.kind
                && matches!(request.kind, ServerRequestKind::Elicitation { .. })
            {
                request.respond(ServerResponse::Elicitation {
                    action: ElicitationAction::Accept,
                    content: Some(json!({"answer": "over http"})),
                });
            }
        }
    });
    let session = connect(&server, ProtocolMode::Modern, Some(sink))
        .await
        .unwrap();
    let elicited = session
        .call_tool("elicit", json!({"question": "where?"}))
        .await
        .unwrap();
    assert_eq!(elicited.content[0]["text"], "answer=over http");

    session.subscribe_resource(COUNTER_URI).await.unwrap();
    session.call_tool("bump", json!({})).await.unwrap();
    wait_for(
        &mut events,
        |kind| matches!(kind, EventKind::ResourceUpdated { uri } if uri == COUNTER_URI),
    )
    .await;
}

#[tokio::test]
async fn cancelling_a_modern_http_call_ends_it() {
    let server = http(Faults::default()).await;
    let session = connect(&server, ProtocolMode::Modern, None).await.unwrap();
    let control = RequestControl::new();
    let call = {
        let session = session.clone();
        let control = control.clone();
        tokio::spawn(async move {
            session
                .call_tool_with("sleep", json!({"millis": 5000}), &control)
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    let started = Instant::now();
    control.cancel();
    let err = call.await.unwrap().unwrap_err();
    assert!(matches!(err, mcp_core::Error::Cancelled), "{err}");
    assert!(started.elapsed() < Duration::from_secs(2));
    let echo = session
        .call_tool("echo", json!({"text": "still here"}))
        .await
        .unwrap();
    assert_eq!(echo.content[0]["text"], "still here");
}
