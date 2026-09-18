//! Integration tests against the in-process mock server.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use mcp_core::{
    ElicitationAction, ElicitationPolicy, EventCategory, EventKind, ListKind, RootsPolicy,
    SamplingPolicy, ServerRequestKind, ServerRequestPolicy, ServerResponse, ServerSpec, Session,
    SessionOptions, list_method,
};
use mcp_mockserver::{COUNTER_URI, Faults, MockServer, Schema};
use mcp_schema_form::FormModel;
use serde_json::{Value, json};

fn spec() -> ServerSpec {
    ServerSpec::Stdio {
        command: "in-process".into(),
        args: vec![],
        env: Default::default(),
        cwd: None,
    }
}

async fn connect(schema: Schema, options: SessionOptions) -> Session {
    let client_side = MockServer::serve_duplex(schema);
    Session::connect_with_transport(spec(), client_side, options)
        .await
        .expect("connect")
}

#[tokio::test]
async fn snapshot_lists_everything() {
    let session = connect(Schema::V1, SessionOptions::default()).await;
    let snap = session.snapshot().await.unwrap();
    assert_eq!(snap.server_name(), Some("mcp-mockserver"));
    assert!(snap.has_capability("tools"));
    assert!(snap.has_capability("resources"));
    assert!(snap.has_capability("prompts"));
    assert!(snap.instructions.as_deref().unwrap().contains("schema v1"));

    let tool_names: Vec<&str> = snap.tools.iter().map(|t| t.name.as_str()).collect();
    for expected in [
        "echo", "add", "fail", "sleep", "complex", "bump", "log", "elicit", "sample", "roots",
        "rows",
    ] {
        assert!(
            tool_names.contains(&expected),
            "missing {expected}: {tool_names:?}"
        );
    }
    let add = snap.tool("add").unwrap();
    assert_eq!(add.input_schema["properties"]["a"]["type"], "number");
    assert!(
        add.output_schema.is_some(),
        "add declares structured output"
    );

    assert_eq!(snap.resources.len(), 5);
    assert_eq!(snap.resource_templates.len(), 1);
    assert_eq!(snap.resource_templates[0].uri_template, "mock://item/{id}");
    assert_eq!(snap.prompts.len(), 2);
    let summarize = snap.prompt("summarize").unwrap();
    assert!(
        summarize
            .arguments
            .iter()
            .any(|a| a.name == "text" && a.required == Some(true))
    );
    assert!(
        summarize
            .arguments
            .iter()
            .any(|a| a.name == "style" && a.required != Some(true))
    );

    assert_eq!(snap.digest().len(), 64);
    let again = session.snapshot().await.unwrap();
    assert_eq!(
        snap.digest(),
        again.digest(),
        "digest is stable across calls"
    );
}

#[tokio::test]
async fn schemas_differ() {
    let a = connect(Schema::V1, SessionOptions::default())
        .await
        .snapshot()
        .await
        .unwrap();
    let b = connect(Schema::V2, SessionOptions::default())
        .await
        .snapshot()
        .await
        .unwrap();
    assert_ne!(a.digest(), b.digest());
    assert!(a.tool("echo").is_some() && b.tool("echo").is_none());
    assert!(a.tool("multiply").is_none() && b.tool("multiply").is_some());
    let required = |s: &mcp_core::Snapshot| -> Vec<String> {
        s.tool("add").unwrap().input_schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect()
    };
    assert!(!required(&a).contains(&"precision".to_string()));
    assert!(required(&b).contains(&"precision".to_string()));
}

