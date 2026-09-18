//! Spawns the real `mcp-mockserver` binary over stdio and drives it through
//! `mcp-core`, exactly like the app and the CLI will.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use mcp_core::{Era, EventKind, ProtocolMode, ServerSpec, Session, SessionOptions};
use serde_json::json;

fn spec(schema: &str) -> ServerSpec {
    ServerSpec::Stdio {
        command: env!("CARGO_BIN_EXE_mcp-mockserver").into(),
        args: vec!["--schema".into(), schema.into()],
        env: Default::default(),
        cwd: None,
    }
}

#[tokio::test]
async fn spawn_snapshot_and_call() {
    let session = Session::connect(spec("v1"), SessionOptions::default())
        .await
        .expect("spawn mock server");
    let snap = session.snapshot().await.unwrap();
    assert_eq!(snap.server_name(), Some(mcp_mockserver::SERVER_NAME));
    assert!(snap.tool("echo").is_some());

    let outcome = session
        .call_tool("echo", json!({"text": "over stdio"}))
        .await
        .unwrap();
    assert_eq!(outcome.content[0]["text"], "over stdio");
    assert!(outcome.elapsed < Duration::from_secs(5));

    let b = Session::connect(spec("v2"), SessionOptions::default())
        .await
        .unwrap();
    let snap_b = b.snapshot().await.unwrap();
    assert!(snap_b.tool("echo").is_none());
    assert!(snap_b.tool("multiply").is_some());
    assert_ne!(snap.digest(), snap_b.digest());
}

#[tokio::test]
async fn the_binary_serves_both_eras() {
    let ServerSpec::Stdio { command, .. } = spec("v1") else {
        unreachable!()
    };
    let with = |flag: Option<&str>| ServerSpec::Stdio {
        command: command.clone(),
        args: ["--schema", "v1"]
            .into_iter()
            .chain(flag)
            .map(str::to_owned)
            .collect(),
        env: Default::default(),
        cwd: None,
    };
    let options = |protocol| SessionOptions {
        protocol,
        ..SessionOptions::default()
    };
    for (flag, mode, era) in [
        (None, ProtocolMode::Modern, Era::Modern),
        (None, ProtocolMode::Legacy, Era::Legacy),
        // Discovery refused before the server sees it, so the handshake that
        // follows on the same process is served.
        (Some("--legacy-only"), ProtocolMode::Auto, Era::Legacy),
        // Discovery answered without 2026-07-28: a second process is spawned
        // for the handshake.
        (Some("--legacy-versions"), ProtocolMode::Auto, Era::Legacy),
        (Some("--modern-only"), ProtocolMode::Auto, Era::Modern),
    ] {
        let session = Session::connect(with(flag), options(mode))
            .await
            .unwrap_or_else(|e| panic!("{flag:?} {mode:?}: {e}"));
        assert_eq!(session.era(), era, "{flag:?} {mode:?}");
        let echo = session
            .call_tool("echo", json!({"text": "either era"}))
            .await
            .unwrap();
        assert_eq!(echo.content[0]["text"], "either era");
    }
    let refused = Session::connect(with(Some("--modern-only")), options(ProtocolMode::Legacy))
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("2026-07-28"), "{refused}");
}

#[tokio::test]
async fn fault_flags_reach_the_lists() {
    let ServerSpec::Stdio { command, .. } = spec("v1") else {
        unreachable!()
    };
    let session = Session::connect(
        ServerSpec::Stdio {
            command,
            args: vec![
                "--schema".into(),
                "v1".into(),
                "--no-templates".into(),
                "--broken-resources".into(),
            ],
            env: Default::default(),
            cwd: None,
        },
        SessionOptions::default(),
    )
    .await
    .unwrap();
    let snap = session.snapshot().await.unwrap();
    assert!(snap.tool("echo").is_some());
    assert!(snap.resources.is_empty());
    assert!(snap.resource_templates.is_empty());
    assert_eq!(snap.list_failures.len(), 1, "{:?}", snap.list_failures);
    assert_eq!(snap.list_failures[0].method, "resources/list");
    assert!(
        snap.list_failures[0].error.contains("-32603"),
        "{}",
        snap.list_failures[0].error
    );

    let echo = session
        .call_tool("echo", json!({"text": "still here"}))
        .await
        .unwrap();
    assert_eq!(echo.content[0]["text"], "still here");
    let read = session.read_resource("mock://text/hello").await.unwrap();
    assert!(
        read.contents[0]["text"]
            .as_str()
            .unwrap()
            .starts_with("Hello")
    );
}

