//! Connecting in each protocol era against the in-process mock server, and
//! what works differently once connected: input requests, subscriptions,
//! the log level, roots.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use mcp_core::{
    CallOutcome, ElicitationAction, ElicitationMode, Era, Event, EventKind, EventSink,
    LEGACY_VERSION, ListKind, MAX_INPUT_ROUNDS, MODERN_VERSION, ProtocolMode, RequestControl, Root,
    ServerRequestKind, ServerResponse, ServerSpec, Session, SessionOptions,
};
use mcp_mockserver::{COUNTER_URI, Faults, MockServer, SERVER_NAME, Schema};
use serde_json::json;
use tokio::sync::broadcast;

fn spec() -> ServerSpec {
    ServerSpec::Stdio {
        command: "in-process".into(),
        args: vec![],
        env: Default::default(),
        cwd: None,
    }
}

fn options(protocol: ProtocolMode) -> SessionOptions {
    SessionOptions {
        protocol,
        ..SessionOptions::default()
    }
}

/// Connect to a mock with `faults`, over as many transports as the connect
/// asks for.
async fn connect(protocol: ProtocolMode, faults: Faults) -> mcp_core::Result<Session> {
    Session::connect_with_transports(
        spec(),
        move || Some(MockServer::serve_duplex_with(Schema::V1, faults)),
        options(protocol),
    )
    .await
}

/// Connect to a healthy mock, reporting to `sink`.
async fn connect_with(protocol: ProtocolMode, sink: EventSink) -> Session {
    let options = SessionOptions {
        protocol,
        sink: Some(sink),
        ..SessionOptions::default()
    };
    Session::connect_with_transport(spec(), MockServer::serve_duplex(Schema::V1), options)
        .await
        .unwrap()
}

/// Answer every request the session asks a person about with `answer`.
fn answering(sink: &EventSink, answer: fn(&ServerRequestKind) -> ServerResponse) {
    let mut events = sink.subscribe();
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(Event {
                    kind: EventKind::ServerRequest(request),
                    ..
                }) => {
                    request.respond(answer(&request.kind));
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    });
}

/// Wait for an event `wanted` accepts.
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