/// A server whose resources are broken and which never implemented templates
/// still gives a snapshot, and its tools still work.
#[tokio::test]
async fn a_failing_list_does_not_fail_the_snapshot() {
    let faults = Faults {
        no_templates: true,
        broken_resources: true,
        ..Faults::default()
    };
    let session = Session::connect_with_transport(
        spec(),
        MockServer::serve_duplex_with(Schema::V1, faults),
        SessionOptions::default(),
    )
    .await
    .expect("connect");
    let snap = session.snapshot().await.unwrap();
    for expected in ["echo", "add", "fail", "sleep", "bump"] {
        assert!(snap.tool(expected).is_some(), "missing {expected}");
    }
    assert_eq!(snap.prompts.len(), 2);
    assert!(snap.resources.is_empty());
    assert!(snap.resource_templates.is_empty());
    // Method-not-found is the server's answer, not a failure.
    assert_eq!(snap.list_failures.len(), 1, "{:?}", snap.list_failures);
    assert_eq!(snap.list_failures[0].method, "resources/list");
    assert!(
        snap.list_failures[0].error.contains("-32603"),
        "{}",
        snap.list_failures[0].error
    );
    assert!(snap.list_failed("resources/list"));
    assert!(!snap.list_failed("resources/templates/list"));

    let echo = session
        .call_tool("echo", json!({"text": "hi"}))
        .await
        .unwrap();
    assert_eq!(echo.content[0]["text"], "hi");
    let text = session.read_resource("mock://text/hello").await.unwrap();
    assert_eq!(text.contents[0]["mimeType"], "text/plain");

    // The digest is about what the server advertised: the same as a healthy
    // server's with those lists empty.
    let mut healthy = connect(Schema::V1, SessionOptions::default())
        .await
        .snapshot()
        .await
        .unwrap();
    assert!(healthy.list_failures.is_empty());
    healthy.resources.clear();
    healthy.resource_templates.clear();
    assert_eq!(snap.digest(), healthy.digest());
}

/// A list that never answers costs the snapshot one timeout, not one per
/// list: the lists after it are not requested, and the ones before it stay.
#[tokio::test]
async fn a_list_that_times_out_is_the_last_one_requested() {
    let timeout = Duration::from_secs(1);
    let faults = Faults {
        stalled_resources: true,
        ..Faults::default()
    };
    let session = Session::connect_with_transport(
        spec(),
        MockServer::serve_duplex_with(Schema::V1, faults),
        SessionOptions {
            request_timeout: Some(timeout),
            ..SessionOptions::default()
        },
    )
    .await
    .expect("connect");
    let started = std::time::Instant::now();
    let snap = session.snapshot().await.unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed >= timeout,
        "resources/list was waited for: {elapsed:?}"
    );
    assert!(
        elapsed < timeout * 2,
        "one timeout, not one per list: {elapsed:?}"
    );

    assert!(snap.tool("echo").is_some(), "the list before it is kept");
    let failed: Vec<&str> = snap
        .list_failures
        .iter()
        .map(|f| f.method.as_str())
        .collect();
    assert_eq!(
        failed,
        [
            list_method::RESOURCES,
            list_method::RESOURCE_TEMPLATES,
            list_method::PROMPTS
        ]
    );
    assert!(
        snap.list_failures[0].error.contains("timed out"),
        "{}",
        snap.list_failures[0].error
    );
    for skipped in &snap.list_failures[1..] {
        assert!(
            skipped.error.starts_with("not requested"),
            "{}",
            skipped.error
        );
    }
    assert!(snap.resource_templates.is_empty() && snap.prompts.is_empty());
}

#[tokio::test]
async fn call_every_tool_with_generated_defaults() {
    let session = connect(Schema::V1, SessionOptions::default()).await;
    let snap = session.snapshot().await.unwrap();
    for tool in &snap.tools {
        let model = FormModel::from_schema(&tool.input_schema);
        let args = model.default_json();
        assert!(
            mcp_schema_form::validate(&tool.input_schema, &args).is_empty(),
            "{}: generated args {args} do not validate",
            tool.name
        );
        let outcome = session
            .call_tool(&tool.name, args.clone())
            .await
            .unwrap_or_else(|e| panic!("{}({args}): {e}", tool.name));
        // `exit` refuses to end a server sharing the test's process.
        assert_eq!(
            outcome.is_error,
            tool.name == "fail" || tool.name == "exit",
            "{}: {:?}",
            tool.name,
            outcome.raw
        );
        assert!(
            !outcome.content.is_empty() || outcome.raw.get("structuredContent").is_some(),
            "{}",
            tool.name
        );
        if tool.name == "add" {
            assert_eq!(outcome.raw["structuredContent"], json!({"value": 0.0}));
        }
    }
}

