//! Headless flows for the protocol eras: a server reached with 2026-07-28,
//! one that only knows the handshake reached through Auto, the server form's
//! Protocol tabs, and a control that cannot run saying why. Each drives the
//! real UI, against the mock server binary where a server is live.

use coco_mcp::calls::ResponseStatus;
use coco_mcp::features::Feature;
use coco_mcp::state::{AppState, Mode, Screen, Status};
use gpui_kit::test::TestWindowExt as _;
use mcp_core::{Era, LEGACY_VERSION, MODERN_VERSION, ProtocolMode, ServerSpec};
use serde_json::json;

use crate::macos::{mock_binary, snap, wait_for_request};
use crate::protocol::{Live, live_in, open};

/// The version the selected server's last session agreed on.
fn agreed(live: &mut Live) -> String {
    live.cx.update(|cx| {
        live.state
            .read(cx)
            .snapshot()
            .map(|s| s.protocol_version.clone())
            .unwrap_or_default()
    })
}

/// A 2026-07-28 session: the Server view names the era, a request that needs
/// input opens the usual dialog, changes arrive on the stream, the log level
/// travels with each request, and saved roots send no notice.
pub fn modern_session_flow() {
    let mut live = live_in(ProtocolMode::Modern, &["--schema", "v1"], None);
    assert_eq!(agreed(&mut live), MODERN_VERSION);
    live.cx.update(|cx| {
        let session = live.state.read(cx).servers[0].session.clone();
        assert_eq!(session.map(|s| s.era()), Some(Era::Modern));
    });
    assert!(live.logged("server/discover"));
    assert!(!live.logged("initialize"), "no handshake");
    live.show(Mode::Server);
    snap(&mut live.cx, live.handle, "56-modern-server-view");

    // A request that needs input opens the dialog a legacy server's own
    // request does, noting what the revision deprecates.
    live.call("sample", json!({"prompt": "Say hi"}));
    wait_for_request(&mut live.cx, live.handle, &live.state);
    live.ui(|window, _| {
        assert!(
            window.try_find("request-deprecated").is_some(),
            "sampling is deprecated in 2026-07-28"
        );
    });
    snap(&mut live.cx, live.handle, "57-modern-sampling-dialog");
    live.ui(|window, cx| {
        window.click("req-text", cx);
        window.input("Hello inside a call", cx);
        window.click("req-accept", cx);
    });
    let sampled = live.text();
    assert!(sampled.contains("Hello inside a call"), "{sampled}");

    // Changes arrive on the subscription stream the session keeps open.
    live.call("add_resource", json!({"name": "notes"}));
    assert_eq!(live.answered().0, ResponseStatus::Ok);
    live.wait("the new resource is listed", |s| {
        s.snapshot()
            .is_some_and(|snap| snap.resource("mock://extra/notes").is_some())
    });
    live.select(Mode::Resources, "mock://counter");
    live.ui(|window, cx| window.click("subscribe", cx));
    live.wait("the subscription is taken", |s| {
        s.servers[0].is_subscribed("mock://counter")
    });
    live.ui(|window, cx| window.click("call", cx));
    assert_eq!(live.text(), "0");
    live.call("bump", json!({}));
    assert_eq!(live.answered().0, ResponseStatus::Ok);
    live.select(Mode::Resources, "mock://counter");
    live.wait("the change is read again", |s| {
        s.response()
            .is_some_and(|r| r.raw["contents"][0]["text"] == "1")
    });
    assert!(live.logged("subscriptions/listen"));
    assert!(!live.logged("resources/subscribe"));

    // The level travels with each request; nothing sets it on the server.
    live.ui(|window, cx| window.click("log-header", cx));
    live.ui(|window, _| {
        assert!(window.try_find("log-level-deprecated").is_some());
    });
    live.cx.update(|cx| {
        live.state
            .update(cx, |s, cx| s.choose_log_level(Some("error"), cx))
    });
    live.wait("the level is kept", |s| {
        s.servers[0].log_level.as_deref() == Some("error")
    });
    live.call("log", json!({"level": "info", "message": "quiet"}));
    assert_eq!(live.text(), "logged=false");
    live.call("log", json!({"level": "error", "message": "disk full"}));
    assert_eq!(live.text(), "logged=true");
    assert!(!live.logged("logging/setLevel"));
    snap(&mut live.cx, live.handle, "58-modern-log-level");
    live.ui(|window, cx| window.click("log-header", cx));

    // Roots are kept and reach the server when it asks; no notice goes out.
    live.select(Mode::Server, "Roots");
    live.ui(|window, _| {
        assert!(window.try_find("roots-deprecated").is_some());
    });
    live.ui(|window, cx| {
        window.click("server-roots", cx);
        window.input("file:///tmp/modern", cx);
        window.click("save-roots", cx);
    });
    live.wait("the roots are kept", |s| {
        s.servers[0]
            .roots
            .as_ref()
            .is_some_and(|roots| roots.iter().any(|r| r.uri == "file:///tmp/modern"))
    });
    live.call("roots", json!({}));
    wait_for_request(&mut live.cx, live.handle, &live.state);
    live.ui(|window, cx| window.click("req-accept", cx));
    assert!(live.text().contains("file:///tmp/modern"));
    assert!(!live.logged("notifications/roots/list_changed"));
}