fn text(outcome: CallOutcome) -> String {
    outcome.content[0]["text"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn each_mode_agrees_on_its_era() {
    for (mode, era, version) in [
        (ProtocolMode::Legacy, Era::Legacy, LEGACY_VERSION),
        (ProtocolMode::Auto, Era::Modern, MODERN_VERSION),
        (ProtocolMode::Modern, Era::Modern, MODERN_VERSION),
    ] {
        let session = connect(mode, Faults::default()).await.unwrap();
        assert_eq!(session.era(), era, "{mode:?}");
        let snapshot = session.snapshot().await.unwrap();
        assert_eq!(snapshot.protocol_version, version, "{mode:?}");
    }
}

#[tokio::test]
async fn a_snapshot_reads_the_same_in_either_era() {
    let legacy = connect(ProtocolMode::Legacy, Faults::default())
        .await
        .unwrap()
        .snapshot()
        .await
        .unwrap();
    let modern = connect(ProtocolMode::Modern, Faults::default())
        .await
        .unwrap()
        .snapshot()
        .await
        .unwrap();
    assert_eq!(modern.server_name(), Some(SERVER_NAME));
    assert_eq!(modern.instructions, legacy.instructions);
    assert!(
        modern.list_failures.is_empty(),
        "{:?}",
        modern.list_failures
    );
    let mut same_version = modern.clone();
    same_version.protocol_version = legacy.protocol_version.clone();
    assert_eq!(same_version.content(), legacy.content());
}

#[tokio::test]
async fn auto_falls_back_to_the_handshake_for_a_legacy_only_server() {
    let faults = Faults {
        legacy_only: true,
        ..Faults::default()
    };
    let session = connect(ProtocolMode::Auto, faults).await.unwrap();
    assert_eq!(session.era(), Era::Legacy);
    let snapshot = session.snapshot().await.unwrap();
    assert_eq!(snapshot.protocol_version, LEGACY_VERSION);
    assert!(snapshot.tool("echo").is_some());
}

#[tokio::test]
async fn auto_opens_a_second_transport_when_discovery_shares_no_version() {
    let faults = Faults {
        legacy_versions: true,
        ..Faults::default()
    };
    let opened = Arc::new(AtomicUsize::new(0));
    let count = opened.clone();
    let session = Session::connect_with_transports(
        spec(),
        move || {
            count.fetch_add(1, Ordering::Relaxed);
            Some(MockServer::serve_duplex_with(Schema::V1, faults))
        },
        options(ProtocolMode::Auto),
    )
    .await
    .unwrap();
    assert_eq!(opened.load(Ordering::Relaxed), 2);
    assert_eq!(session.era(), Era::Legacy);
    let echo = session
        .call_tool("echo", json!({"text": "after the fallback"}))
        .await
        .unwrap();
    assert_eq!(text(echo), "after the fallback");

    // With one transport there is nothing to fall back on.
    let err = Session::connect_with_transport(
        spec(),
        MockServer::serve_duplex_with(Schema::V1, faults),
        options(ProtocolMode::Auto),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("not 2026-07-28"), "{err}");
}

#[tokio::test]
async fn modern_explains_a_server_that_only_knows_the_handshake() {
    let faults = Faults {
        legacy_only: true,
        ..Faults::default()
    };
    let err = connect(ProtocolMode::Modern, faults)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("server/discover"), "{err}");
    assert!(err.contains("Legacy or Auto"), "{err}");

    let faults = Faults {
        legacy_versions: true,
        ..Faults::default()
    };
    let err = connect(ProtocolMode::Modern, faults)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains(
            "supports protocol 2024-11-05, 2025-03-26, 2025-06-18, 2025-11-25, not 2026-07-28"
        ),
        "{err}"
    );
}

#[tokio::test]
async fn legacy_names_the_versions_a_modern_only_server_supports() {
    let faults = Faults {
        modern_only: true,
        ..Faults::default()
    };
    let err = connect(ProtocolMode::Legacy, faults)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains(MODERN_VERSION), "{err}");
    let session = connect(ProtocolMode::Auto, faults).await.unwrap();
    assert_eq!(session.era(), Era::Modern);
}

#[tokio::test]
async fn only_a_legacy_session_pings_a_quiet_server() {
    for (mode, pings) in [(ProtocolMode::Legacy, true), (ProtocolMode::Modern, false)] {
        let sink = EventSink::new(256);
        let mut events = sink.subscribe();
        let options = SessionOptions {
            protocol: mode,
            keepalive: Some(Duration::from_millis(100)),
            sink: Some(sink),
            ..SessionOptions::default()
        };
        let session =
            Session::connect_with_transport(spec(), MockServer::serve_duplex(Schema::V1), options)
                .await
                .unwrap();
        tokio::time::sleep(Duration::from_millis(600)).await;
        let mut pinged = false;
        while let Ok(event) = events.try_recv() {
            if let EventKind::Request { method, .. } = &event.kind
                && method == "ping"
            {
                pinged = true;
            }
        }
        assert_eq!(pinged, pings, "{mode:?}");
        assert_eq!(session.state(), mcp_core::ConnectionState::Connected);
    }
}

fn answer(kind: &ServerRequestKind) -> ServerResponse {
    match kind {
        ServerRequestKind::Elicitation {
            mode: ElicitationMode::Form { .. },
            ..
        } => ServerResponse::Elicitation {
            action: ElicitationAction::Accept,
            content: Some(json!({"answer": "blue"})),
        },
        ServerRequestKind::Elicitation { .. } => ServerResponse::Elicitation {
            action: ElicitationAction::Decline,
            content: None,
        },
        ServerRequestKind::Sampling(_) => ServerResponse::Sampling(json!({
            "role": "assistant",
            "content": {"type": "text", "text": "sampled"},
            "model": "test-model",
            "stopReason": "endTurn",
        })),
        ServerRequestKind::ListRoots => ServerResponse::Roots(vec![Root {
            uri: "file:///work".into(),
            name: None,
        }]),
    }
}