#[tokio::test]
async fn call_results_and_errors() {
    let session = connect(Schema::V1, SessionOptions::default()).await;
    let echo = session
        .call_tool("echo", json!({"text": "hi"}))
        .await
        .unwrap();
    assert_eq!(echo.content[0]["text"], "hi");
    assert!(!echo.is_error);

    let add = session
        .call_tool("add", json!({"a": 1.5, "b": 2}))
        .await
        .unwrap();
    assert_eq!(add.raw["structuredContent"], json!({"value": 3.5}));

    let fail = session
        .call_tool("fail", json!({"message": "boom"}))
        .await
        .unwrap();
    assert!(fail.is_error);
    assert_eq!(fail.content[0]["text"], "boom");

    let missing = session.call_tool("nope", json!({})).await.unwrap_err();
    assert!(
        matches!(missing, mcp_core::Error::Server { .. }),
        "{missing}"
    );

    let bad_args = session.call_tool("echo", json!([1])).await.unwrap_err();
    assert!(matches!(bad_args, mcp_core::Error::InvalidArguments(_)));

    // rmcp's router reports argument deserialization failures as `isError` results.
    let invalid = session.call_tool("add", json!({"a": "x"})).await.unwrap();
    assert!(invalid.is_error);
    assert!(
        invalid.content[0]["text"]
            .as_str()
            .unwrap()
            .contains("deserialize")
    );
}

#[tokio::test]
async fn resources_and_prompts() {
    let session = connect(Schema::V1, SessionOptions::default()).await;
    let text = session.read_resource("mock://text/hello").await.unwrap();
    assert_eq!(text.contents[0]["mimeType"], "text/plain");
    assert!(
        text.contents[0]["text"]
            .as_str()
            .unwrap()
            .starts_with("Hello")
    );

    let png = session.read_resource("mock://png/pixel").await.unwrap();
    assert_eq!(png.contents[0]["mimeType"], "image/png");
    assert!(png.contents[0]["blob"].is_string());

    let item = session.read_resource("mock://item/42").await.unwrap();
    assert_eq!(item.contents[0]["text"], "item 42");

    let err = session.read_resource("mock://missing").await.unwrap_err();
    assert!(
        matches!(err, mcp_core::Error::Server { code: -32002, .. }),
        "{err}"
    );

    let mut args = serde_json::Map::new();
    args.insert("name".into(), Value::String("Ada".into()));
    let prompt = session.get_prompt("greet", args).await.unwrap();
    assert_eq!(prompt.raw["messages"].as_array().map(Vec::len), Some(1));
    assert!(
        prompt.raw["messages"][0]["content"]["text"]
            .as_str()
            .unwrap()
            .contains("Ada")
    );
}

#[tokio::test]
async fn subscriptions_and_notifications_reach_the_event_log() {
    let session = connect(Schema::V1, SessionOptions::default()).await;
    let mut events = session.events();
    session.subscribe_resource(COUNTER_URI).await.unwrap();
    session.call_tool("bump", json!({})).await.unwrap();
    session
        .call_tool("log", json!({"level": "warning", "message": "careful"}))
        .await
        .unwrap();

    let mut saw_update = false;
    let mut saw_log = false;
    let mut saw_wire_notification = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !(saw_update && saw_log && saw_wire_notification) {
        let event = tokio::time::timeout_at(deadline, events.recv())
            .await
            .expect("timed out waiting for events")
            .unwrap();
        match &event.kind {
            EventKind::ResourceUpdated { uri } if uri == COUNTER_URI => saw_update = true,
            EventKind::Log { level, data, .. } if level == "warning" => {
                assert_eq!(data, "careful");
                saw_log = true;
            }
            EventKind::Notification { method, .. }
                if method == "notifications/resources/updated" =>
            {
                saw_wire_notification = true;
            }
            _ => {}
        }
    }

    let counter = session.read_resource(COUNTER_URI).await.unwrap();
    assert_eq!(counter.contents[0]["text"], "1");
    session.unsubscribe_resource(COUNTER_URI).await.unwrap();
}