/// Auto on a server that only knows the handshake: discovery is tried, the
/// handshake follows, and the session is a legacy one.
pub fn auto_fallback_flow() {
    let mut live = live_in(
        ProtocolMode::Auto,
        &["--schema", "v1", "--legacy-only"],
        None,
    );
    assert_eq!(agreed(&mut live), LEGACY_VERSION);
    assert!(live.logged("server/discover"), "discovery is tried first");
    assert!(live.logged("initialize"), "then the handshake");
    live.call("echo", json!({"text": "after the handshake"}));
    assert_eq!(live.text(), "after the handshake");
}

/// The server form's Protocol tabs choose the era a server is saved and
/// connected in, and editing starts from the saved one.
pub fn protocol_tabs_flow() {
    let mock = mock_binary();
    let bridge = coco_mcp::bridge::Bridge::new().unwrap();
    let store = mcp_store::Store::open_in_memory().unwrap();
    let model = AppState::new(Some(bridge), Some(store.clone()));
    let mut live = open(model, store);
    live.ui(|window, cx| window.click("add-server", cx));
    live.ui(|window, cx| {
        window.click("name", cx);
        window.input("modern", cx);
        window.click("command", cx);
        window.input(&format!("{} --schema v1", mock.display()), cx);
        window.click("protocol-modern", cx);
    });
    snap(&mut live.cx, live.handle, "59-protocol-tabs");
    live.ui(|window, cx| window.click("connect", cx));
    live.wait("the server connects in the chosen era", |s| {
        s.servers
            .first()
            .is_some_and(|server| server.status == Status::Connected)
    });
    assert_eq!(agreed(&mut live), MODERN_VERSION);
    assert_eq!(
        live.store.list_servers().unwrap()[0].protocol,
        ProtocolMode::Modern
    );

    // The Server view's Settings row opens the same form, prefilled.
    live.select(Mode::Server, "Settings");
    live.cx.update(|cx| {
        assert_eq!(live.state.read(cx).screen, Screen::AddServer);
    });
    live.ui(|window, cx| window.click("protocol-legacy", cx));
    live.ui(|window, cx| window.click("connect", cx));
    live.wait("it reconnects with the handshake", |s| {
        s.servers[0].status == Status::Connected
            && s.snapshot()
                .is_some_and(|snap| snap.protocol_version == LEGACY_VERSION)
    });
    assert_eq!(
        live.store.list_servers().unwrap()[0].protocol,
        ProtocolMode::Legacy
    );
}

/// A control that cannot run for the selected server stays on screen, dimmed,
/// and pressing it puts the reason in the status bar.
pub fn disabled_control_flow() {
    let mut model = AppState::new(None, None);
    let spec = ServerSpec::Stdio {
        command: "quiet".into(),
        args: vec![],
        env: Default::default(),
        cwd: None,
    };
    let ix = model.add_demo_server("quiet", spec, Status::Connected);
    let snapshot: mcp_core::Snapshot = serde_json::from_value(json!({
        "protocolVersion": LEGACY_VERSION,
        "serverInfo": {"name": "quiet", "version": "1.0"},
        "capabilities": {"resources": {}},
        "resources": [{"uri": "mock://counter", "name": "counter"}],
        "takenAt": "2026-09-15T12:00:00Z"
    }))
    .unwrap();
    model.servers[ix].set_snapshot(Some(snapshot));
    let mut live = open(model, mcp_store::Store::open_in_memory().unwrap());
    live.select(Mode::Resources, "mock://counter");
    live.ui(|window, _| {
        assert!(window.try_find("subscribe").is_some(), "never hidden");
    });
    live.ui(|window, cx| window.click("subscribe", cx));
    live.cx.update(|cx| {
        let features = live.state.read(cx).features();
        let reason = features.get(Feature::Subscriptions).reason();
        assert_eq!(
            reason,
            Some("The server doesn't offer resource subscriptions")
        );
        assert_eq!(coco_mcp::clip::note(cx).as_deref(), reason);
        assert!(
            features
                .get(Feature::LogLevel)
                .reason()
                .is_some_and(|r| r.contains("logging"))
        );
    });
    snap(&mut live.cx, live.handle, "60-disabled-control");
}