#[tokio::test]
async fn env_var_selects_schema() {
    let ServerSpec::Stdio { command, .. } = spec("v1") else {
        unreachable!()
    };
    let mut env = std::collections::BTreeMap::new();
    env.insert("MCP_MOCK_SCHEMA".to_string(), "v2".to_string());
    let session = Session::connect(
        ServerSpec::Stdio {
            command,
            args: vec![],
            env,
            cwd: None,
        },
        SessionOptions::default(),
    )
    .await
    .unwrap();
    let snap = session.snapshot().await.unwrap();
    assert!(snap.instructions.as_deref().unwrap().contains("schema v2"));
}

#[tokio::test]
async fn schema_flag_accepts_an_equals_sign() {
    let ServerSpec::Stdio { command, .. } = spec("v1") else {
        unreachable!()
    };
    let session = Session::connect(
        ServerSpec::Stdio {
            command,
            args: vec!["--schema=v2".into()],
            env: Default::default(),
            cwd: None,
        },
        SessionOptions::default(),
    )
    .await
    .unwrap();
    let snap = session.snapshot().await.unwrap();
    assert!(snap.instructions.as_deref().unwrap().contains("schema v2"));
}

#[tokio::test]
async fn bad_arguments_fail_initialize_with_failed_state() {
    let ServerSpec::Stdio { command, .. } = spec("v1") else {
        unreachable!()
    };
    let err = Session::connect(
        ServerSpec::Stdio {
            command,
            args: vec!["--bogus".into()],
            env: Default::default(),
            cwd: None,
        },
        SessionOptions::default(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, mcp_core::Error::Initialize(_)), "{err}");
}

#[tokio::test]
async fn missing_command_is_a_transport_error() {
    let err = Session::connect(
        ServerSpec::Stdio {
            command: "/definitely/not/a/real/binary".into(),
            args: vec![],
            env: Default::default(),
            cwd: None,
        },
        SessionOptions::default(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, mcp_core::Error::Transport(_)), "{err}");
}

#[tokio::test]
async fn child_exit_is_reported_as_state_change() {
    let session = Session::connect(spec("v1"), SessionOptions::default())
        .await
        .unwrap();
    let mut events = session.events();
    session.close();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let event = tokio::time::timeout_at(deadline, events.recv())
            .await
            .expect("event")
            .unwrap();
        if let EventKind::StateChange { state, .. } = event.kind
            && state != mcp_core::ConnectionState::Connected
            && state != mcp_core::ConnectionState::Connecting
        {
            break;
        }
    }
}

#[tokio::test]
async fn paged_resources_are_listed_whole() {
    let ServerSpec::Stdio { command, .. } = spec("v1") else {
        unreachable!()
    };
    let paged = Session::connect(
        ServerSpec::Stdio {
            command,
            args: vec!["--paged-resources".into()],
            env: Default::default(),
            cwd: None,
        },
        SessionOptions::default(),
    )
    .await
    .unwrap();
    let whole = Session::connect(spec("v1"), SessionOptions::default())
        .await
        .unwrap();
    let paged = paged.snapshot().await.unwrap();
    let whole = whole.snapshot().await.unwrap();
    assert!(whole.resources.len() > mcp_mockserver::PAGE_SIZE);
    assert_eq!(paged.resources, whole.resources, "every page was followed");
    assert!(paged.list_failures.is_empty(), "{:?}", paged.list_failures);
}

#[tokio::test]
async fn stderr_lines_arrive_and_exit_ends_the_session() {
    let session = Session::connect(spec("v1"), SessionOptions::default())
        .await
        .unwrap();
    let mut events = session.events();
    session
        .call_tool("stderr", json!({"lines": ["first", "second"]}))
        .await
        .unwrap();
    let exit = session
        .call_tool("exit", json!({"code": 3, "delay_ms": 20}))
        .await
        .unwrap();
    assert_eq!(exit.content[0]["text"], "exiting with 3 in 20ms");

    let mut lines = Vec::new();
    let ended =
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match events.recv().await {
                    Ok(event) => match event.kind {
                        EventKind::Stderr { line } => lines.push(line),
                        EventKind::StateChange {
                            state:
                                mcp_core::ConnectionState::Disconnected
                                | mcp_core::ConnectionState::Failed,
                            ..
                        } => return true,
                        _ => {}
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => return false,
                }
            }
        })
        .await;
    assert_eq!(ended, Ok(true), "the session ends with the process");
    assert!(
        lines.iter().any(|l| l == "first") && lines.iter().any(|l| l == "second"),
        "{lines:?}"
    );
}