#[tokio::test]
async fn wire_events_carry_timing_and_state() {
    let session = connect(Schema::V1, SessionOptions::default()).await;
    let mut events = session.events();
    session
        .call_tool("echo", json!({"text": "t"}))
        .await
        .unwrap();
    let mut request_seen = false;
    let mut response_seen = false;
    while let Ok(Ok(event)) = tokio::time::timeout(Duration::from_millis(500), events.recv()).await
    {
        match event.kind {
            EventKind::Request { method, .. } if method == "tools/call" => request_seen = true,
            EventKind::Response {
                method, elapsed, ..
            } if method.as_deref() == Some("tools/call") => {
                assert!(elapsed.is_some());
                response_seen = true;
            }
            _ => {}
        }
        if request_seen && response_seen {
            break;
        }
    }
    assert!(request_seen && response_seen);
    assert_eq!(session.state(), mcp_core::ConnectionState::Connected);

    let mut watch = session.state_watch();
    session.close();
    tokio::time::timeout(Duration::from_secs(5), async {
        while *watch.borrow() == mcp_core::ConnectionState::Connected {
            watch.changed().await.unwrap();
        }
    })
    .await
    .expect("state change after close");
    assert_ne!(session.state(), mcp_core::ConnectionState::Connected);
}

/// Call the three tools that send server requests and return their texts:
/// elicitation, sampling, roots.
async fn server_request_texts(session: &Session) -> [String; 3] {
    let text =
        |outcome: mcp_core::CallOutcome| outcome.content[0]["text"].as_str().unwrap().to_owned();
    let elicit = session
        .call_tool("elicit", json!({"question": "?"}))
        .await
        .unwrap();
    let sample = session
        .call_tool("sample", json!({"prompt": "p"}))
        .await
        .unwrap();
    let roots = session.call_tool("roots", json!({})).await.unwrap();
    [text(elicit), text(sample), text(roots)]
}