#[tokio::test]
async fn input_requests_are_answered_the_same_in_either_era() {
    for mode in [ProtocolMode::Legacy, ProtocolMode::Modern] {
        let sink = EventSink::new(256);
        answering(&sink, answer);
        let session = connect_with(mode, sink).await;
        let elicited = session
            .call_tool("elicit", json!({"question": "colour?"}))
            .await
            .unwrap();
        assert_eq!(text(elicited), "answer=blue", "{mode:?}");
        let url = session
            .call_tool(
                "elicit",
                json!({"question": "verify?", "url": "https://example.com/verify"}),
            )
            .await
            .unwrap();
        assert_eq!(text(url), "declined", "{mode:?}");
        let sampled = text(
            session
                .call_tool("sample", json!({"prompt": "hi"}))
                .await
                .unwrap(),
        );
        assert!(sampled.contains("sampled"), "{mode:?}: {sampled}");
        let roots = text(session.call_tool("roots", json!({})).await.unwrap());
        assert!(roots.contains("file:///work"), "{mode:?}: {roots}");
    }
}

#[tokio::test]
async fn a_server_that_keeps_asking_hits_the_round_limit() {
    let sink = EventSink::new(256);
    answering(&sink, answer);
    let session = connect_with(ProtocolMode::Modern, sink).await;
    let twice = session
        .call_tool("elicit", json!({"question": "twice?", "repeat": 2}))
        .await
        .unwrap();
    assert_eq!(text(twice), "answer=blue");
    let err = session
        .call_tool("elicit", json!({"question": "again?", "repeat": 50}))
        .await
        .unwrap_err();
    assert!(
        matches!(err, mcp_core::Error::InputRounds(MAX_INPUT_ROUNDS)),
        "{err}"
    );
}

#[tokio::test]
async fn cancelling_while_input_is_asked_for_stops_the_request() {
    let sink = EventSink::new(256);
    let mut events = sink.subscribe();
    let session = connect_with(ProtocolMode::Modern, sink).await;
    let control = RequestControl::new();
    let call = {
        let session = session.clone();
        let control = control.clone();
        tokio::spawn(async move {
            session
                .call_tool_with("elicit", json!({"question": "wait"}), &control)
                .await
        })
    };
    wait_for(&mut events, |kind| {
        matches!(kind, EventKind::ServerRequest(_))
    })
    .await;
    assert!(control.progress_token().is_some());
    control.cancel();
    let err = call.await.unwrap().unwrap_err();
    assert!(matches!(err, mcp_core::Error::Cancelled), "{err}");
    // The session is still usable.
    let echo = session
        .call_tool("echo", json!({"text": "on"}))
        .await
        .unwrap();
    assert_eq!(text(echo), "on");
}

#[tokio::test]
async fn a_modern_session_hears_changes_on_its_stream() {
    let sink = EventSink::new(1024);
    let mut events = sink.subscribe();
    let session = connect_with(ProtocolMode::Modern, sink).await;
    wait_for(&mut events, |kind| {
        matches!(kind, EventKind::Notification { method, .. } if method == "notifications/subscriptions/acknowledged")
    })
    .await;

    session
        .call_tool("add_resource", json!({"name": "notes"}))
        .await
        .unwrap();
    wait_for(&mut events, |kind| {
        matches!(kind, EventKind::ListChanged(ListKind::Resources))
    })
    .await;

    session.subscribe_resource(COUNTER_URI).await.unwrap();
    session.call_tool("bump", json!({})).await.unwrap();
    wait_for(
        &mut events,
        |kind| matches!(kind, EventKind::ResourceUpdated { uri } if uri == COUNTER_URI),
    )
    .await;

    session.unsubscribe_resource(COUNTER_URI).await.unwrap();
    session.call_tool("bump", json!({})).await.unwrap();
    let heard = tokio::time::timeout(
        Duration::from_millis(400),
        wait_for(&mut events, |kind| {
            matches!(kind, EventKind::ResourceUpdated { .. })
        }),
    )
    .await;
    assert!(heard.is_err(), "no update after unsubscribing");
}