#[tokio::test]
async fn a_refusing_policy_answers_without_asking() {
    let options = SessionOptions {
        policy: ServerRequestPolicy {
            sampling: SamplingPolicy::Reject,
            elicitation: ElicitationPolicy::Decline,
            roots: RootsPolicy::Fixed { roots: Vec::new() },
            ..ServerRequestPolicy::default()
        },
        ..SessionOptions::default()
    };
    let session = connect(Schema::V1, options).await;
    // A listener that never answers: the refusing policy does not ask it.
    let _events = session.events();
    let [elicit, sample, roots] =
        tokio::time::timeout(Duration::from_secs(10), server_request_texts(&session))
            .await
            .expect("a refusing policy never waits");
    assert_eq!(elicit, "declined");
    assert!(sample.contains("sampling error"), "{sample}");
    assert!(sample.contains("rejected by client policy"), "{sample}");
    assert_eq!(roots, r#"{"roots":[]}"#);
}

#[tokio::test]
async fn the_default_policy_falls_back_when_nobody_listens() {
    let session = connect(Schema::V1, SessionOptions::default()).await;
    let [elicit, sample, roots] =
        tokio::time::timeout(Duration::from_secs(10), server_request_texts(&session))
            .await
            .expect("with no listener the default does not wait for an answer");
    assert_eq!(elicit, "declined");
    assert!(sample.contains("sampling error"), "{sample}");
    assert!(sample.contains("unanswered"), "{sample}");
    assert_eq!(roots, r#"{"roots":[]}"#);
}

#[tokio::test]
async fn server_requests_can_be_answered_by_a_listener() {
    let options = SessionOptions {
        policy: ServerRequestPolicy {
            sampling: SamplingPolicy::Prompt,
            elicitation: ElicitationPolicy::Prompt,
            roots: RootsPolicy::Prompt,
            prompt_timeout: Duration::from_secs(5),
        },
        ..SessionOptions::default()
    };
    let session = connect(Schema::V1, options).await;
    let mut events = session.events();
    let answerer = tokio::spawn(async move {
        let mut answered = 0;
        while answered < 3 {
            let Ok(event) = events.recv().await else {
                break;
            };
            assert_eq!(
                matches!(event.kind, EventKind::ServerRequest(_)),
                event.kind.category() == EventCategory::ServerRequest
            );
            if let EventKind::ServerRequest(req) = event.kind {
                let response = match &req.kind {
                    ServerRequestKind::Elicitation { message, mode } => {
                        assert_eq!(message, "favourite colour?");
                        assert!(
                            matches!(mode, mcp_core::ElicitationMode::Form { schema } if schema["properties"]["answer"].is_object())
                        );
                        ServerResponse::Elicitation {
                            action: ElicitationAction::Accept,
                            content: Some(json!({"answer": "teal"})),
                        }
                    }
                    ServerRequestKind::Sampling(params) => {
                        assert_eq!(params["messages"][0]["content"]["text"], "p");
                        ServerResponse::Sampling(json!({
                            "model": "test-model",
                            "role": "assistant",
                            "content": {"type": "text", "text": "hello"},
                            "stopReason": "endTurn"
                        }))
                    }
                    ServerRequestKind::ListRoots => ServerResponse::Roots(vec![mcp_core::Root {
                        uri: "file:///work".into(),
                        name: Some("work".into()),
                    }]),
                };
                assert!(req.respond(response));
                answered += 1;
            }
        }
        answered
    });

    let elicit = session
        .call_tool("elicit", json!({"question": "favourite colour?"}))
        .await
        .unwrap();
    assert_eq!(elicit.content[0]["text"], "answer=teal");
    let sample = session
        .call_tool("sample", json!({"prompt": "p"}))
        .await
        .unwrap();
    let result: Value = serde_json::from_str(sample.content[0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(result["model"], "test-model");
    assert_eq!(result["content"]["text"], "hello");
    let roots = session.call_tool("roots", json!({})).await.unwrap();
    assert!(
        roots.content[0]["text"]
            .as_str()
            .unwrap()
            .contains("file:///work")
    );
    assert_eq!(answerer.await.unwrap(), 3);
}

#[tokio::test]
async fn auto_sampling_policy_answers_without_listener() {
    let options = SessionOptions {
        policy: ServerRequestPolicy {
            sampling: SamplingPolicy::Auto {
                model: "canned".into(),
                text: "canned reply".into(),
            },
            ..ServerRequestPolicy::default()
        },
        ..SessionOptions::default()
    };
    let session = connect(Schema::V1, options).await;
    let sample = session
        .call_tool("sample", json!({"prompt": "p"}))
        .await
        .unwrap();
    let result: Value = serde_json::from_str(sample.content[0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(result["model"], "canned");
    assert_eq!(result["content"]["text"], "canned reply");
}

#[tokio::test]
async fn request_timeout_is_enforced() {
    let options = SessionOptions {
        request_timeout: Some(Duration::from_millis(100)),
        ..SessionOptions::default()
    };
    let session = connect(Schema::V1, options).await;
    let err = session
        .call_tool("sleep", json!({"millis": 2000}))
        .await
        .unwrap_err();
    assert!(matches!(err, mcp_core::Error::Timeout(_)), "{err}");
}

/// Events from `rx` until `done` says stop or `limit` passes.
async fn collect_until(
    rx: &mut tokio::sync::broadcast::Receiver<mcp_core::Event>,
    limit: Duration,
    mut done: impl FnMut(&EventKind) -> bool,
) -> Vec<EventKind> {
    let mut seen = Vec::new();
    let _ = tokio::time::timeout(limit, async {
        while let Ok(event) = rx.recv().await {
            let stop = done(&event.kind);
            seen.push(event.kind);
            if stop {
                break;
            }
        }
    })
    .await;
    seen
}

fn sent_cancellation(events: &[EventKind]) -> Option<Value> {
    events.iter().find_map(|kind| match kind {
        EventKind::Notification {
            direction: mcp_core::Direction::Outbound,
            method,
            params,
        } if method == "notifications/cancelled" => params.clone(),
        _ => None,
    })
}

#[tokio::test]
async fn a_cancelled_call_stops_and_tells_the_server() {
    let session = connect(Schema::V1, SessionOptions::default()).await;
    let mut rx = session.events();
    let control = mcp_core::RequestControl::new();
    let call = {
        let session = session.clone();
        let control = control.clone();
        tokio::spawn(async move {
            session
                .call_tool_with("sleep", json!({"millis": 5000}), &control)
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(control.progress_token().is_some(), "sent with a token");
    let started = std::time::Instant::now();
    control.cancel();
    let result = tokio::time::timeout(Duration::from_secs(2), call)
        .await
        .expect("cancel ends the call at once")
        .unwrap();
    assert!(
        matches!(result, Err(mcp_core::Error::Cancelled)),
        "{result:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    let events = collect_until(&mut rx, Duration::from_secs(2), |kind| {
        sent_cancellation(std::slice::from_ref(kind)).is_some()
    })
    .await;
    let params = sent_cancellation(&events).expect("notifications/cancelled sent");
    assert!(params.get("requestId").is_some(), "{params}");

    // The session is still usable afterwards.
    let echo = session
        .call_tool("echo", json!({"text": "after"}))
        .await
        .unwrap();
    assert_eq!(echo.content[0]["text"], "after");
}

#[tokio::test]
async fn a_call_past_the_timeout_tells_the_server() {
    let options = SessionOptions {
        request_timeout: Some(Duration::from_millis(200)),
        ..SessionOptions::default()
    };
    let session = connect(Schema::V1, options).await;
    let mut rx = session.events();
    let result = session.call_tool("sleep", json!({"millis": 3000})).await;
    assert!(
        matches!(result, Err(mcp_core::Error::Timeout(_))),
        "{result:?}"
    );
    let events = collect_until(&mut rx, Duration::from_secs(2), |kind| {
        sent_cancellation(std::slice::from_ref(kind)).is_some()
    })
    .await;
    let params = sent_cancellation(&events).expect("notifications/cancelled sent");
    assert_eq!(params["reason"], "request timed out");
}

#[tokio::test]
async fn progress_names_the_call_it_belongs_to() {
    let session = connect(Schema::V1, SessionOptions::default()).await;
    let mut rx = session.events();
    let control = mcp_core::RequestControl::new();
    let out = session
        .call_tool_with("progress", json!({"steps": 3, "delay_ms": 10}), &control)
        .await
        .unwrap();
    assert_eq!(out.content[0]["text"], "done 3 steps");
    let token = control.progress_token().unwrap();
    let events = collect_until(
        &mut rx,
        Duration::from_secs(2),
        |kind| matches!(kind, EventKind::Progress { progress, .. } if *progress >= 3.0),
    )
    .await;
    let progress: Vec<(Value, f64, Option<f64>, Option<String>)> = events
        .into_iter()
        .filter_map(|kind| match kind {
            EventKind::Progress {
                token,
                progress,
                total,
                message,
            } => Some((token, progress, total, message)),
            _ => None,
        })
        .collect();
    assert_eq!(progress.len(), 3, "{progress:?}");
    assert!(
        progress
            .iter()
            .all(|(t, _, total, _)| *t == token && *total == Some(3.0))
    );
    assert_eq!(progress[2].3.as_deref(), Some("step 3 of 3"));
}

#[tokio::test]
async fn the_log_level_is_set_on_the_server() {
    let session = connect(Schema::V1, SessionOptions::default()).await;
    session.set_log_level("error").await.unwrap();
    let quiet = session
        .call_tool("log", json!({"level": "info", "message": "hidden"}))
        .await
        .unwrap();
    assert_eq!(quiet.content[0]["text"], "logged=false");
    let loud = session
        .call_tool("log", json!({"level": "error", "message": "shown"}))
        .await
        .unwrap();
    assert_eq!(loud.content[0]["text"], "logged=true");
    assert!(matches!(
        session.set_log_level("chatty").await,
        Err(mcp_core::Error::InvalidArguments(_))
    ));
    assert_eq!(mcp_core::LOG_LEVELS[0], "debug");
}

#[tokio::test]
async fn completions_suggest_prompt_arguments_and_template_variables() {
    let session = connect(Schema::V1, SessionOptions::default()).await;
    let snap = session.snapshot().await.unwrap();
    assert!(snap.has_capability("completions"));
    let none = std::collections::BTreeMap::new();
    let names = session
        .complete(
            &mcp_core::CompletionTarget::Prompt("greet".into()),
            "name",
            "g",
            &none,
        )
        .await
        .unwrap();
    assert_eq!(names.values, ["Grace", "Guido"]);
    assert!(!names.has_more);
    let filled = std::collections::BTreeMap::from([("text".to_owned(), "a b".to_owned())]);
    let styles = session
        .complete(
            &mcp_core::CompletionTarget::Prompt("summarize".into()),
            "style",
            "",
            &filled,
        )
        .await
        .unwrap();
    assert_eq!(styles.values, ["brief", "detailed"]);
    let ids = session
        .complete(
            &mcp_core::CompletionTarget::ResourceTemplate("mock://item/{id}".into()),
            "id",
            "4",
            &none,
        )
        .await
        .unwrap();
    assert_eq!(ids.values, ["42"]);
}

#[tokio::test]
async fn a_changed_list_is_read_again_in_place() {
    let session = connect(Schema::V1, SessionOptions::default()).await;
    let mut rx = session.events();
    let before = session.snapshot().await.unwrap();
    session
        .call_tool("add_resource", json!({"name": "notes"}))
        .await
        .unwrap();
    let events = collect_until(&mut rx, Duration::from_secs(2), |kind| {
        matches!(kind, EventKind::ListChanged(ListKind::Resources))
    })
    .await;
    assert!(
        events
            .iter()
            .any(|k| matches!(k, EventKind::ListChanged(ListKind::Resources))),
        "the server announced it"
    );
    let after = session.relist(&before, ListKind::Resources).await.unwrap();
    assert_eq!(after.resources.len(), before.resources.len() + 1);
    assert!(after.resource("mock://extra/notes").is_some());
    assert_eq!(after.tools, before.tools, "other lists are kept");
    assert_eq!(after.prompts, before.prompts);
    assert_eq!(after.server_info, before.server_info);
    let read = session.read_resource("mock://extra/notes").await.unwrap();
    assert_eq!(read.contents[0]["text"], "extra notes");
}

#[tokio::test]
async fn keepalive_fails_a_session_that_stopped_answering() {
    let faults = Faults {
        ignore_pings: true,
        ..Faults::default()
    };
    let options = SessionOptions {
        keepalive: Some(Duration::from_millis(100)),
        ..SessionOptions::default()
    };
    let session = Session::connect_with_transport(
        spec(),
        MockServer::serve_duplex_with(Schema::V1, faults),
        options,
    )
    .await
    .unwrap();
    let mut rx = session.events();
    let mut state = session.state_watch();
    tokio::time::timeout(
        Duration::from_secs(3),
        state.wait_for(|s| *s == mcp_core::ConnectionState::Failed),
    )
    .await
    .expect("the session fails")
    .unwrap();
    let events = collect_until(&mut rx, Duration::from_millis(500), |kind| {
        matches!(kind, EventKind::StateChange { .. })
    })
    .await;
    let detail = events.iter().find_map(|kind| match kind {
        EventKind::StateChange { detail, .. } => detail.clone(),
        _ => None,
    });
    assert!(detail.unwrap_or_default().contains("ping"));
    assert_eq!(session.state(), mcp_core::ConnectionState::Failed);
}

#[tokio::test]
async fn keepalive_leaves_a_live_session_connected() {
    let options = SessionOptions {
        keepalive: Some(Duration::from_millis(50)),
        ..SessionOptions::default()
    };
    let session = connect(Schema::V1, options).await;
    let mut rx = session.events();
    let events = collect_until(&mut rx, Duration::from_millis(600), |_| false).await;
    assert!(
        events.iter().any(|kind| matches!(
            kind,
            EventKind::Request { method, .. } if method == "ping"
        )),
        "a quiet server is pinged"
    );
    assert_eq!(session.state(), mcp_core::ConnectionState::Connected);
    // A slow call keeps the server busy with our request: no ping fails it.
    let slow = session
        .call_tool("sleep", json!({"millis": 300}))
        .await
        .unwrap();
    assert_eq!(slow.content[0]["text"], "slept 300ms");
    assert_eq!(session.state(), mcp_core::ConnectionState::Connected);
}

#[tokio::test]
async fn a_request_the_server_withdraws_is_announced() {
    let session = connect(Schema::V1, SessionOptions::default()).await;
    let mut rx = session.events();
    let call = {
        let session = session.clone();
        tokio::spawn(async move {
            session
                .call_tool(
                    "elicit",
                    json!({"question": "still there?", "timeout_ms": 200}),
                )
                .await
        })
    };
    let events = collect_until(&mut rx, Duration::from_secs(3), |kind| {
        matches!(kind, EventKind::RequestCancelled { .. })
    })
    .await;
    let asked = events.iter().find_map(|kind| match kind {
        EventKind::ServerRequest(request) => Some(request.id.clone()),
        _ => None,
    });
    let withdrawn = events.iter().find_map(|kind| match kind {
        EventKind::RequestCancelled { id, .. } => Some(id.clone()),
        _ => None,
    });
    assert!(asked.is_some(), "the server asked: {events:?}");
    assert_eq!(withdrawn, asked, "and withdrew that same request");
    let out = tokio::time::timeout(Duration::from_secs(2), call)
        .await
        .expect("the tool gives up")
        .unwrap()
        .unwrap();
    assert_eq!(out.content[0]["text"], "withdrawn");
}