#[tokio::test]
async fn a_legacy_session_never_opens_a_stream() {
    let sink = EventSink::new(1024);
    let mut events = sink.subscribe();
    let session = connect_with(ProtocolMode::Legacy, sink).await;
    session.subscribe_resource(COUNTER_URI).await.unwrap();
    session.call_tool("bump", json!({})).await.unwrap();
    wait_for(
        &mut events,
        |kind| matches!(kind, EventKind::ResourceUpdated { uri } if uri == COUNTER_URI),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut listened = false;
    while let Ok(event) = events.try_recv() {
        listened |= matches!(&event.kind, EventKind::Request { method, .. } if method == "subscriptions/listen");
    }
    assert!(!listened);
}

#[tokio::test]
async fn a_modern_session_carries_its_log_level_on_every_request() {
    let sink = EventSink::new(1024);
    let mut events = sink.subscribe();
    let session = connect_with(ProtocolMode::Modern, sink).await;
    session.set_log_level("error").await.unwrap();
    let quiet = session
        .call_tool("log", json!({"level": "info", "message": "quiet"}))
        .await
        .unwrap();
    assert_eq!(text(quiet), "logged=false");
    let loud = session
        .call_tool("log", json!({"level": "error", "message": "loud"}))
        .await
        .unwrap();
    assert_eq!(text(loud), "logged=true");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut logged = Vec::new();
    let mut set_level = false;
    let mut carried = false;
    while let Ok(event) = events.try_recv() {
        match &event.kind {
            EventKind::Log { level, .. } => logged.push(level.clone()),
            EventKind::Request { method, .. } if method == "logging/setLevel" => set_level = true,
            EventKind::Request { method, params, .. } if method == "tools/call" => {
                carried |= params
                    .as_ref()
                    .and_then(|p| p.get("_meta"))
                    .and_then(|m| m.as_object())
                    .is_some_and(|meta| meta.values().any(|v| v == "error"));
            }
            _ => {}
        }
    }
    assert_eq!(logged, ["error"]);
    assert!(!set_level, "2026-07-28 has no logging/setLevel");
    assert!(carried, "the level travels in _meta");
}

#[tokio::test]
async fn a_modern_session_sends_no_roots_notice() {
    for (mode, notified) in [(ProtocolMode::Legacy, true), (ProtocolMode::Modern, false)] {
        let sink = EventSink::new(256);
        let mut events = sink.subscribe();
        let session = connect_with(mode, sink).await;
        session.notify_roots_changed().await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut sent = false;
        while let Ok(event) = events.try_recv() {
            sent |= matches!(&event.kind, EventKind::Notification { method, .. } if method == "notifications/roots/list_changed");
        }
        assert_eq!(sent, notified, "{mode:?}");
    }
}

#[tokio::test]
async fn a_missing_resource_reads_as_not_found_in_either_era() {
    for (mode, code) in [
        (ProtocolMode::Legacy, -32002),
        (ProtocolMode::Modern, -32602),
    ] {
        let session = connect(mode, Faults::default()).await.unwrap();
        let err = session.read_resource("mock://nope").await.unwrap_err();
        assert!(err.is_resource_not_found(), "{mode:?}: {err:?}");
        assert!(
            matches!(err, mcp_core::Error::Server { code: c, .. } if c == code),
            "{mode:?}: {err:?}"
        );
    }
}
